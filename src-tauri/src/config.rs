use crate::util;
use serde_json::{json, Value};

/// Defaults are compiled in; the user's config.json is deep-merged on top.
pub const DEFAULT_CONFIG: &str = include_str!("../../config.default.json");

pub fn defaults() -> Value {
    serde_json::from_str(DEFAULT_CONFIG).unwrap_or_else(|_| json!({}))
}

pub fn deep_merge(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Object(ao), Value::Object(bo)) => {
            let mut out = ao.clone();
            for (k, v) in bo {
                let merged = match out.get(k) {
                    Some(av) => deep_merge(av, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (_, b) => b.clone(),
    }
}

pub fn ensure_user_data() {
    let _ = std::fs::create_dir_all(util::app_data_dir());
    let p = util::config_path();
    if !p.exists() {
        let _ = std::fs::write(&p, DEFAULT_CONFIG);
    }
}

pub fn load() -> Value {
    let user = util::read_json(&util::config_path()).unwrap_or_else(|| json!({}));
    deep_merge(&defaults(), &user)
}

pub fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |acc, key| acc.get(key))
}

pub fn get_bool(v: &Value, path: &str, default: bool) -> bool {
    get(v, path).and_then(Value::as_bool).unwrap_or(default)
}

pub fn get_u64(v: &Value, path: &str, default: u64) -> u64 {
    match get(v, path) {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(default),
        _ => default,
    }
}

pub fn get_f64(v: &Value, path: &str, default: f64) -> f64 {
    get(v, path).and_then(Value::as_f64).unwrap_or(default)
}

pub fn get_str(v: &Value, path: &str) -> Option<String> {
    get(v, path).and_then(Value::as_str).map(String::from)
}

pub fn get_str_list(v: &Value, path: &str) -> Vec<String> {
    get(v, path)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default()
}

/// logo.style: "headsup" (the HeadsUp mark everywhere, teal accent) or "claude" (Claude's logo
/// on banners from Claude, orange accent). Anything else is the default, "headsup".
pub fn logo_style(v: &Value) -> &'static str {
    match get_str(v, "logo.style").as_deref().map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("claude") => "claude",
        _ => "headsup",
    }
}

/// logo.dismiss: how the limit-reset logo goes away. "anyClick" (a click anywhere on the
/// screen, the default), "logoClick" (a click on the logo) or "auto" (after logo.durationMs).
pub fn logo_dismiss(v: &Value) -> &'static str {
    match get_str(v, "logo.dismiss").as_deref().map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("logoClick") => "logoClick",
        Some(s) if s.eq_ignore_ascii_case("auto") => "auto",
        _ => "anyClick",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_keys_default_and_parse() {
        let d = defaults();
        assert_eq!(logo_style(&d), "headsup");
        assert_eq!(logo_dismiss(&d), "anyClick");
        assert_eq!(get_u64(&d, "logo.durationMs", 0), 9000);
        let c = |style: Value, dismiss: Value| json!({"logo": {"style": style, "dismiss": dismiss}});
        assert_eq!(logo_style(&c(json!("HeadsUp"), json!("auto"))), "headsup");
        assert_eq!(logo_dismiss(&c(json!("HeadsUp"), json!("auto"))), "auto");
        assert_eq!(logo_dismiss(&c(json!("x"), json!("logoclick"))), "logoClick");
        assert_eq!(logo_style(&c(json!("x"), json!(3))), "headsup");
        assert_eq!(logo_dismiss(&c(json!(null), json!("never"))), "anyClick");
    }
}
