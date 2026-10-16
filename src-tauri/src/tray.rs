use crate::phone::{self, PhoneMsg};
use crate::{events, installer, util, Shared};
use serde_json::json;
use std::sync::Arc;
use tauri::menu::{Menu, MenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Wry};

fn make_menu(app: &AppHandle, shared: &Shared) -> tauri::Result<Menu<Wry>> {
    let t = &shared.t().tray;
    MenuBuilder::new(app)
        .text("test_notify", t.test_notify)
        .text("test_logo", t.test_logo)
        .separator()
        .text("reinstall", t.reinstall)
        .text("notif_access", t.notif_access)
        .text("open_config", t.open_config)
        .text("open_log", t.open_log)
        .text("reload", t.reload)
        .separator()
        .text("codex", if installer::codex_installed() { t.codex_disable } else { t.codex_enable })
        .text("phone_test", t.phone_test)
        .text("remote", if crate::remote::enabled(shared) { crate::remote::s(shared).tray_disable } else { crate::remote::s(shared).tray_enable })
        .separator()
        .text("uninstall", t.uninstall)
        .text("quit", t.quit)
        .build()
}

pub fn build(app: &AppHandle, shared: &Arc<Shared>) -> tauri::Result<()> {
    let menu = make_menu(app, shared)?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let s = shared.clone();
    let tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip(format!("Claude Overlay · 127.0.0.1:{}", shared.port))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| on_menu(app, &s, event.id.as_ref()))
        .build(app)?;
    *shared.tray.lock().unwrap() = Some(tray);
    Ok(())
}

pub fn rebuild_menu(app: &AppHandle, shared: &Shared) {
    if let Ok(menu) = make_menu(app, shared) {
        if let Some(tray) = shared.tray.lock().unwrap().as_ref() {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn on_menu(app: &AppHandle, shared: &Arc<Shared>, id: &str) {
    match id {
        "test_notify" => events::handle_event(app, shared, "test-notify", &json!({})),
        "test_logo" => events::handle_event(app, shared, "test-logo", &json!({})),
        "reinstall" => match installer::install() {
            Ok(_) => {
                shared.reload_config();
                rebuild_menu(app, shared);
                let t = shared.t();
                events::notify(app, shared, "Claude Overlay", t.hooks_installed, "", "done", None);
            }
            Err(e) => {
                let t = shared.t();
                events::notify(app, shared, "Claude Overlay", t.error, &e, "permission", None);
            }
        },
        "notif_access" => util::open_path("ms-settings:privacy-notifications"),
        "open_config" => util::open_path(&util::config_path().to_string_lossy()),
        "open_log" => util::open_path(&util::log_path().to_string_lossy()),
        "reload" => events::handle_event(app, shared, "reload-config", &json!({})),
        "codex" => {
            let t = shared.t();
            if installer::codex_installed() {
                installer::uninstall_codex();
                events::notify(app, shared, "Claude Overlay", t.codex_disabled, "", "info", None);
            } else {
                match installer::install_codex() {
                    Ok(_) => events::notify(app, shared, "Claude Overlay", t.codex_enabled, t.codex_enabled_body, "done", None),
                    Err(e) => events::notify(app, shared, "Claude Overlay", t.error, &e, "permission", None),
                }
            }
            rebuild_menu(app, shared);
        }
        "phone_test" => {
            let t = shared.t();
            if phone::configured(shared) {
                phone::push(shared, PhoneMsg { urgent: false, kind: "test", text: format!("Claude Overlay · {}", t.phone_test_sent), html: None, buttons: vec![], pending_id: None });
                events::notify(app, shared, "Claude Overlay", t.phone_test_sent, "", "done", None);
            } else {
                events::notify(app, shared, "Claude Overlay", t.phone_not_configured, "", "permission", Some(10000));
            }
        }
        "remote" => {
            let rs = crate::remote::s(shared);
            match crate::remote::toggle(app, shared) {
                crate::remote::Toggle::On => events::notify(app, shared, "Claude Overlay", rs.enabled_title, rs.enabled_body, "permission", Some(9000)),
                crate::remote::Toggle::Off => events::notify(app, shared, "Claude Overlay", rs.disabled_title, "", "info", None),
                crate::remote::Toggle::OffNotSaved => events::notify(app, shared, "Claude Overlay", rs.disabled_title, rs.off_not_saved, "permission", Some(10000)),
                crate::remote::Toggle::NeedPairing => events::notify(app, shared, "Claude Overlay", rs.pair_first, "", "permission", Some(10000)),
                crate::remote::Toggle::ConfigUnreadable => events::notify(app, shared, "Claude Overlay", rs.config_unreadable, "", "permission", Some(10000)),
            }
            rebuild_menu(app, shared);
        }
        "uninstall" => {
            installer::uninstall();
            let t = shared.t();
            events::notify(app, shared, "Claude Overlay", t.hooks_removed, "", "info", None);
        }
        "quit" => events::quit(app, shared),
        _ => {}
    }
}
