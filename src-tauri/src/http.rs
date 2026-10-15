//! Local HTTP endpoint (127.0.0.1:<port>) the hook scripts talk to, plus a tiny client
//! used by the CLI flags.
use crate::{events, Shared};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tiny_http::{Header, Method, Request, Response, Server};

pub fn respond_json(req: Request, body: String) {
    let header =
        Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap();
    let _ = req.respond(Response::from_string(body).with_header(header));
}

fn respond_status(req: Request, code: u16, body: &str) {
    let _ = req.respond(Response::from_string(body).with_status_code(code));
}

pub fn serve(server: Server, app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let method = req.method().clone();
            if method == Method::Get {
                if url == "/health" {
                    respond_json(req, events::health_json(&shared).to_string());
                } else {
                    respond_status(req, 404, "not found");
                }
                continue;
            }
            if method != Method::Post {
                respond_status(req, 405, "method not allowed");
                continue;
            }
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let value: Value = if body.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => {
                        respond_status(req, 400, "bad json");
                        continue;
                    }
                }
            };
            let mut kind = url.trim_start_matches('/').to_string();
            if let Some(q) = kind.find('?') {
                kind.truncate(q);
            }
            if kind.is_empty() {
                kind = value.get("type").and_then(Value::as_str).unwrap_or("").to_string();
            }
            events::handle_request(&app, &shared, &kind, value, req);
        }
    });
}

/// Minimal HTTP/1.1 POST to the running overlay; returns the response body.
pub fn post(port: u16, kind: &str, body: &str, timeout_ms: u64) -> Result<String, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(1500))
        .map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(timeout_ms)));
    let req = format!(
        "POST /{} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        kind,
        body.len(),
        body
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    Ok(resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}
