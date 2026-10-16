//! Phone delivery: decides whether an event goes to the phone (opt-in, and by default only
//! while the PC has been idle for a while) and fans it out to Telegram and/or ntfy.
use crate::{config, telegram, util, Shared};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub struct PhoneMsg {
    /// Send now, ignoring the idle rule (remote sessions: the user is away by definition).
    pub urgent: bool,
    /// "stop" | "permission" | "question" | "limit" | "test"
    pub kind: &'static str,
    /// Plain text (ntfy, fallback).
    pub text: String,
    /// Telegram HTML version; built with `esc` for user content.
    pub html: Option<String>,
    /// Rows of (label, callback_data); Telegram only.
    pub buttons: Vec<Vec<(String, String)>>,
    /// The interactive card this message belongs to, if any.
    pub pending_id: Option<String>,
}

/// Escape user content for Telegram's HTML parse mode.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// "<b>Agent</b> · <i>project</i>" header line for Telegram messages.
pub fn head(agent: &str, project: &str) -> String {
    if project.is_empty() {
        format!("<b>{}</b>", esc(agent))
    } else {
        format!("<b>{}</b> · <i>{}</i>", esc(agent), esc(project))
    }
}

fn ntfy_send(server: &str, topic: &str, msg: &PhoneMsg) -> Result<(), String> {
    let url = format!("{}/", server.trim_end_matches('/'));
    let mut lines = msg.text.splitn(2, '\n');
    let title = lines.next().unwrap_or("").to_string();
    let body = lines.next().unwrap_or("").to_string();
    // ntfy JSON publishing wants a numeric priority (1 min .. 5 max).
    let priority: u8 = if msg.kind == "permission" || msg.kind == "question" { 4 } else { 3 };
    let tags: Vec<&str> = match msg.kind {
        "permission" => vec!["lock"],
        "question" => vec!["question"],
        "limit" => vec!["orange_circle"],
        _ => vec!["white_check_mark"],
    };
    let payload: Value = json!({
        "topic": topic,
        "title": title,
        "message": if body.is_empty() { title.clone() } else { body },
        "priority": priority,
        "tags": tags,
    });
    ureq::post(&url)
        .timeout(Duration::from_secs(10))
        .send_json(payload)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Send to the phone if phone delivery is on and the event kind is enabled. Unless this is a
/// test, the PC must have been idle for `phone.onlyWhenIdleMs`; if it is not idle yet, the
/// message waits (up to `phone.deferWindowMs`) and goes out the moment the PC becomes idle,
/// as long as the card is still open and no newer event replaced it.
pub fn push(shared: &Arc<Shared>, msg: PhoneMsg) {
    let cfg = shared.cfg();
    if !config::get_bool(&cfg, "phone.enabled", true) {
        return;
    }
    if !configured(shared) {
        if msg.kind == "test" {
            util::log_line(&["phone-error", "not configured"]);
        }
        return;
    }
    if msg.kind == "test" {
        deliver(shared, msg);
        return;
    }
    if msg.urgent {
        // Remote runs: the user is away and the request is denied without an answer, so the
        // per-event switches do not apply.
        deliver(shared, msg);
        return;
    }
    if !config::get_bool(&cfg, &format!("phone.events.{}", msg.kind), true) {
        return;
    }
    let idle_required = config::get_u64(&cfg, "phone.onlyWhenIdleMs", 180000);
    if idle_required == 0 || crate::win::idle_ms() >= idle_required {
        deliver(shared, msg);
        return;
    }
    let window_ms = config::get_u64(&cfg, "phone.deferWindowMs", 900000);
    // An open card that nobody answers on the PC goes to the phone after this, idle or not.
    let unanswered_ms = config::get_u64(&cfg, "phone.sendUnansweredAfterMs", 60000);
    let generation = if msg.pending_id.is_none() {
        Some(shared.phone_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
    } else {
        None
    };
    util::log_line(&["phone-deferred", msg.kind]);
    let shared = shared.clone();
    std::thread::spawn(move || {
        let started = util::now_ms();
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if shared.quitting.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if util::now_ms() - started > window_ms as i64 {
                return;
            }
            if let Some(id) = &msg.pending_id {
                if !shared.pending.lock().unwrap().contains_key(id) {
                    return; // answered on the PC or expired
                }
            } else if let Some(g) = generation {
                if shared.phone_generation.load(std::sync::atomic::Ordering::SeqCst) != g {
                    return; // a newer event replaced this one
                }
            }
            let unanswered = msg.pending_id.is_some()
                && unanswered_ms > 0
                && util::now_ms() - started >= unanswered_ms as i64;
            if unanswered || crate::win::idle_ms() >= idle_required {
                deliver(&shared, msg);
                return;
            }
        }
    });
}

fn deliver(shared: &Arc<Shared>, msg: PhoneMsg) {
    let cfg = shared.cfg();
    let has_telegram = telegram::creds(&cfg).is_some();
    let ntfy_topic = config::get_str(&cfg, "phone.ntfy.topic")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let shared = shared.clone();
    std::thread::spawn(move || {
        if has_telegram {
            let html = msg.html.clone().unwrap_or_else(|| esc(&msg.text));
            match telegram::send(&shared, &html, &msg.buttons) {
                Ok(mid) => {
                    if let Some(id) = &msg.pending_id {
                        shared.tg_card_msgs.lock().unwrap().insert(mid);
                        shared.tg_msgs.lock().unwrap().insert(mid, (id.clone(), html.clone()));
                        if shared.pending.lock().unwrap().contains_key(id) {
                            shared.tg_last_pending_ms.store(util::now_ms(), std::sync::atomic::Ordering::SeqCst);
                            *shared.tg_last_pending.lock().unwrap() = Some(id.clone());
                        } else if shared.tg_msgs.lock().unwrap().remove(&mid).is_some() {
                            // Closed while this message was on its way (and resolved() did not
                            // see it): close the message right away.
                            telegram::edit(&shared, mid, &format!("{html}\n\n{}", shared.t().tg_closed));
                        }
                    }
                    util::log_line(&["telegram-sent", msg.kind]);
                }
                Err(e) => util::log_line(&["telegram-error", &e]),
            }
        }
        if let Some(topic) = ntfy_topic {
            let server = config::get_str(&cfg, "phone.ntfy.server")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://ntfy.sh".to_string());
            match ntfy_send(&server, &topic, &msg) {
                Ok(()) => util::log_line(&["ntfy-sent", msg.kind]),
                Err(e) => util::log_line(&["ntfy-error", &e]),
            }
        }
    });
}

/// A card was answered (on the PC or the phone) or expired: update its Telegram messages.
pub fn resolved(shared: &Arc<Shared>, id: &str, summary: String) {
    let mids: Vec<(i64, String)> = {
        let mut m = shared.tg_msgs.lock().unwrap();
        let ids: Vec<(i64, String)> = m
            .iter()
            .filter(|(_, v)| v.0 == id)
            .map(|(k, v)| (*k, v.1.clone()))
            .collect();
        for (k, _) in &ids {
            m.remove(k);
        }
        ids
    };
    {
        let mut lp = shared.tg_last_pending.lock().unwrap();
        if lp.as_deref() == Some(id) {
            *lp = None;
        }
    }
    if mids.is_empty() {
        return;
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        for (mid, html) in mids {
            telegram::edit(&shared, mid, &format!("{html}\n\n{summary}"));
        }
    });
}

/// True when any phone channel is configured (used by the tray test).
pub fn configured(shared: &Shared) -> bool {
    let cfg = shared.cfg();
    telegram::creds(&cfg).is_some()
        || config::get_str(&cfg, "phone.ntfy.topic").map(|t| !t.trim().is_empty()).unwrap_or(false)
}
