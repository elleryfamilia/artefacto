//! The accept loop, routing, and the two guards every route sits behind.
//!
//! # Locks
//!
//! Four, in this order: `commit` → `log` → `core` → `sockets`.
//!
//! `commit` is the mutation gate. Every state change — validate, append,
//! fold — happens while it is held, so the in-memory `Review` is always
//! exactly `fold(all events)`. Appending under `log` and then folding under
//! `core` as two separate steps lets two threads interleave and leaves the
//! two disagreeing, which is the hardest class of bug to reproduce.
//!
//! No blocking operation runs while `core` or `sockets` is held: no socket
//! write, no `fsync`, no long poll, no browser launch.

use crate::server::log::EventLog;
use crate::server::state_dir::derive_credential;
use anyhow::Result;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

/// How long with no page and no agent before the daemon exits on its own.
pub const SELF_EXIT: Duration = Duration::from_secs(30 * 60);

pub struct Core {
    /// Bootstrap token -> (artifact, issued).
    pub bootstrap: HashMap<String, (String, Instant)>,
    /// Last authenticated CLI request. Only one of the two activity clocks:
    /// the reviewer's is separate, because an agent polling every 90 seconds
    /// is not the reviewer doing anything.
    pub last_request_at: Instant,
}

pub struct Shared {
    /// The mutation gate. Acquire before `log` or `core` for any state change.
    pub commit: Mutex<()>,
    pub log: Mutex<EventLog>,
    pub core: Mutex<Core>,
    pub secret: String,
    /// Derived from `secret`, so it survives a restart. Never the secret.
    pub page_cookie: String,
    pub port: u16,
    stopping: AtomicBool,
}

impl Shared {
    pub fn new(dir: &Path, secret: String, port: u16) -> Result<Shared> {
        Ok(Shared {
            commit: Mutex::new(()),
            log: Mutex::new(EventLog::open(dir)?),
            core: Mutex::new(Core {
                bootstrap: HashMap::new(),
                last_request_at: Instant::now(),
            }),
            page_cookie: derive_credential(&secret, "page-cookie"),
            secret,
            port,
            stopping: AtomicBool::new(false),
        })
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
}

/// The accept loop. One thread per request, so a long poll blocks only its own
/// thread. A fixed worker pool would let N concurrent polls starve every other
/// route.
pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration) {
    loop {
        if shared.stopping() {
            return;
        }
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(request)) => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle(shared, request));
            }
            // `Server::unblock` also produces this, which is why the stopping
            // flag is checked at the top rather than trusting the timeout.
            Ok(None) => {
                if should_self_exit(&shared, idle) {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Spec 4.2: exit when no page and no agent have been connected for the idle
/// window. Traffic is not the measure — an unauthenticated stranger must not
/// be able to hold the daemon open, and a page connected over a WebSocket
/// sends no further HTTP requests but is very much present.
fn should_self_exit(shared: &Arc<Shared>, idle: Duration) -> bool {
    // Page and lease counts join this predicate as those subsystems land.
    let quiet = {
        let core = shared.core.lock().unwrap();
        core.last_request_at.elapsed()
    };
    quiet > idle
}

fn handle(shared: Arc<Shared>, request: Request) {
    if !host_ok(&request, shared.port) {
        let _ = request.respond(error_response(
            421,
            "bad_host",
            "this server answers only on 127.0.0.1 by address",
        ));
        return;
    }
    let url = request.url().to_string();

    if url == "/healthz" {
        let _ = request.respond(json_response(200, "{\"ok\":true}"));
        return;
    }
    if let Some(rest) = url.strip_prefix("/cli/") {
        if !bearer_ok(&request, &shared.secret) {
            let _ = request.respond(error_response(
                401,
                "unauthorized",
                "bearer secret required",
            ));
            return;
        }
        // Only an authenticated call counts as activity.
        shared.core.lock().unwrap().last_request_at = Instant::now();
        return cli_route(&shared, request, rest);
    }
    let _ = request.respond(error_response(404, "not_found", "no such route"));
}

fn cli_route(shared: &Arc<Shared>, request: Request, rest: &str) {
    match rest {
        "status" => {
            let last_seq = shared.log.lock().unwrap().last_seq();
            let body = serde_json::json!({
                "ok": true,
                "port": shared.port,
                "last_seq": last_seq,
                "artifacts": [],
            });
            let _ = request.respond(json_response(200, &body.to_string()));
        }
        "stop" => {
            shared.request_stop();
            let _ = request.respond(json_response(200, "{\"ok\":true}"));
        }
        _ => {
            let _ = request.respond(error_response(404, "not_found", "no such route"));
        }
    }
}

/// `HeaderField::equiv` needs a `&'static str`; a `&str` parameter does not
/// compile here (E0521).
pub fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

/// Exactly `127.0.0.1:<port>`. Not `localhost`, not a bare address. This is
/// the DNS-rebinding defense, and on a real machine `localhost` resolves to
/// 127.0.0.1, so this check is the only thing rejecting it.
pub fn host_ok(req: &Request, port: u16) -> bool {
    header(req, "Host").is_some_and(|h| h == format!("127.0.0.1:{port}"))
}

/// Compared without an early return, so timing does not leak how much of the
/// secret was right.
pub fn bearer_ok(req: &Request, secret: &str) -> bool {
    let Some(value) = header(req, "Authorization") else {
        return false;
    };
    let Some(given) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(given.as_bytes(), secret.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn json_response(status: u16, body: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(status)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .expect("static header"),
        )
}

/// One error shape for every route: a CLI parsing a failure should never have
/// to tell an HTML error page from a JSON one.
pub fn error_response(status: u16, code: &str, message: &str) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::json!({ "ok": false, "error": { "code": code, "message": message } });
    json_response(status, &body.to_string())
}
