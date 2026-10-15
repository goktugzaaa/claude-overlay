//! Everything that turns an incoming signal into something on screen: hook events,
//! interactive cards (question / permission), mirrored toasts, limit-reset timers and the
//! cursor polling that makes cards clickable.
use crate::phone::{self, PhoneMsg};
use crate::{config, i18n, installer, util, Pending, Shared};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const WINDOW_HEIGHT_LOGICAL: f64 = 640.0;

fn num_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

pub fn window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main")
}

// ---------------------------------------------------------------- window placement

/// Keep the overlay strip at the top of the monitor the cursor is on.
pub fn place_on_cursor_monitor(app: &AppHandle, force: bool) {
    let Some(win) = window(app) else { return };
    let monitors = app.available_monitors().unwrap_or_default();
    let cursor = app.cursor_position().ok();
    let found = cursor.and_then(|c| {
        monitors
            .iter()
            .find(|m| {
                let p = m.position();
                let s = m.size();
                c.x >= p.x as f64
                    && c.x < p.x as f64 + s.width as f64
                    && c.y >= p.y as f64
                    && c.y < p.y as f64 + s.height as f64
            })
            .cloned()
    });
    let mon = found
        .or_else(|| app.primary_monitor().ok().flatten())
        .or_else(|| monitors.first().cloned());
    let Some(mon) = mon else { return };
    let p = *mon.position();
    let s = *mon.size();
    let height = (WINDOW_HEIGHT_LOGICAL * mon.scale_factor()).round() as u32;
    let same_pos = win.outer_position().map(|c| c.x == p.x && c.y == p.y).unwrap_or(false);
    let same_size = win
        .outer_size()
        .map(|c| c.width == s.width && c.height == height)
        .unwrap_or(false);
    if force || !same_pos || !same_size {
        let _ = win.set_position(tauri::PhysicalPosition::new(p.x, p.y));
        let _ = win.set_size(tauri::PhysicalSize::new(s.width, height));
    }
}

pub fn show_overlay(app: &AppHandle, shared: &Shared) {
    place_on_cursor_monitor(app, false);
    if let Some(w) = window(app) {
        // Whoever set TOPMOST last wins, so re-assert it on every event.
        let _ = w.set_always_on_top(true);
        if !shared.typing.load(Ordering::SeqCst) {
            crate::win::ensure_noactivate(&w);
        }
    }
}

// ---------------------------------------------------------------- banners

pub fn emit(app: &AppHandle, shared: &Shared, channel: &str, payload: Value) {
    shared.active_items.fetch_add(1, Ordering::SeqCst);
    show_overlay(app, shared);
    let _ = app.emit(channel, payload);
}

pub fn notify(
    app: &AppHandle,
    shared: &Shared,
    title: &str,
    subtitle: &str,
    body: &str,
    kind: &str,
    duration_ms: Option<u64>,
) {
    let cfg = shared.cfg();
    let t = shared.t();
    let dur = duration_ms.unwrap_or_else(|| config::get_u64(&cfg, "notification.durationMs", 6000));
    let title = if title.is_empty() { t.app_name } else { title };
    emit(
        app,
        shared,
        "notify",
        json!({
            "title": title,
            "subtitle": subtitle,
            "body": body,
            "kind": kind,
            "nowLabel": t.now,
            "durationMs": dur,
            "sound": config::get_bool(&cfg, "sound", true),
        }),
    );
}

pub fn show_logo(app: &AppHandle, shared: &Shared, caption: &str) {
    let cfg = shared.cfg();
    emit(
        app,
        shared,
        "logo",
        json!({
            "caption": caption,
            "durationMs": config::get_u64(&cfg, "logo.durationMs", 9000),
            "sound": config::get_bool(&cfg, "sound", true),
        }),
    );
}

fn with_project(label: &str, cwd: Option<&Value>) -> String {
    let p = util::project_name(cwd);
    if p.is_empty() {
        label.to_string()
    } else {
        format!("{label} · {p}")
    }
}

/// Human-readable one-liner for a tool call (permission requests).
pub fn describe_tool(name: &str, input: Option<&Value>) -> String {
    let obj = input.and_then(Value::as_object);
    let short = |s: &str| util::truncate(&s.split_whitespace().collect::<Vec<_>>().join(" "), 160);
    let field = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| obj.and_then(|o| o.get(*k)).and_then(Value::as_str).map(short))
            .unwrap_or_default()
    };
    match name {
        "Bash" | "PowerShell" => format!("{name}: {}", field(&["command", "description"])),
        "Edit" | "Write" | "Read" | "NotebookEdit" | "MultiEdit" => {
            format!("{name}: {}", field(&["file_path", "notebook_path"]))
        }
        "WebFetch" => format!("WebFetch: {}", field(&["url"])),
        "WebSearch" => format!("WebSearch: {}", field(&["query"])),
        "Agent" | "Task" => format!("{name}: {}", field(&["description", "prompt"])),
        "Glob" | "Grep" => format!("{name}: {}", field(&["pattern"])),
        _ => {
            let name = if name.is_empty() { "tool" } else { name };
            let first_str = obj.and_then(|o| o.values().find_map(|v| v.as_str().map(short)));
            match first_str {
                Some(s) => format!("{name}: {s}"),
                None => match obj {
                    Some(o) if !o.is_empty() => {
                        format!("{name}: {}", util::truncate(&Value::Object(o.clone()).to_string(), 160))
                    }
                    _ => name.to_string(),
                },
            }
        }
    }
}

pub fn health_json(shared: &Shared) -> Value {
    json!({
        "ok": true,
        "pid": std::process::id(),
        "packaged": true,
        "engine": "tauri",
        "lang": *shared.lang_code.lock().unwrap(),
        "listener": shared.listener_status.lock().unwrap().clone(),
        "foreground": shared.foreground.lock().unwrap().clone(),
        "pending": shared.pending.lock().unwrap().len(),
        "windows": shared.state.lock().unwrap().get("windows").cloned().unwrap_or_else(|| json!({})),
        "now": util::now_secs(),
    })
}

// ---------------------------------------------------------------- requests from hooks

pub fn handle_request(
    app: &AppHandle,
    shared: &Arc<Shared>,
    kind: &str,
    body: Value,
    req: tiny_http::Request,
) {
    match kind {
        "question" | "permission" => {
            util::log_line(&[
                kind,
                body.get("tool_name").and_then(Value::as_str).unwrap_or(""),
                &util::project_name(body.get("cwd")),
            ]);
            start_interactive(app, shared, kind, body, req);
        }
        "test-question" => start_interactive(
            app,
            shared,
            "question",
            json!({
                "cwd": "C:/demo/project",
                "tool_name": "AskUserQuestion",
                "tool_input": {"questions": [{
                    "question": "Which database should we use?",
                    "header": "Database",
                    "options": [
                        {"label": "Postgres", "description": "Managed, scalable"},
                        {"label": "SQLite", "description": "Single file, zero setup"},
                        {"label": "MySQL", "description": ""}
                    ]
                }]}
            }),
            req,
        ),
        "test-permission" => start_interactive(
            app,
            shared,
            "permission",
            json!({"cwd": "C:/demo/project", "tool_name": "Bash", "tool_input": {"command": "npm install && npm run build"}}),
            req,
        ),
        _ => {
            handle_event(app, shared, kind, &body);
            crate::http::respond_json(req, "{\"ok\":true}".to_string());
        }
    }
}

pub fn handle_event(app: &AppHandle, shared: &Arc<Shared>, kind: &str, event: &Value) {
    let cfg = shared.cfg();
    let t = shared.t();
    if kind == "status" {
        util::log_line(&["status", &summarize_limits(event.get("status"))]);
    } else {
        util::log_line(&[
            kind,
            event
                .get("notification_type")
                .or_else(|| event.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or(""),
            &util::project_name(event.get("cwd")),
        ]);
    }
    match kind {
        "stop" => {
            if !config::get_bool(&cfg, "events.stop", true)
                || event.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false)
            {
                return;
            }
            shared.last_hook_banner_ms.store(util::now_ms(), Ordering::SeqCst);
            let body = if config::get_bool(&cfg, "notification.showLastMessage", true) {
                util::truncate(
                    &util::clean_message(event.get("last_assistant_message").and_then(Value::as_str).unwrap_or("")),
                    180,
                )
            } else {
                String::new()
            };
            // Adapters for other agents (Codex) send their own title.
            let title = event.get("title").and_then(Value::as_str).unwrap_or(t.code_name);
            let subtitle = with_project(t.task_done, event.get("cwd"));
            notify(app, shared, title, &subtitle, &body, "done", None);
            let project = util::project_name(event.get("cwd"));
            phone::push(shared, PhoneMsg {
                kind: "stop",
                text: format!("✅ {title} · {subtitle}\n{body}"),
                html: Some(format!("✅ {}\n{}\n{}", phone::head(title, &project), phone::esc(t.task_done), phone::esc(&body))),
                buttons: vec![],
                pending_id: None,
            });
        }
        "notification" => {
            let nt = event.get("notification_type").and_then(Value::as_str).unwrap_or("");
            if nt == "quota_auto_resume_fired" {
                if config::get_bool(&cfg, "events.quota_auto_resume_fired", true) {
                    show_logo(app, shared, t.limit_resumed);
                }
                return;
            }
            if !nt.is_empty() && !config::get_bool(&cfg, &format!("events.{nt}"), false) {
                return;
            }
            if nt == "permission_prompt"
                && util::now_ms() - shared.last_permission_ms.load(Ordering::SeqCst) < 8000
            {
                return; // PermissionRequest already showed the richer card
            }
            shared.last_hook_banner_ms.store(util::now_ms(), Ordering::SeqCst);
            let label = match nt {
                "permission_prompt" => t.permission,
                "idle_prompt" => t.waiting_for_you,
                "agent_needs_input" => t.needs_input,
                _ => t.notification,
            };
            let title = event.get("title").and_then(Value::as_str).unwrap_or(t.code_name);
            let body = util::truncate(
                &util::clean_message(event.get("message").and_then(Value::as_str).unwrap_or("")),
                160,
            );
            let kind = if nt == "permission_prompt" { "permission" } else { "info" };
            notify(app, shared, title, &with_project(label, event.get("cwd")), &body, kind, None);
        }
        "status" => handle_status(app, shared, event.get("status").unwrap_or(event)),
        "test-notify" => notify(
            app,
            shared,
            t.app_name,
            &format!("{} · {}", t.task_done, t.test),
            t.test_body,
            "done",
            None,
        ),
        "test-logo" => show_logo(app, shared, t.limit_five_hour),
        "reload-config" => {
            shared.reload_config();
            crate::tray::rebuild_menu(app, shared);
            let _ = app.emit("config", shared.cfg());
        }
        "quit" => quit(app, shared),
        _ => {}
    }
}

// ---------------------------------------------------------------- interactive cards

fn start_interactive(
    app: &AppHandle,
    shared: &Arc<Shared>,
    kind: &str,
    event: Value,
    req: tiny_http::Request,
) {
    let cfg = shared.cfg();
    let t = shared.t();
    let enabled_key = if kind == "question" { "events.question" } else { "events.permission_request" };
    if !config::get_bool(&cfg, enabled_key, true) {
        crate::http::respond_json(req, "{}".to_string());
        return;
    }
    let skip = config::get_str_list(&cfg, "interactive.skipWhenFocused");
    let fg = shared.foreground.lock().unwrap().clone();
    if !config::get_bool(&cfg, "interactive.enabled", true)
        || skip.iter().any(|s| s.eq_ignore_ascii_case(&fg))
    {
        // The Claude app / a terminal is in front: its own dialog is visible, answer there.
        util::log_line(&[&format!("{kind}-skip"), if fg.is_empty() { "disabled" } else { &fg }]);
        shared.last_hook_banner_ms.store(util::now_ms(), Ordering::SeqCst);
        if kind == "permission" {
            shared.last_permission_ms.store(util::now_ms(), Ordering::SeqCst);
        }
        crate::http::respond_json(req, "{}".to_string());
        return;
    }

    let mut wait_ms = config::get_u64(&cfg, "interactive.waitMs", 20000).max(3000);
    // With a phone channel configured the card stays open long enough to be answered from
    // the phone; the PC card can still be answered at any time.
    if phone::configured(shared) {
        wait_ms = wait_ms.max(config::get_u64(&cfg, "phone.holdMs", 480000));
    }
    let id = (shared.seq.fetch_add(1, Ordering::SeqCst) + 1).to_string();
    shared.pending.lock().unwrap().insert(
        id.clone(),
        Pending { req, kind: kind.to_string(), event: event.clone(), deadline_ms: util::now_ms() + wait_ms as i64 },
    );
    shared.last_hook_banner_ms.store(util::now_ms(), Ordering::SeqCst);
    if kind == "permission" {
        shared.last_permission_ms.store(util::now_ms(), Ordering::SeqCst);
    }
    {
        let app = app.clone();
        let shared = shared.clone();
        let id = id.clone();
        std::thread::spawn(move || loop {
            let remaining = shared.pending.lock().unwrap().get(&id).map(|p| p.deadline_ms - util::now_ms());
            match remaining {
                None => return,
                Some(r) if r <= 0 => {
                    finish_interactive(&app, &shared, &id, None);
                    return;
                }
                Some(r) => std::thread::sleep(Duration::from_millis(r.clamp(10, 1000) as u64)),
            }
        });
    }

    let mut payload = json!({
        "id": id,
        "kind": kind,
        "waitMs": wait_ms,
        "nowLabel": t.now,
        "sound": config::get_bool(&cfg, "sound", true),
        "labels": {"allow": t.allow, "deny": t.deny, "answerInApp": t.answer_in_app, "answered": t.answered, "ok": "OK",
                   "other": t.other, "send": t.send, "typePlaceholder": t.type_placeholder},
        "title": t.code_name,
    });
    if kind == "question" {
        let questions: Vec<Value> = event
            .pointer("/tool_input/questions")
            .and_then(Value::as_array)
            .map(|qs| {
                qs.iter()
                    .filter_map(|q| {
                        let question = util::clean_message(q.get("question").and_then(Value::as_str).unwrap_or(""));
                        if question.is_empty() {
                            return None;
                        }
                        let options: Vec<Value> = q
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|os| {
                                os.iter()
                                    .filter_map(|o| {
                                        let label = o.get("label").and_then(Value::as_str).unwrap_or("");
                                        if label.is_empty() {
                                            return None;
                                        }
                                        Some(json!({
                                            "label": label,
                                            "description": o.get("description").and_then(Value::as_str).unwrap_or(""),
                                        }))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        Some(json!({
                            "question": question,
                            "header": q.get("header").and_then(Value::as_str).unwrap_or(""),
                            "multiSelect": q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
                            "options": options,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if questions.is_empty() {
            finish_interactive(app, shared, &id, None);
            return;
        }
        let subtitle = with_project(t.question, event.get("cwd"));
        let project = util::project_name(event.get("cwd"));
        let mut text = format!("❓ {} · {subtitle}\n", t.code_name);
        let mut html = format!("❓ {} · {}\n", phone::head(t.code_name, &project), phone::esc(t.question));
        for q in &questions {
            let qt = q.get("question").and_then(Value::as_str).unwrap_or("");
            let header = q.get("header").and_then(Value::as_str).unwrap_or("");
            text.push_str(qt);
            text.push('\n');
            if !header.is_empty() {
                html.push_str(&format!("<b>{}</b>\n", phone::esc(header)));
            }
            html.push_str(&format!("{}\n", phone::esc(qt)));
            if let Some(os) = q.get("options").and_then(Value::as_array) {
                for (i, o) in os.iter().enumerate() {
                    let label = o.get("label").and_then(Value::as_str).unwrap_or("");
                    let desc = o.get("description").and_then(Value::as_str).unwrap_or("");
                    let line = if desc.is_empty() { format!("{}. {}", i + 1, label) } else { format!("{}. {} — {}", i + 1, label, desc) };
                    text.push_str(&line);
                    text.push('\n');
                    html.push_str(&format!("{}\n", phone::esc(&line)));
                }
            }
            html.push('\n');
        }
        text.push_str(t.tg_hint_reply);
        html.push_str(&format!("<i>{}</i>", phone::esc(t.tg_hint_reply)));
        let buttons: Vec<Vec<(String, String)>> = questions[0]
            .get("options")
            .and_then(Value::as_array)
            .map(|os| {
                os.iter()
                    .enumerate()
                    .filter_map(|(i, o)| {
                        o.get("label")
                            .and_then(Value::as_str)
                            .map(|l| vec![(l.to_string(), format!("q:{id}:{i}"))])
                    })
                    .collect()
            })
            .unwrap_or_default();
        payload["subtitle"] = json!(subtitle);
        payload["questions"] = Value::Array(questions);
        emit(app, shared, "interactive", payload);
        phone::push(shared, PhoneMsg { kind: "question", text, html: Some(html), buttons, pending_id: Some(id.clone()) });
    } else {
        let subtitle = with_project(t.permission, event.get("cwd"));
        let body_text = describe_tool(
            event.get("tool_name").and_then(Value::as_str).unwrap_or(""),
            event.get("tool_input"),
        );
        payload["subtitle"] = json!(subtitle);
        payload["body"] = json!(body_text);
        emit(app, shared, "interactive", payload);
        let project = util::project_name(event.get("cwd"));
        let text = format!("🔐 {} · {subtitle}\n{body_text}", t.code_name);
        let html = format!("🔐 {}\n{}\n<code>{}</code>", phone::head(t.code_name, &project), phone::esc(t.permission), phone::esc(&body_text));
        let buttons = vec![vec![
            (t.allow.to_string(), format!("p:{id}:a")),
            (t.deny.to_string(), format!("p:{id}:d")),
        ]];
        phone::push(shared, PhoneMsg { kind: "permission", text, html: Some(html), buttons, pending_id: Some(id.clone()) });
    }
}

/// Resolve a pending card: `data` is the click ({answers} or {behavior}); None = timeout.
pub fn finish_interactive(app: &AppHandle, shared: &Arc<Shared>, id: &str, data: Option<Value>) {
    let pending = shared.pending.lock().unwrap().remove(id);
    let Some(p) = pending else { return };
    let mut out = json!({});
    if let Some(d) = &data {
        if p.kind == "question" {
            if let Some(answers) = d.get("answers").filter(|a| a.is_object()) {
                let mut updated = p.event.get("tool_input").cloned().unwrap_or_else(|| json!({}));
                if !updated.is_object() {
                    updated = json!({});
                }
                updated["answers"] = answers.clone();
                out = json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "updatedInput": updated,
                }});
            }
        } else if let Some(b) = d.get("behavior").and_then(Value::as_str) {
            if b == "allow" {
                out = json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}});
            } else if b == "deny" {
                out = json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": "Denied from Claude Overlay"}}});
            }
        }
    }
    let answered = out.as_object().map(|o| !o.is_empty()).unwrap_or(false);
    util::log_line(&[
        &format!("{}-answer", p.kind),
        &data.as_ref().map(|d| util::truncate(&d.to_string(), 200)).unwrap_or_else(|| "timeout".to_string()),
    ]);
    crate::http::respond_json(p.req, out.to_string());
    let _ = app.emit("resolve", json!({"id": id, "answered": answered}));
    util::log_line(&["resolve-sent", id]);
    let t = shared.t();
    let summary = match (p.kind.as_str(), data.as_ref().filter(|_| answered)) {
        ("permission", Some(d)) => {
            if d.get("behavior").and_then(Value::as_str) == Some("allow") {
                format!("✓ {}", t.allow)
            } else {
                format!("✗ {}", t.deny)
            }
        }
        ("question", Some(d)) => d
            .get("answers")
            .and_then(Value::as_object)
            .and_then(|m| m.values().next())
            .and_then(Value::as_str)
            .map(|a| format!("✓ {}", phone::esc(a)))
            .unwrap_or_else(|| "✓".to_string()),
        _ => t.tg_expired.to_string(),
    };
    phone::resolved(shared, id, summary);
}

// ---------------------------------------------------------------- limits

pub fn load_state() -> Value {
    let mut st = util::read_json(&util::state_path()).unwrap_or_else(|| json!({}));
    if !st.is_object() {
        st = json!({});
    }
    if !st.get("windows").map(Value::is_object).unwrap_or(false) {
        st["windows"] = json!({});
    }
    st
}

fn save_state(st: &Value) {
    let _ = util::write_json(&util::state_path(), st);
}

fn window_label(shared: &Shared, name: &str) -> &'static str {
    let t = shared.t();
    match name {
        "five_hour" => t.limit_five_hour,
        "seven_day" => t.limit_seven_day,
        "spend_limit" => t.limit_spend,
        _ => t.limit_generic,
    }
}

fn handle_status(app: &AppHandle, shared: &Arc<Shared>, status: &Value) {
    let Some(rl) = status.get("rate_limits").and_then(Value::as_object) else { return };
    let mut expired: Vec<(String, i64)> = Vec::new();
    {
        let mut st = shared.state.lock().unwrap();
        if !st.get("windows").map(Value::is_object).unwrap_or(false) {
            st["windows"] = json!({});
        }
        let windows = st["windows"].as_object_mut().unwrap();
        for (name, info) in rl {
            let Some(incoming) = info.get("resets_at").and_then(num_i64) else { continue };
            let w = windows.entry(name.clone()).or_insert_with(|| json!({}));
            if !w.is_object() {
                *w = json!({});
            }
            let prev = w.get("resetsAt").and_then(num_i64);
            let fired = w.get("firedFor").and_then(num_i64);
            if let Some(p) = prev {
                // A new window started while we were not watching: fire for the old one.
                if incoming > p && fired != Some(p) && p * 1000 <= util::now_ms() {
                    expired.push((name.clone(), p));
                }
            }
            if prev != Some(incoming) {
                w["usedPercentage"] = json!(0);
            }
            w["resetsAt"] = json!(incoming);
            if let Some(used) = info.get("used_percentage").and_then(Value::as_f64) {
                let cur = w.get("usedPercentage").and_then(Value::as_f64).unwrap_or(0.0);
                w["usedPercentage"] = json!(used.max(cur));
            }
        }
        save_state(&st);
    }
    for (name, at) in expired {
        fire_reset(app, shared, &name, at);
    }
}

fn fire_reset(app: &AppHandle, shared: &Arc<Shared>, name: &str, resets_at: i64) {
    let used = {
        let mut st = shared.state.lock().unwrap();
        let used = {
            let Some(w) = st.get_mut("windows").and_then(|w| w.get_mut(name)) else { return };
            if w.get("firedFor").and_then(num_i64) == Some(resets_at) {
                return;
            }
            w["firedFor"] = json!(resets_at);
            w.get("usedPercentage").and_then(Value::as_f64).unwrap_or(0.0)
        };
        save_state(&st);
        used
    };
    // A reset that happened hours ago (PC was off) is not news any more.
    if util::now_ms() - resets_at * 1000 > 3 * 3600 * 1000 {
        return;
    }
    let cfg = shared.cfg();
    if !config::get_bool(&cfg, &format!("limits.{name}"), true) {
        return;
    }
    if used < config::get_f64(&cfg, "limits.minUsedPercentage", 0.0) {
        return;
    }
    util::log_line(&["reset", name, &resets_at.to_string(), &format!("used={used}")]);
    show_logo(app, shared, window_label(shared, name));
    phone::push(shared, PhoneMsg { kind: "limit", text: format!("🟠 {}", window_label(shared, name)), html: Some(format!("🟠 <b>{}</b>", phone::esc(window_label(shared, name)))), buttons: vec![], pending_id: None });
}

pub fn start_limit_timer(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if shared.quitting.load(Ordering::SeqCst) {
            return;
        }
        let due: Vec<(String, i64)> = {
            let st = shared.state.lock().unwrap();
            st.get("windows")
                .and_then(Value::as_object)
                .map(|ws| {
                    ws.iter()
                        .filter_map(|(n, w)| {
                            let at = w.get("resetsAt").and_then(num_i64)?;
                            let fired = w.get("firedFor").and_then(num_i64);
                            if fired != Some(at) && at * 1000 <= util::now_ms() {
                                Some((n.clone(), at))
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for (n, at) in due {
            fire_reset(&app, &shared, &n, at);
        }
    });
}

fn summarize_limits(status: Option<&Value>) -> String {
    let Some(rl) = status.and_then(|s| s.get("rate_limits")).and_then(Value::as_object) else {
        return "no-rate-limits".to_string();
    };
    rl.iter()
        .map(|(k, v)| {
            format!(
                "{k}={}%@{}",
                v.get("used_percentage").map(|x| x.to_string()).unwrap_or_default(),
                v.get("resets_at").map(|x| x.to_string()).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- toast listener bridge

pub fn start_toast_listener(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || loop {
        if shared.quitting.load(Ordering::SeqCst) {
            return;
        }
        let cfg = shared.cfg();
        if !config::get_bool(&cfg, "toasts.enabled", true) {
            *shared.listener_status.lock().unwrap() = "disabled".to_string();
            return;
        }
        let script = util::hooks_dir().join("claude-overlay-toast-listener.ps1");
        if !script.exists() {
            *shared.listener_status.lock().unwrap() = "missing".to_string();
            return;
        }
        let mut child = match util::powershell()
            .arg("-File")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                util::log_line(&["listener-error", &e.to_string()]);
                *shared.listener_status.lock().unwrap() = "error".to_string();
                return;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *shared.listener_child.lock().unwrap() = Some(child);
        if let Some(err) = stderr {
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let l = line.trim();
                    if !l.is_empty() {
                        util::log_line(&["listener-stderr", &util::truncate(l, 300)]);
                    }
                }
            });
        }
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let l = line.trim();
                if !l.is_empty() {
                    handle_listener_line(&app, &shared, l);
                }
            }
        }
        let code = {
            let mut guard = shared.listener_child.lock().unwrap();
            let c = guard.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
            *guard = None;
            c
        };
        util::log_line(&["listener-exit", &code.map(|c| c.to_string()).unwrap_or_else(|| "null".to_string())]);
        if shared.quitting.load(Ordering::SeqCst) || *shared.listener_status.lock().unwrap() != "allowed" {
            return;
        }
        std::thread::sleep(Duration::from_secs(5)); // crashed: restart
    });
}

fn handle_listener_line(app: &AppHandle, shared: &Arc<Shared>, line: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else { return };
    match msg.get("event").and_then(Value::as_str).unwrap_or("") {
        "foreground" => {
            *shared.foreground.lock().unwrap() =
                msg.get("process").and_then(Value::as_str).unwrap_or("").to_string();
        }
        "access" => {
            let status = msg.get("status").and_then(Value::as_str).unwrap_or("");
            let allowed = status == "Allowed";
            *shared.listener_status.lock().unwrap() =
                if allowed { "allowed".to_string() } else { "denied".to_string() };
            util::log_line(&["listener-access", status]);
            if !allowed {
                let t = shared.t();
                notify(app, shared, "Claude Overlay", t.access_denied, t.access_denied_body, "permission", Some(12000));
            }
        }
        "error" => util::log_line(&[
            "listener-error",
            &util::truncate(msg.get("message").and_then(Value::as_str).unwrap_or(""), 300),
        ]),
        "toast" => handle_toast(app, shared, &msg),
        _ => {}
    }
}

fn handle_toast(app: &AppHandle, shared: &Arc<Shared>, msg: &Value) {
    let cfg = shared.cfg();
    let mut watch = config::get_str_list(&cfg, "toasts.watchApps");
    if watch.is_empty() {
        watch.push("Claude".to_string());
    }
    let app_name = msg.get("app").and_then(Value::as_str).unwrap_or("").to_string();
    let aumid = msg.get("aumid").and_then(Value::as_str).unwrap_or("");
    let hay = format!("{app_name} {aumid}").to_lowercase();
    if !watch.iter().any(|w| hay.contains(&w.to_lowercase())) {
        util::log_line(&["toast-skip", &app_name]);
        return;
    }
    let texts: Vec<String> = msg
        .get("texts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    util::log_line(&["toast", &app_name, &util::truncate(&texts.join(" | "), 200)]);

    let dedupe = config::get_u64(&cfg, "toasts.dedupeWindowMs", 6000) as i64;
    if util::now_ms() - shared.last_hook_banner_ms.load(Ordering::SeqCst) < dedupe {
        return; // a hook already showed richer info
    }
    // Hold briefly: when a Claude Code hook for the same moment arrives, it wins (it carries
    // the actual message / command). Chat and Cowork have no hooks, so their toast shows.
    let arrived = util::now_ms();
    let hold = config::get_u64(&cfg, "toasts.holdMs", 1500);
    let app = app.clone();
    let shared = shared.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(hold));
        if shared.last_hook_banner_ms.load(Ordering::SeqCst) >= arrived - 500 {
            return;
        }
        show_toast_banner(&app, &shared, &app_name, &texts);
    });
}

fn show_toast_banner(app: &AppHandle, shared: &Arc<Shared>, app_name: &str, texts: &[String]) {
    let t = shared.t();
    let head = texts.first().cloned().unwrap_or_default();
    let mut body = texts.iter().skip(1).cloned().collect::<Vec<_>>().join(" · ");
    let probe = format!("{head} {body}").to_lowercase();
    let (kind, label) = if probe.contains("izin") || probe.contains("permission") {
        ("permission", t.permission)
    } else if i18n::is_generic_waiting(&body) {
        body.clear();
        ("done", t.reply_ready)
    } else if body.contains('?') {
        ("question", t.question)
    } else if probe.contains("tamamland")
        || probe.contains("complete")
        || probe.contains("finished")
        || probe.contains("done")
    {
        ("done", t.task_done)
    } else {
        ("done", t.reply_ready)
    };
    let subtitle = if head.is_empty() { label.to_string() } else { format!("{label} · {head}") };
    let title = if app_name.is_empty() { t.app_name } else { app_name };
    notify(app, shared, title, &subtitle, &body, kind, None);
    phone::push(shared, PhoneMsg { kind: "stop", text: format!("💬 {title} · {subtitle}\n{body}"), html: Some(format!("💬 <b>{}</b> · {}\n{}", phone::esc(title), phone::esc(&subtitle), phone::esc(&body))), buttons: vec![], pending_id: None });
}

// ---------------------------------------------------------------- cursor polling

fn set_capture(app: &AppHandle, shared: &Shared, flag: bool) {
    if shared.capturing.swap(flag, Ordering::SeqCst) == flag {
        return;
    }
    util::log_line(&["mouse", if flag { "capture" } else { "passthrough" }]);
    if let Some(w) = window(app) {
        let _ = w.set_ignore_cursor_events(!flag);
        if !shared.typing.load(Ordering::SeqCst) {
            crate::win::ensure_noactivate(&w);
        }
    }
}

/// While interactive cards are on screen, poll the cursor and lift the click-through only
/// over the cards. No OS-level mouse hook involved.
pub fn start_cursor_poll(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(40));
        if shared.quitting.load(Ordering::SeqCst) {
            return;
        }
        let rects = shared.hot_rects.lock().unwrap().clone();
        if rects.is_empty() {
            set_capture(&app, &shared, false);
            continue;
        }
        let Some(win) = window(&app) else { continue };
        if !shared.typing.load(Ordering::SeqCst) {
            crate::win::ensure_noactivate(&win);
        }
        let (Ok(cur), Ok(pos), Ok(scale)) = (app.cursor_position(), win.outer_position(), win.scale_factor()) else {
            continue;
        };
        let x = (cur.x - pos.x as f64) / scale;
        let y = (cur.y - pos.y as f64) / scale;
        let inside = rects
            .iter()
            .any(|r| x >= r.left - 4.0 && x <= r.right + 4.0 && y >= r.top - 4.0 && y <= r.bottom + 4.0);
        set_capture(&app, &shared, inside);
    });
}

// ---------------------------------------------------------------- lifecycle

pub fn quit(app: &AppHandle, shared: &Arc<Shared>) {
    shared.quitting.store(true, Ordering::SeqCst);
    let ids: Vec<String> = shared.pending.lock().unwrap().keys().cloned().collect();
    for id in ids {
        finish_interactive(app, shared, &id, None);
    }
    if let Some(mut c) = shared.listener_child.lock().unwrap().take() {
        let _ = c.kill();
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        app.exit(0);
    });
}

/// First start (or the exe moved, or the hook set changed): wire everything up.
pub fn first_run(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        if std::env::var_os("CLAUDE_OVERLAY_NO_INSTALL").is_some() || installer::is_installed() {
            return;
        }
        match installer::install() {
            Ok(lines) => {
                shared.reload_config();
                let mut fields: Vec<&str> = vec!["install"];
                fields.extend(lines.iter().map(String::as_str));
                util::log_line(&fields);
                let t = shared.t();
                notify(&app, &shared, "Claude Overlay", t.installed, t.installed_body, "done", None);
            }
            Err(e) => {
                let t = shared.t();
                notify(&app, &shared, "Claude Overlay", t.install_error, &e, "permission", None);
            }
        }
    });
}
