//! UI strings. config.language: "auto" (OS locale), "en" or "tr".

pub struct TrayStrings {
    pub test_notify: &'static str,
    pub test_logo: &'static str,
    pub reinstall: &'static str,
    pub notif_access: &'static str,
    pub open_config: &'static str,
    pub open_log: &'static str,
    pub reload: &'static str,
    pub uninstall: &'static str,
    pub quit: &'static str,
}

pub struct Strings {
    pub app_name: &'static str,
    pub code_name: &'static str,
    pub now: &'static str,
    pub task_done: &'static str,
    pub reply_ready: &'static str,
    pub waiting_for_you: &'static str,
    pub needs_input: &'static str,
    pub permission: &'static str,
    pub question: &'static str,
    pub notification: &'static str,
    pub test: &'static str,
    pub test_body: &'static str,
    pub installed: &'static str,
    pub installed_body: &'static str,
    pub install_error: &'static str,
    pub hooks_installed: &'static str,
    pub hooks_removed: &'static str,
    pub error: &'static str,
    pub access_denied: &'static str,
    pub access_denied_body: &'static str,
    pub limit_five_hour: &'static str,
    pub limit_seven_day: &'static str,
    pub limit_spend: &'static str,
    pub limit_generic: &'static str,
    pub limit_resumed: &'static str,
    pub allow: &'static str,
    pub deny: &'static str,
    pub answer_in_app: &'static str,
    pub answered: &'static str,
    pub tray: TrayStrings,
}

pub static EN: Strings = Strings {
    app_name: "Claude",
    code_name: "Claude Code",
    now: "now",
    task_done: "Task finished",
    reply_ready: "Reply ready",
    waiting_for_you: "Waiting for you",
    needs_input: "Needs your input",
    permission: "Asking permission",
    question: "Asking a question",
    notification: "Notification",
    test: "test",
    test_body: "Overlay is running. This is a test notification.",
    installed: "Setup complete",
    installed_body: "Chat, Cowork and Code notifications will now show here.",
    install_error: "Setup error",
    hooks_installed: "Hooks installed",
    hooks_removed: "Hooks removed",
    error: "Error",
    access_denied: "Windows notification access is off",
    access_denied_body: "Settings > Privacy & security > Notifications > turn on \"Let desktop apps access notifications\".",
    limit_five_hour: "5-hour limit reset",
    limit_seven_day: "Weekly limit reset",
    limit_spend: "Spend limit reset",
    limit_generic: "Limit reset",
    limit_resumed: "Limit reset · resuming",
    allow: "Allow",
    deny: "Deny",
    answer_in_app: "Answer in the app",
    answered: "Sent",
    tray: TrayStrings {
        test_notify: "Test notification",
        test_logo: "Test logo",
        reinstall: "Reinstall hooks",
        notif_access: "Windows notification access setting",
        open_config: "Open config file",
        open_log: "Open event log",
        reload: "Reload config",
        uninstall: "Remove hooks",
        quit: "Quit",
    },
};

pub static TR: Strings = Strings {
    app_name: "Claude",
    code_name: "Claude Code",
    now: "şimdi",
    task_done: "Görev tamamlandı",
    reply_ready: "Yanıt hazır",
    waiting_for_you: "Seni bekliyor",
    needs_input: "Girdi bekleniyor",
    permission: "İzin istiyor",
    question: "Soru soruyor",
    notification: "Bildirim",
    test: "test",
    test_body: "Overlay çalışıyor. Bu bir test bildirimi.",
    installed: "Kurulum tamamlandı",
    installed_body: "Chat, Cowork ve Code bildirimleri artık burada görünecek.",
    install_error: "Kurulum hatası",
    hooks_installed: "Hook'lar kuruldu",
    hooks_removed: "Hook'lar kaldırıldı",
    error: "Hata",
    access_denied: "Windows bildirim erişimi kapalı",
    access_denied_body: "Ayarlar > Gizlilik ve güvenlik > Bildirimler > \"Masaüstü uygulamalarının bildirimlere erişmesine izin ver\" açın.",
    limit_five_hour: "5 saatlik limit yenilendi",
    limit_seven_day: "Haftalık limit yenilendi",
    limit_spend: "Harcama limiti yenilendi",
    limit_generic: "Limit yenilendi",
    limit_resumed: "Limit yenilendi · devam ediyor",
    allow: "İzin ver",
    deny: "Reddet",
    answer_in_app: "Uygulamada cevapla",
    answered: "Gönderildi",
    tray: TrayStrings {
        test_notify: "Bildirim testi",
        test_logo: "Logo testi",
        reinstall: "Hook'ları yeniden kur",
        notif_access: "Windows bildirim erişimi ayarı",
        open_config: "Config dosyasını aç",
        open_log: "Olay günlüğünü aç",
        reload: "Config yeniden yükle",
        uninstall: "Hook'ları kaldır",
        quit: "Çıkış",
    },
};

pub fn pick(language: Option<&str>, locale: &str) -> (&'static str, &'static Strings) {
    let lang = match language {
        Some(l) if l != "auto" && !l.is_empty() => l.to_lowercase(),
        _ => locale.chars().take(2).collect::<String>().to_lowercase(),
    };
    if lang == "tr" {
        ("tr", &TR)
    } else {
        ("en", &EN)
    }
}

/// Phrases the Claude app uses as generic "done" toast bodies, in the languages we know.
const GENERIC_WAITING: [&str; 7] = [
    "waiting for your input",
    "needs your input",
    "is waiting",
    "waiting for you",
    "girdinizi bekliyor",
    "girdinize ihtiyacı var",
    "bekliyor",
];

pub fn is_generic_waiting(body: &str) -> bool {
    let b = body.to_lowercase();
    GENERIC_WAITING.iter().any(|p| b.contains(p))
}
