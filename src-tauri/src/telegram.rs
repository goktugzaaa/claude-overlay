//! Telegram bot bridge. The overlay talks to api.telegram.org directly: it sends banners to
//! the user's own bot chat (with inline buttons for permissions and options) and long-polls
//! getUpdates so a tap or a typed reply answers the pending card. No server, no open port.
use crate::{config, events, util, Shared};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;

pub struct Creds {
    pub token: String,
    pub chat_id: String,
}

pub fn creds(cfg: &Value) -> Option<Creds> {
    let token = config::get_str(cfg, "phone.telegram.token")?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    let chat_id = config::get_str(cfg, "phone.telegram.chatId")
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(Creds { token, chat_id })
}

fn api(token: &str, method: &str, body: &Value, timeout_s: u64) -> Result<Value, String> {
    let url = format!("https://api.telegram.org/bot{token}/{method}");
    // Never let the bot token reach the log through error messages.
    let redact = |s: String| s.replace(token, "***");
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(timeout_s))
        .send_json(body.clone());
    let v: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| redact(e.to_string()))?,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            return Err(redact(format!("HTTP {code}: {}", util::truncate(&text, 200))));
        }
        Err(e) => return Err(redact(e.to_string())),
    };
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(util::truncate(&v.to_string(), 200));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// Send a message to the paired chat. `buttons` are rows of (label, callback_data).
/// Returns Telegram's message_id so the message can be edited once the card is answered.
pub fn send(shared: &Shared, text: &str, buttons: &[Vec<(String, String)>]) -> Result<i64, String> {
    let cfg = shared.cfg();
    let c = creds(&cfg).ok_or_else(|| "telegram not configured".to_string())?;
    if c.chat_id.is_empty() {
        return Err("telegram not paired yet: send /start to your bot".to_string());
    }
    let mut body = json!({
        "chat_id": c.chat_id,
        "text": util::truncate(text, 3500),
        "parse_mode": "HTML",
        "disable_web_page_preview": true,
    });
    if !buttons.is_empty() {
        let rows: Vec<Value> = buttons
            .iter()
            .map(|row| {
                Value::Array(
                    row.iter()
                        .map(|(label, data)| json!({"text": label, "callback_data": data}))
                        .collect(),
                )
            })
            .collect();
        body["reply_markup"] = json!({"inline_keyboard": rows});
    }
    let r = api(&c.token, "sendMessage", &body, 15)?;
    r.get("message_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "no message_id in reply".to_string())
}

/// Replace a message's text and drop its buttons (card answered or expired).
pub fn edit(shared: &Shared, message_id: i64, text: &str) {
    let cfg = shared.cfg();
    let Some(c) = creds(&cfg) else { return };
    if c.chat_id.is_empty() {
        return;
    }
    let body = json!({
        "chat_id": c.chat_id,
        "message_id": message_id,
        "text": util::truncate(text, 3500),
        "parse_mode": "HTML",
        "reply_markup": {"inline_keyboard": []},
    });
    if let Err(e) = api(&c.token, "editMessageText", &body, 15) {
        util::log_line(&["telegram-error", &e]);
    }
}

/// Register the bot's command menu (the "/" list in Telegram) for remote sessions.
fn register_commands(shared: &Shared, c: &Creds) {
    if shared.remote.lock().unwrap().commands_registered {
        return;
    }
    let cmds: Vec<Value> = if crate::remote::enabled(shared) {
        crate::remote::commands(shared)
            .into_iter()
            .map(|(command, description)| json!({"command": command, "description": description}))
            .collect()
    } else {
        Vec::new()
    };
    let had_commands = shared.remote.lock().unwrap().commands_set;
    let result = if cmds.is_empty() {
        if !had_commands {
            shared.remote.lock().unwrap().commands_registered = true;
            return;
        }
        api(&c.token, "deleteMyCommands", &json!({}), 10)
    } else {
        api(&c.token, "setMyCommands", &json!({"commands": cmds}), 10)
    };
    match result {
        Ok(_) => {
            let mut st = shared.remote.lock().unwrap();
            st.commands_registered = true;
            st.commands_set = !cmds.is_empty();
        }
        Err(e) => util::log_line(&["telegram-error", &e]),
    }
}

/// Replace a message's text and its buttons (next question of a multi-question card).
pub fn edit_with_buttons(shared: &Shared, message_id: i64, text: &str, buttons: &[Vec<(String, String)>]) -> bool {
    let cfg = shared.cfg();
    let Some(c) = creds(&cfg) else { return false };
    if c.chat_id.is_empty() {
        return false;
    }
    let rows: Vec<Value> = buttons
        .iter()
        .map(|row| Value::Array(row.iter().map(|(label, data)| json!({"text": label, "callback_data": data})).collect()))
        .collect();
    let body = json!({
        "chat_id": c.chat_id,
        "message_id": message_id,
        "text": util::truncate(text, 3500),
        "parse_mode": "HTML",
        "reply_markup": {"inline_keyboard": rows},
    });
    match api(&c.token, "editMessageText", &body, 15) {
        Ok(_) => true,
        Err(e) => {
            util::log_line(&["telegram-error", &e]);
            false
        }
    }
}

/// Ask for a typed answer: Telegram opens the reply box on the user's phone.
fn send_force_reply(shared: &Shared, text: &str) -> Option<i64> {
    let cfg = shared.cfg();
    let c = creds(&cfg)?;
    if c.chat_id.is_empty() {
        return None;
    }
    let body = json!({
        "chat_id": c.chat_id,
        "text": util::truncate(text, 3500),
        "parse_mode": "HTML",
        "reply_markup": {"force_reply": true, "input_field_placeholder": util::truncate(shared.t().type_placeholder, 60)},
    });
    match api(&c.token, "sendMessage", &body, 15) {
        Ok(r) => r.get("message_id").and_then(Value::as_i64),
        Err(e) => {
            util::log_line(&["telegram-error", &e]);
            None
        }
    }
}

/// Answer question `qi` of a question card from the phone. A multi-question card collects
/// answers one by one and moves its Telegram message on to the next question; the card is
/// resolved when every question has an answer. Returns false when the card or question is
/// not current any more.
fn answer_question(app: &AppHandle, shared: &Arc<Shared>, id: &str, qi: usize, answer: String) -> bool {
    let progress = {
        let mut pending = shared.pending.lock().unwrap();
        let Some(p) = pending.get_mut(id) else { return false };
        if p.kind != "question" || qi != p.partial.len() {
            return false;
        }
        let Some(qs) = p.event.pointer("/tool_input/questions").and_then(Value::as_array) else { return false };
        let total = qs.len();
        let Some(key) = qs.get(qi).and_then(|q| q.get("question")).and_then(Value::as_str).map(String::from) else { return false };
        p.partial.insert(key, Value::String(answer));
        if p.partial.len() >= total {
            Err(Value::Object(p.partial.clone()))
        } else {
            Ok((p.event.clone(), p.partial.len()))
        }
    };
    match progress {
        Err(all) => events::finish_interactive(app, shared, id, Some(json!({"answers": all}))),
        Ok((event, next)) => {
            if let Some((_, html, buttons)) = events::phone_question(shared, id, &event, next) {
                let mid = shared.tg_msgs.lock().unwrap().iter().find(|(_, v)| v.0 == id && !v.1.is_empty()).map(|(k, _)| *k);
                let mut shown = None;
                if let Some(mid) = mid {
                    shared.tg_msgs.lock().unwrap().insert(mid, (id.to_string(), html.clone()));
                    if edit_with_buttons(shared, mid, &html, &buttons) {
                        shown = Some(mid);
                    }
                }
                if shown.is_none() {
                    // Could not move the old message on: send the next question as a new one.
                    if let Ok(new_mid) = send(shared, &html, &buttons) {
                        shared.tg_card_msgs.lock().unwrap().insert(new_mid);
                        shared.tg_msgs.lock().unwrap().insert(new_mid, (id.to_string(), html.clone()));
                        shown = Some(new_mid);
                    }
                }
                // Closed (on the PC, or expired) while this was being sent: close it again.
                if let Some(m) = shown {
                    if !shared.pending.lock().unwrap().contains_key(id) {
                        shared.tg_msgs.lock().unwrap().remove(&m);
                        edit(shared, m, &format!("{html}\n\n{}", shared.t().tg_closed));
                    }
                }
            }
        }
    }
    true
}

/// (option labels, index of the question waiting for an answer) of a question card.
fn current_question(shared: &Shared, id: &str) -> Option<(Vec<String>, usize)> {
    let pending = shared.pending.lock().unwrap();
    let p = pending.get(id)?;
    let qi = p.partial.len();
    let q = p.event.pointer("/tool_input/questions")?.as_array()?.get(qi)?;
    let options = q
        .get("options")
        .and_then(Value::as_array)
        .map(|os| os.iter().filter_map(|o| o.get("label").and_then(Value::as_str).map(String::from)).collect())
        .unwrap_or_default();
    Some((options, qi))
}

fn plain(shared: &Shared, c: &Creds, chat: &str, text: &str) {
    let _ = api(&c.token, "sendMessage", &json!({"chat_id": chat, "text": text}), 10);
    let _ = shared;
}

/// Long-poll loop. Re-reads the config every round, so filling in the token (and
/// "Reload config") is enough to turn it on.
pub fn start(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let mut offset: i64 = 0;
        loop {
            if shared.quitting.load(Ordering::SeqCst) {
                return;
            }
            let cfg = shared.cfg();
            let enabled = config::get_bool(&cfg, "phone.enabled", true);
            let Some(c) = creds(&cfg).filter(|_| enabled) else {
                std::thread::sleep(Duration::from_secs(10));
                continue;
            };
            if !c.chat_id.is_empty() {
                register_commands(&shared, &c);
            }
            let body = json!({
                "offset": offset,
                "timeout": 25,
                "allowed_updates": ["message", "callback_query"],
            });
            match api(&c.token, "getUpdates", &body, 40) {
                Ok(Value::Array(updates)) => {
                    for u in updates {
                        if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                            offset = offset.max(id + 1);
                        }
                        handle_update(&app, &shared, &c, &u);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    util::log_line(&["telegram-error", &e]);
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
    });
}

fn pair(shared: &Shared, chat: &str) -> bool {
    let path = util::config_path();
    let mut cfg = match util::read_json(&path) {
        Some(v) if v.is_object() => v,
        // Never overwrite a config.json that exists but does not parse.
        _ if path.exists() => {
            util::log_line(&["telegram-error", "config.json unreadable, pairing not saved"]);
            return false;
        }
        _ => json!({}),
    };
    if !cfg.get("phone").map(Value::is_object).unwrap_or(false) {
        cfg["phone"] = json!({});
    }
    if !cfg["phone"].get("telegram").map(Value::is_object).unwrap_or(false) {
        cfg["phone"]["telegram"] = json!({});
    }
    cfg["phone"]["telegram"]["chatId"] = json!(chat);
    // A newly paired chat never inherits remote control: it has to be switched on again
    // from the tray, on the PC.
    if cfg.get("remote").map(Value::is_object).unwrap_or(false) {
        cfg["remote"]["enabled"] = json!(false);
    }
    if util::write_json(&util::config_path(), &cfg).is_err() {
        return false;
    }
    shared.reload_config();
    util::log_line(&["telegram-paired", chat]);
    true
}

fn handle_update(app: &AppHandle, shared: &Arc<Shared>, c: &Creds, u: &Value) {
    let t = shared.t();
    // Re-read: an earlier update of the same batch may have just paired a chat.
    let fresh = creds(&shared.cfg());
    let c = fresh.as_ref().unwrap_or(c);

    if let Some(cb) = u.get("callback_query") {
        let data = cb.get("data").and_then(Value::as_str).unwrap_or("");
        let cb_id = cb.get("id").and_then(Value::as_str).unwrap_or("");
        let chat = cb.pointer("/message/chat/id").map(|v| v.to_string()).unwrap_or_default();
        if c.chat_id.is_empty() || chat != c.chat_id {
            return;
        }
        if data.starts_with('r') {
            let toast = crate::remote::on_callback(app, shared, data).unwrap_or_default();
            let _ = api(&c.token, "answerCallbackQuery", &json!({"callback_query_id": cb_id, "text": util::truncate(&toast, 180)}), 10);
            return;
        }
        let parts: Vec<&str> = data.split(':').collect();
        let mut done = false;
        match (parts.first().copied(), parts.len()) {
            (Some("p"), 3) => {
                let id = parts[1].to_string();
                if shared.pending.lock().unwrap().contains_key(&id) {
                    let behavior = if parts[2] == "a" { "allow" } else { "deny" };
                    events::finish_interactive(app, shared, &id, Some(json!({"behavior": behavior})));
                    done = true;
                }
            }
            (Some("qo"), 3) => {
                let id = parts[1].to_string();
                let qi: usize = parts[2].parse().unwrap_or(usize::MAX);
                if let Some((_, current)) = current_question(shared, &id) {
                    if current == qi {
                        let question = {
                            let pending = shared.pending.lock().unwrap();
                            pending
                                .get(&id)
                                .and_then(|p| p.event.pointer(&format!("/tool_input/questions/{qi}/question")).and_then(Value::as_str).map(String::from))
                                .unwrap_or_default()
                        };
                        *shared.tg_await_text.lock().unwrap() = Some((id.clone(), qi));
                        let prompt = format!("✍️ <b>{}</b>\n{}", crate::phone::esc(t.type_placeholder), crate::phone::esc(&util::clean_message(&question)));
                        if let Some(mid) = send_force_reply(shared, &prompt) {
                            // A reply to this prompt answers exactly this question.
                            shared.tg_prompt_msgs.lock().unwrap().insert(mid, (id.clone(), qi));
                            shared.tg_card_msgs.lock().unwrap().insert(mid);
                        }
                        util::log_line(&["tg-await-text", &id, &qi.to_string()]);
                        done = true;
                    }
                }
            }
            (Some("q"), 3) | (Some("q"), 4) => {
                let id = parts[1].to_string();
                let (qi, oi) = if parts.len() == 4 {
                    (parts[2].parse().unwrap_or(usize::MAX), parts[3].parse().unwrap_or(usize::MAX))
                } else {
                    (0, parts[2].parse().unwrap_or(usize::MAX))
                };
                if let Some((options, current)) = current_question(shared, &id) {
                    if current == qi {
                        if let Some(label) = options.get(oi) {
                            done = answer_question(app, shared, &id, qi, label.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        let _ = api(
            &c.token,
            "answerCallbackQuery",
            &json!({"callback_query_id": cb_id, "text": if done { t.tg_answered } else { t.tg_no_open }}),
            10,
        );
        return;
    }

    let Some(m) = u.get("message") else { return };
    let chat = m.pointer("/chat/id").map(|v| v.to_string()).unwrap_or_default();
    let text = m.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if chat.is_empty() {
        return;
    }
    if c.chat_id.is_empty() {
        // First chat that talks to the bot becomes the paired one.
        if pair(shared, &chat) {
            plain(shared, c, &chat, t.tg_paired);
            events::notify(app, shared, "Claude Overlay", t.tg_paired, "", "done", None);
        } else {
            let msg = crate::remote::s(shared).config_unreadable;
            plain(shared, c, &chat, msg);
            events::notify(app, shared, "Claude Overlay", msg, "", "permission", None);
        }
        return;
    }
    if chat != c.chat_id || text.is_empty() {
        return;
    }
    if text.starts_with("/start") {
        plain(shared, c, &chat, t.tg_paired);
        return;
    }
    if text.starts_with('/') && crate::remote::on_command(app, shared, &text) {
        return;
    }

    // "Other…" was tapped on a question: this text is its answer.
    let reply_to = m.pointer("/reply_to_message/message_id").and_then(Value::as_i64);
    // A reply to an "Other…" prompt answers the question that prompt asked, if still current.
    let prompt = reply_to.and_then(|mid| shared.tg_prompt_msgs.lock().unwrap().get(&mid).cloned());
    if let Some((id, qi)) = prompt {
        *shared.tg_await_text.lock().unwrap() = None;
        let current = current_question(shared, &id).map(|(_, q)| q);
        if current == Some(qi) {
            util::log_line(&["tg-text", "prompt", &id]);
            if answer_question(app, shared, &id, qi, text.clone()) {
                return;
            }
        }
        plain(shared, c, &chat, t.tg_no_open);
        return;
    }
    // Plain text right after tapping "Other…" answers that question; an explicit reply to
    // some other message cancels the wait and is routed normally.
    let awaiting = shared.tg_await_text.lock().unwrap().take();
    if let (Some((id, qi)), None) = (awaiting, reply_to) {
        let current = current_question(shared, &id).map(|(_, q)| q);
        if current == Some(qi) {
            util::log_line(&["tg-text", "await", &id]);
            if !answer_question(app, shared, &id, qi, text.clone()) {
                plain(shared, c, &chat, t.tg_no_open);
            }
            return;
        }
    }

    // Which open card (if any) does this text answer?
    //  1. A Telegram reply to a card message answers that card.
    //  2. A reply to any other message (e.g. a remote run's result) never answers a card.
    //  3. Plain text answers the newest open card only while no remote session is in use;
    //     otherwise it goes to the remote session, so a "yes" meant for that conversation
    //     can never approve some other session's permission request.
    let reply_mid = m.pointer("/reply_to_message/message_id").and_then(Value::as_i64);
    let replied_card = reply_mid.and_then(|mid| shared.tg_msgs.lock().unwrap().get(&mid).map(|v| v.0.clone()));
    // A reply to a card that is already closed is not a new prompt for anybody.
    if let (Some(mid), None) = (reply_mid, &replied_card) {
        if shared.tg_card_msgs.lock().unwrap().contains(&mid) {
            plain(shared, c, &chat, t.tg_no_open);
            return;
        }
    }
    // Plain text goes to whichever came last: the newest open card, or the remote session.
    let remote_in_use = crate::remote::enabled(shared) && crate::remote::has_target_or_run(shared);
    // While a remote session is in use, plain text only answers a card of that remote run
    // (newer than the last remote interaction). Cards of other sessions are answered with
    // their buttons, "Other…", or a Telegram reply to the card.
    let last_card = shared.tg_last_pending.lock().unwrap().clone();
    let last_card_is_remote = last_card
        .as_ref()
        .and_then(|id| shared.pending.lock().unwrap().get(id).and_then(|p| p.event.get("session_id").and_then(Value::as_str).map(String::from)))
        .map(|sid| crate::remote::is_remote_session(shared, Some(&sid)))
        .unwrap_or(false);
    let card_is_newer = last_card_is_remote
        && shared.tg_last_pending_ms.load(Ordering::SeqCst) > crate::remote::last_touch_ms(shared);
    let target = match (reply_mid, replied_card) {
        (Some(_), Some(card)) => {
            if !shared.pending.lock().unwrap().contains_key(&card) {
                plain(shared, c, &chat, t.tg_no_open);
                return;
            }
            Some(card)
        }
        // A reply to one of the remote run's own messages is a prompt for it (on_text then
        // says so if nothing is selected); it never answers a card.
        (Some(mid), None) if crate::remote::is_remote_msg(shared, mid) => None,
        (Some(_), None) if remote_in_use => {
            plain(shared, c, &chat, t.tg_no_open);
            return;
        }
        (Some(_), None) => shared.tg_last_pending.lock().unwrap().clone(),
        (None, _) if remote_in_use && !card_is_newer => None,
        (None, _) => shared.tg_last_pending.lock().unwrap().clone(),
    }
    .filter(|id| shared.pending.lock().unwrap().contains_key(id));
    util::log_line(&["tg-text", match &target { Some(_) => "card", None if remote_in_use => "remote", None => "none" }]);
    let Some(id) = target else {
        if crate::remote::on_text(app, shared, &text) {
            return;
        }
        plain(shared, c, &chat, t.tg_no_open);
        return;
    };
    let kind = shared.pending.lock().unwrap().get(&id).map(|p| p.kind.clone());
    if kind.as_deref() == Some("question") {
        let qi = current_question(shared, &id).map(|(_, qi)| qi).unwrap_or(usize::MAX);
        if !answer_question(app, shared, &id, qi, text.clone()) {
            plain(shared, c, &chat, t.tg_no_open);
        }
        return;
    }
    let answer = match kind.as_deref() {
        Some("permission") => {
            let l = text.to_lowercase();
            if ["allow", "yes", "y", "ok", "evet", "e", "izin", "izin ver"].contains(&l.as_str()) {
                Some(json!({"behavior": "allow"}))
            } else if ["deny", "no", "n", "hayır", "hayir", "h", "reddet"].contains(&l.as_str()) {
                Some(json!({"behavior": "deny"}))
            } else {
                None
            }
        }
        _ => None,
    };
    match answer {
        Some(a) => events::finish_interactive(app, shared, &id, Some(a)),
        None => plain(shared, c, &chat, t.tg_no_open),
    }
}
