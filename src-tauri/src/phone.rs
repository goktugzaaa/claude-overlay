//! Phone delivery: decides whether an event goes to the phone (opt-in, and by default only
//! while the PC has been idle for a while) and fans it out to Telegram and/or ntfy.
use crate::{config, telegram, util, Shared};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub struct PhoneMsg {
    /// "stop" | "permission" | "question" | "limit" | "test"
    pub kind: &'static str,
    pub text: String,
    /// Rows of (label, callback_data); Telegram only.
    pub buttons: Vec<Vec<(String, String)>>,
    /// The interactive card this message belongs to, if any.
    pub pending_id: Option<String>,
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

/// Send to the phone if phone delivery is on, the event kind is enabled and (unless this is a
/// test) the PC has been idle long enough.
pub fn push(shared: &Arc<Shared>, msg: PhoneMsg) {
    let cfg = shared.cfg();
    if !config::get_bool(&cfg, "phone.enabled", true) {
        return;
    }
    if msg.kind != "test" {
        if !config::get_bool(&cfg, &format!("phone.events.{}", msg.kind), true) {
            return;
        }
        let idle_required = config::get_u64(&cfg, "phone.onlyWhenIdleMs", 180000);
        if idle_required > 0 && crate::win::idle_ms() < idle_required {
            return;
        }
    }
    let has_telegram = telegram::creds(&cfg).is_some();
    let ntfy_topic = config::get_str(&cfg, "phone.ntfy.topic")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if !has_telegram && ntfy_topic.is_none() {
        if msg.kind == "test" {
            util::log_line(&["phone-error", "not configured"]);
        }
        return;
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        if has_telegram {
            match telegram::send(&shared, &msg.text, &msg.buttons) {
                Ok(mid) => {
                    if let Some(id) = &msg.pending_id {
                        shared.tg_msgs.lock().unwrap().insert(mid, id.clone());
                        *shared.tg_last_pending.lock().unwrap() = Some(id.clone());
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
    let mids: Vec<i64> = {
        let mut m = shared.tg_msgs.lock().unwrap();
        let ids: Vec<i64> = m.iter().filter(|(_, v)| v.as_str() == id).map(|(k, _)| *k).collect();
        for k in &ids {
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
        for mid in mids {
            telegram::edit(&shared, mid, &summary);
        }
    });
}

/// True when any phone channel is configured (used by the tray test).
pub fn configured(shared: &Shared) -> bool {
    let cfg = shared.cfg();
    telegram::creds(&cfg).is_some()
        || config::get_str(&cfg, "phone.ntfy.topic").map(|t| !t.trim().is_empty()).unwrap_or(false)
}
