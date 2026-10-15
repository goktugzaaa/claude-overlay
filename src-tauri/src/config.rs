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
