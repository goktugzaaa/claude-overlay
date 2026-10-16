//! Local HTTP endpoint (127.0.0.1:<port>) the hook scripts talk to, plus a tiny client
//! used by the CLI flags.
//!
//! Every request except `GET /health` is authenticated with the per-install key
//! (util::secret): `X-Overlay-Nonce` (32 hex chars, random per request) and
//! `X-Overlay-Auth` = hex(HMAC-SHA256(key, "req\n" + path + "\n" + nonce + "\n" + body)).
//! Replies carry `X-Overlay-Sig` = hex(HMAC-SHA256(key, "resp\n" + nonce + "\n" + body)), so
//! a hook can tell this overlay from anything else that took the port. Browser requests
//! (Origin / Sec-Fetch-*), foreign Host names, non-JSON bodies and bodies over 1 MiB are
//! refused before the body is read.
use crate::{events, util, Shared};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tiny_http::{Header, Method, Request, Response, Server};

pub const MAX_BODY: usize = 1 << 20;
/// Bodies being read at once within their first READ_DEADLINE; more POSTs get 503.
const MAX_IN_FLIGHT: usize = 32;
/// A body still arriving after this long gives its slot back. tiny_http sets no socket
/// timeout, so the reading thread itself cannot be stopped: it lingers until the client
/// sends the rest or goes away.
const READ_DEADLINE: Duration = Duration::from_secs(10);
/// Lingering readers (past their deadline, or draining a refusal while every slot was
/// taken); past this many, new POSTs get 503.
const MAX_LINGERING: usize = 256;

/// An authenticated hook request. Replies are signed with its nonce.
pub struct HookReq {
    req: Request,
    nonce: String,
    /// `X-Hook-Pid` of the hook process (0 = not sent): its card closes when it exits.
    pub pid: u32,
}

impl HookReq {
    fn respond(self, code: u16, body: String) {
        let sig = response_mac(&self.nonce, body.as_bytes());
        let resp = Response::from_string(body)
            .with_status_code(code)
            .with_header(header("Content-Type", "application/json; charset=utf-8"))
            .with_header(header("X-Overlay-Sig", &sig))
            // Always Content-Length (never chunked), so the signed bytes are the body as sent.
            .with_chunked_threshold(usize::MAX);
        let _ = self.req.respond(resp);
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

pub fn respond_json(req: HookReq, body: String) {
    req.respond(200, body);
}

fn respond_status(req: Request, code: u16) {
    let _ = req.respond(Response::empty(code));
}

/// X-Overlay-Auth for a request to `path` (no leading slash, no query).
pub fn request_mac(path: &str, nonce: &str, body: &[u8]) -> String {
    request_mac_with(&util::secret(), path, nonce, body)
}

/// X-Overlay-Sig for a reply to the request that carried `nonce`.
pub fn response_mac(nonce: &str, body: &[u8]) -> String {
    response_mac_with(&util::secret(), nonce, body)
}

fn request_mac_with(key: &str, path: &str, nonce: &str, body: &[u8]) -> String {
    util::hex(&util::hmac_sha256(key.as_bytes(), &[b"req\n", path.as_bytes(), b"\n", nonce.as_bytes(), b"\n", body]))
}

fn response_mac_with(key: &str, nonce: &str, body: &[u8]) -> String {
    util::hex(&util::hmac_sha256(key.as_bytes(), &[b"resp\n", nonce.as_bytes(), b"\n", body]))
}

fn get_header<'a>(req: &'a Request, name: &'static str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str())
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

enum Route {
    Health,
    Post { path: String, nonce: String, auth: String, pid: u32 },
}

/// Everything that needs no body: where the request goes, or the status to refuse it with.
fn route(req: &Request, port: u16) -> Result<Route, u16> {
    // Browsers always send these (also on no-cors and DNS-rebinding requests); the hooks and
    // http::post never do.
    for h in ["Origin", "Sec-Fetch-Site", "Sec-Fetch-Mode", "Sec-Fetch-Dest"] {
        if get_header(req, h).is_some() {
            return Err(403);
        }
    }
    let host = get_header(req, "Host").unwrap_or("").trim().to_ascii_lowercase();
    let with_port = |h: &str| format!("{h}:{port}");
    if !(host == "127.0.0.1" || host == "localhost" || host == with_port("127.0.0.1") || host == with_port("localhost")) {
        return Err(403);
    }
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if *req.method() == Method::Get {
        return if path == "/health" { Ok(Route::Health) } else { Err(404) };
    }
    if *req.method() != Method::Post {
        return Err(405);
    }
    let json_body = get_header(req, "Content-Type")
        .map(|v| v.trim().to_ascii_lowercase().starts_with("application/json"))
        .unwrap_or(false);
    if !json_body {
        return Err(415);
    }
    if get_header(req, "X-Overlay-Nonce").is_none() && get_header(req, "X-Overlay-Auth").is_none() {
        heal_hook_scripts();
    }
    let nonce = get_header(req, "X-Overlay-Nonce").unwrap_or("").trim().to_string();
    let auth = get_header(req, "X-Overlay-Auth").unwrap_or("").trim().to_ascii_lowercase();
    if !is_hex(&nonce, 32) || !is_hex(&auth, 64) {
        return Err(401);
    }
    match req.body_length() {
        Some(n) if n > MAX_BODY => return Err(413),
        None if get_header(req, "Transfer-Encoding").is_some() => return Err(411),
        _ => {}
    }
    let pid = get_header(req, "X-Hook-Pid").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0);
    Ok(Route::Post { path, nonce, auth, pid })
}

/// A POST with no auth headers at all comes from hook scripts of an older version (an old
/// exe started while this one runs rewrites them): put ours back, at most every 30 s. That
/// request still fails; the next hook call runs the restored script.
fn heal_hook_scripts() {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = util::now_ms();
    let last = LAST.load(Ordering::SeqCst);
    if now - last < 30_000 || LAST.compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    // Off the accept loop: writing a file may retry for a moment.
    std::thread::spawn(|| {
        if crate::hooks::write_scripts() {
            util::log_line(&["hooks-rewritten"]);
        }
    });
}

/// Dropping a tiny_http request drains a Content-Length body's unread rest on the dropping
/// thread, with one buffer as large as that rest. Bodies up to 1 KiB (without Expect) are read
/// before we see the request and cost nothing; a chunked body is not drained on drop. Anything
/// else must not be dropped on the accept loop.
fn lazy_body(req: &Request) -> bool {
    match req.body_length() {
        Some(0) | None => false,
        Some(n) => n > 1024 || get_header(req, "Expect").is_some(),
    }
}

/// Body-reading capacity shared by the accept loop and its workers.
#[derive(Default)]
struct Limits {
    in_flight: AtomicUsize,
    lingering: AtomicUsize,
}

impl Limits {
    fn overloaded(&self) -> bool {
        self.lingering.load(Ordering::SeqCst) >= MAX_LINGERING
    }
}

const HOLDS_SLOT: u8 = 0;
const LINGERING: u8 = 1;
const READ_DONE: u8 = 2;

/// One thread reading a request body. It holds one of the MAX_IN_FLIGHT slots until the body
/// has been read or READ_DEADLINE passes; then (or when it started without a slot) it counts
/// as lingering until it finishes. Dropping it releases whatever it still holds.
struct Reader {
    limits: Arc<Limits>,
    state: AtomicU8,
}

impl Reader {
    /// A slot for reading one body, or None when MAX_IN_FLIGHT bodies are being read already.
    fn slot(limits: &Arc<Limits>) -> Option<Arc<Reader>> {
        if limits.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            limits.in_flight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let reader = Arc::new(Reader { limits: limits.clone(), state: AtomicU8::new(HOLDS_SLOT) });
        watch(&reader);
        Some(reader)
    }

    /// A reader without a slot (a refusal that must drain while every slot is taken).
    fn lingering(limits: &Arc<Limits>) -> Arc<Reader> {
        limits.lingering.fetch_add(1, Ordering::SeqCst);
        Arc::new(Reader { limits: limits.clone(), state: AtomicU8::new(LINGERING) })
    }

    /// The body has been read (or failed): release the slot or the lingering count, once.
    fn done(&self) {
        match self.state.swap(READ_DONE, Ordering::SeqCst) {
            HOLDS_SLOT => self.limits.in_flight.fetch_sub(1, Ordering::SeqCst),
            LINGERING => self.limits.lingering.fetch_sub(1, Ordering::SeqCst),
            _ => 0,
        };
    }

    /// READ_DEADLINE passed: give the slot back and count the reader as lingering. Counted
    /// before the switch, so a done() racing with it never takes the count below zero.
    fn expire(&self) {
        self.limits.lingering.fetch_add(1, Ordering::SeqCst);
        if self.state.compare_exchange(HOLDS_SLOT, LINGERING, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            self.limits.in_flight.fetch_sub(1, Ordering::SeqCst);
        } else {
            self.limits.lingering.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.done();
    }
}

/// One watchdog thread expires readers that still hold their slot after READ_DEADLINE.
fn watch(reader: &Arc<Reader>) {
    static WATCHED: Mutex<Vec<(Instant, Weak<Reader>)>> = Mutex::new(Vec::new());
    static START: Once = Once::new();
    WATCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((Instant::now() + READ_DEADLINE, Arc::downgrade(reader)));
    START.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = Instant::now();
            let due: Vec<Weak<Reader>> = {
                let mut w = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
                // Finished readers are gone (their Weak no longer upgrades): drop them too.
                w.retain(|(_, r)| r.strong_count() > 0);
                let (due, keep): (Vec<_>, Vec<_>) = w.drain(..).partition(|(at, _)| *at <= now);
                *w = keep;
                due.into_iter().map(|(_, r)| r).collect()
            };
            for r in due {
                if let Some(r) = r.upgrade() {
                    r.expire();
                }
            }
        });
    });
}

/// Refuse a request without using its body. A lazy body is drained on its own thread first
/// (with every slot taken, as a lingering reader) and never leaked: a forgotten request
/// would keep its socket and a tiny_http connection thread forever.
fn refuse(req: Request, code: u16, limits: &Arc<Limits>) {
    if !lazy_body(&req) {
        respond_status(req, code);
        return;
    }
    let reader = Reader::slot(limits).unwrap_or_else(|| Reader::lingering(limits));
    std::thread::spawn(move || {
        drain_and_respond(req, code);
        reader.done();
    });
}

/// Read and discard the rest of the body in small chunks, then answer. Once fully read the
/// body reader is empty, so dropping the request buffers nothing.
fn drain_and_respond(mut req: Request, code: u16) {
    let _ = std::io::copy(req.as_reader(), &mut std::io::sink());
    respond_status(req, code);
}

/// Nonces seen recently: a captured request cannot be replayed.
fn fresh_nonce(nonce: &str) -> bool {
    static SEEN: Mutex<Option<(HashSet<String>, VecDeque<String>)>> = Mutex::new(None);
    let mut guard = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let (set, order) = guard.get_or_insert_with(|| (HashSet::new(), VecDeque::new()));
    if !set.insert(nonce.to_string()) {
        return false;
    }
    order.push_back(nonce.to_string());
    if order.len() > 4096 {
        if let Some(old) = order.pop_front() {
            set.remove(&old);
        }
    }
    true
}

pub fn serve(server: Server, app: AppHandle, shared: Arc<Shared>) {
    let port = shared.port;
    let limits = Arc::new(Limits::default());
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            match route(&req, port) {
                Err(code) => refuse(req, code, &limits),
                Ok(Route::Health) if lazy_body(&req) => refuse(req, 400, &limits),
                Ok(Route::Health) => {
                    let body = json!({"ok": true, "engine": "tauri", "version": env!("CARGO_PKG_VERSION")}).to_string();
                    let resp = Response::from_string(body)
                        .with_header(header("Content-Type", "application/json; charset=utf-8"));
                    let _ = req.respond(resp);
                }
                Ok(Route::Post { path, nonce, auth, pid }) => {
                    let reader = if limits.overloaded() { None } else { Reader::slot(&limits) };
                    let Some(reader) = reader else {
                        refuse(req, 503, &limits);
                        continue;
                    };
                    // Read and handle the body off the accept loop: a client that stalls
                    // mid-body only holds its own thread, and its slot only until READ_DEADLINE.
                    let (app, shared) = (app.clone(), shared.clone());
                    std::thread::spawn(move || {
                        handle_post(&app, &shared, req, path, nonce, auth, pid, &reader);
                    });
                }
            }
        }
    });
}

/// Read the body and check the request's MAC: the authenticated request and its body, or
/// None when it was refused. The reader's slot is released as soon as the body is in.
fn authenticate(mut req: Request, path: &str, nonce: String, auth: &str, pid: u32, reader: &Reader) -> Option<(HookReq, Vec<u8>)> {
    let mut body = Vec::new();
    let read = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body);
    if body.len() > MAX_BODY {
        drain_and_respond(req, 413);
        reader.done();
        return None;
    }
    reader.done();
    if read.is_err() {
        // A broken body: route() capped its declared length at MAX_BODY, so dropping the
        // request buffers at most that much.
        respond_status(req, 400);
        return None;
    }
    let expected = request_mac(path, &nonce, &body);
    if !util::ct_eq(expected.as_bytes(), auth.as_bytes()) || !fresh_nonce(&nonce) {
        respond_status(req, 401);
        return None;
    }
    Some((HookReq { req, nonce, pid }, body))
}

#[allow(clippy::too_many_arguments)]
fn handle_post(app: &AppHandle, shared: &Arc<Shared>, req: Request, path: String, nonce: String, auth: String, pid: u32, reader: &Reader) {
    let kind_path = path.strip_prefix('/').unwrap_or(&path).to_string();
    let Some((req, body)) = authenticate(req, &kind_path, nonce, &auth, pid, reader) else { return };
    let text = match String::from_utf8(body) {
        Ok(t) => t,
        Err(_) => {
            req.respond(400, "{\"error\":\"bad utf-8\"}".to_string());
            return;
        }
    };
    let value: Value = if text.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(_) => {
                req.respond(400, "{\"error\":\"bad json\"}".to_string());
                return;
            }
        }
    };
    let mut kind = kind_path;
    if kind.is_empty() {
        kind = value.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    }
    events::handle_request(app, shared, &kind, value, req);
}

/// Send a raw HTTP/1.1 request; returns (status, headers, body bytes).
fn exchange(port: u16, head: &str, body: &[u8], timeout_ms: u64) -> Result<(u16, String, Vec<u8>), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(timeout_ms)));
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or("malformed reply")?;
    let head = String::from_utf8_lossy(&resp[..split]).to_string();
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Ok((status, head, resp[split + 4..].to_vec()))
}

/// Signed HTTP/1.1 POST to the running overlay; returns the response body. Fails unless
/// the reply is signed with this install's key, i.e. it really came from our overlay.
pub fn post(port: u16, kind: &str, body: &str, timeout_ms: u64) -> Result<String, String> {
    let path = kind.split('?').next().unwrap_or("");
    let nonce = util::random_hex(16);
    let auth = request_mac(path, &nonce, body.as_bytes());
    let head = format!(
        "POST /{kind} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nX-Overlay-Nonce: {nonce}\r\nX-Overlay-Auth: {auth}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let (status, head, reply) = exchange(port, &head, body.as_bytes(), timeout_ms)?;
    let sig = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim().eq_ignore_ascii_case("X-Overlay-Sig").then(|| value.trim().to_ascii_lowercase())
        })
        .unwrap_or_default();
    if !util::ct_eq(sig.as_bytes(), response_mac(&nonce, &reply).as_bytes()) {
        return Err(format!("port {port} did not answer as this HeadsUp install (HTTP {status})"));
    }
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    Ok(String::from_utf8_lossy(&reply).to_string())
}

/// Unsigned `POST /quit` for Claude Overlay 2.x, which takes unauthenticated requests (this
/// version refuses it). Never http::post here: signing reads the key, and creating one
/// creates the data folder before the 2.x settings could be copied there.
pub fn quit_unsigned(port: u16) {
    let head = "POST /quit HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
    let _ = exchange(port, head, b"{}", 1500);
}

/// GET /health; Some(body) when something answered over HTTP.
pub fn health(port: u16, timeout_ms: u64) -> Option<String> {
    let head = format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    exchange(port, &head, b"", timeout_ms).ok().map(|(_, _, b)| String::from_utf8_lossy(&b).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_match_the_powershell_recipe() {
        // Reference values computed with HMACSHA256 in Windows PowerShell exactly as the
        // hooks do it (key = UTF-8 of the hex string; body = UTF-8 bytes).
        let key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let nonce = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            request_mac_with(key, "permission", nonce, "{\"cmd\":\"çğü\"}".as_bytes()),
            "3f27f5527c1734393bdbfde005008f5621e89fa48707eb510e0e45e2db9ec852"
        );
        assert_eq!(
            response_mac_with(key, nonce, b"{\"ok\":true}"),
            "fdaa0e7e2b7d5122a1f2d8bfa7fc82750b124a283b376177fc75541b2fa754f2"
        );
    }

    #[test]
    fn reader_slots_are_released_once() {
        let limits = Arc::new(Limits::default());
        let count = |l: &Limits| (l.in_flight.load(Ordering::SeqCst), l.lingering.load(Ordering::SeqCst));
        let a = Reader::slot(&limits).unwrap();
        a.done();
        a.done();
        assert_eq!(count(&limits), (0, 0));
        // Stalled past the deadline: the slot is free again, the reader lingers until it ends.
        let b = Reader::slot(&limits).unwrap();
        b.expire();
        assert_eq!(count(&limits), (0, 1));
        drop(b);
        assert_eq!(count(&limits), (0, 0));
        let held: Vec<_> = (0..MAX_IN_FLIGHT).map(|_| Reader::slot(&limits).unwrap()).collect();
        assert!(Reader::slot(&limits).is_none());
        drop(held);
        assert_eq!(count(&limits), (0, 0));
    }

    #[test]
    fn nonces_are_single_use() {
        let n = util::hex(&[7u8; 16]);
        assert!(fresh_nonce(&n));
        assert!(!fresh_nonce(&n));
    }

    /// The real route/authenticate/sign path over a socket, plus the CLI client. The key
    /// lives in a temp folder: USERPROFILE is pointed there before anything reads it.
    #[test]
    fn server_auth_end_to_end() {
        let dir = std::env::temp_dir().join(format!("headsup-test-{}", std::process::id()));
        std::env::set_var("USERPROFILE", &dir);
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let limits = Arc::new(Limits::default());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                match route(&req, port) {
                    Err(code) => refuse(req, code, &limits),
                    Ok(Route::Health) => respond_status(req, 200),
                    Ok(Route::Post { path, nonce, auth, pid }) => {
                        let path = path.trim_start_matches('/').to_string();
                        let reader = Reader::slot(&limits).unwrap();
                        if let Some((req, body)) = authenticate(req, &path, nonce, &auth, pid, &reader) {
                            let reply = format!("{{\"len\":{},\"pid\":{}}}", body.len(), req.pid);
                            req.respond(200, reply);
                        }
                    }
                }
            }
        });
        assert!(util::secret_path().starts_with(&dir));
        assert_eq!(post(port, "stop", "{\"a\":1}", 3000).unwrap(), "{\"len\":7,\"pid\":0}");

        let raw = |head: String| exchange(port, &head, b"{}", 3000).map(|r| r.0).unwrap_or_else(|e| panic!("{e}: {head}"));
        let nonce = util::random_hex(16);
        let auth = request_mac("stop", &nonce, b"{}");
        let signed = |extra: &str, host: &str, ctype: &str, auth: &str| {
            format!(
                "POST /stop HTTP/1.1\r\nHost: {host}\r\nContent-Type: {ctype}\r\nX-Overlay-Nonce: {nonce}\r\nX-Overlay-Auth: {auth}\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\n"
            )
        };
        let host = format!("127.0.0.1:{port}");
        assert_eq!(raw(signed("Origin: https://evil.example\r\n", &host, "application/json", &auth)), 403);
        assert_eq!(raw(signed("Sec-Fetch-Mode: no-cors\r\n", &host, "application/json", &auth)), 403);
        assert_eq!(raw(signed("", "rebind.example:1234", "application/json", &auth)), 403);
        assert_eq!(raw(signed("", &host, "text/plain", &auth)), 415);
        assert_eq!(raw(signed("", &host, "application/json", &"0".repeat(64))), 401);
        // Valid once, then a replay.
        assert_eq!(raw(signed("X-Hook-Pid: 1234\r\n", &host, "application/json", &auth)), 200);
        assert_eq!(raw(signed("", &host, "application/json", &auth)), 401);
        let big = format!(
            "POST /stop HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nX-Overlay-Nonce: {}\r\nX-Overlay-Auth: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            util::random_hex(16),
            "a".repeat(64),
            MAX_BODY + 1
        );
        assert_eq!(exchange(port, &big, &vec![b' '; MAX_BODY + 1], 5000).unwrap().0, 413);
        let get = format!("GET /health HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
        assert_eq!(exchange(port, &get, b"", 3000).unwrap().0, 200);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
