//! Telegram bot bridge. The overlay talks to api.telegram.org directly: it sends banners to
//! the user's own bot chat (with inline buttons for permissions and options) and long-polls
//! getUpdates so a tap or a typed reply answers the pending card. No server, no open port.
use crate::{config, events, util, Shared};
use serde_json::{json, Map, Value};
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

/// (cleaned question text, option label) for the first question of a pending card.
fn first_question(shared: &Shared, id: &str) -> Option<(String, Vec<String>)> {
    let pending = shared.pending.lock().unwrap();
    let p = pending.get(id)?;
    let q = p.event.pointer("/tool_input/questions/0")?;
    let text = util::clean_message(q.get("question").and_then(Value::as_str).unwrap_or(""));
    if text.is_empty() {
        return None;
    }
    let options = q
        .get("options")
        .and_then(Value::as_array)
        .map(|os| {
            os.iter()
                .filter_map(|o| o.get("label").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some((text, options))
}

fn answers_json(question: String, answer: String) -> Value {
    let mut m = Map::new();
    m.insert(question, Value::String(answer));
    json!({"answers": Value::Object(m)})
}

fn pair(shared: &Shared, chat: &str) {
    let mut cfg = util::read_json(&util::config_path()).unwrap_or_else(|| json!({}));
    if !cfg.is_object() {
        cfg = json!({});
    }
    if !cfg.get("phone").map(Value::is_object).unwrap_or(false) {
        cfg["phone"] = json!({});
    }
    if !cfg["phone"].get("telegram").map(Value::is_object).unwrap_or(false) {
        cfg["phone"]["telegram"] = json!({});
    }
    cfg["phone"]["telegram"]["chatId"] = json!(chat);
    let _ = util::write_json(&util::config_path(), &cfg);
    shared.reload_config();
    util::log_line(&["telegram-paired", chat]);
}

fn handle_update(app: &AppHandle, shared: &Arc<Shared>, c: &Creds, u: &Value) {
    let t = shared.t();

    if let Some(cb) = u.get("callback_query") {
        let data = cb.get("data").and_then(Value::as_str).unwrap_or("");
        let cb_id = cb.get("id").and_then(Value::as_str).unwrap_or("");
        let chat = cb.pointer("/message/chat/id").map(|v| v.to_string()).unwrap_or_default();
        if c.chat_id.is_empty() || chat != c.chat_id {
            return;
        }
        let parts: Vec<&str> = data.split(':').collect();
        let mut done = false;
        if parts.len() == 3 {
            let id = parts[1].to_string();
            let answer = match parts[0] {
                "p" => Some(json!({"behavior": if parts[2] == "a" { "allow" } else { "deny" }})),
                "q" => {
                    let idx: usize = parts[2].parse().unwrap_or(usize::MAX);
                    first_question(shared, &id).and_then(|(q, options)| {
                        options.get(idx).map(|label| answers_json(q, label.clone()))
                    })
                }
                _ => None,
            };
            if let Some(a) = answer {
                if shared.pending.lock().unwrap().contains_key(&id) {
                    events::finish_interactive(app, shared, &id, Some(a));
                    done = true;
                }
            }
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
        pair(shared, &chat);
        plain(shared, c, &chat, t.tg_paired);
        events::notify(app, shared, "Claude Overlay", t.tg_paired, "", "done", None);
        return;
    }
    if chat != c.chat_id || text.is_empty() {
        return;
    }
    if text.starts_with("/start") {
        plain(shared, c, &chat, t.tg_paired);
        return;
    }

    // A typed reply answers the card it replies to, or else the newest open card.
    let target = m
        .pointer("/reply_to_message/message_id")
        .and_then(Value::as_i64)
        .and_then(|mid| shared.tg_msgs.lock().unwrap().get(&mid).map(|v| v.0.clone()))
        .or_else(|| shared.tg_last_pending.lock().unwrap().clone())
        .filter(|id| shared.pending.lock().unwrap().contains_key(id));
    let Some(id) = target else {
        plain(shared, c, &chat, t.tg_no_open);
        return;
    };
    let kind = shared.pending.lock().unwrap().get(&id).map(|p| p.kind.clone());
    let answer = match kind.as_deref() {
        Some("question") => first_question(shared, &id).map(|(q, _)| answers_json(q, text.clone())),
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
