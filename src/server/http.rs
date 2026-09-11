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

use crate::server::event::{Actor, Event};
use crate::server::log::EventLog;
use crate::server::review::Review;
use crate::server::state_dir::derive_credential;
use anyhow::Result;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

/// How long with no page and no agent before the daemon exits on its own.
pub const SELF_EXIT: Duration = Duration::from_secs(30 * 60);

pub struct Core {
    /// Every piece of review state, folded from the log. Never written
    /// directly: `Committer` is the only path, so this and the log cannot
    /// disagree.
    pub review: Review,
    /// Bootstrap token -> (artifact, issued).
    pub bootstrap: HashMap<String, (String, Instant)>,
    /// Last authenticated CLI request. Only one of the two activity clocks:
    /// the reviewer's is separate, because an agent polling every 90 seconds
    /// is not the reviewer doing anything.
    pub last_request_at: Instant,
    /// When the reviewer last did anything, marked by ingress. Separate from
    /// `last_request_at` on purpose: an `await` long poll is an HTTP request
    /// every 90 seconds, so one field for both would mean the idle nudge could
    /// never fire while an agent was attached.
    pub last_reviewer_activity_at: Instant,
    /// When the lease holder was last heard from, in milliseconds since
    /// [`Shared::now_ms`]'s origin. Written only by `lease`.
    ///
    /// Signed, and relative to this server's start, because the two obvious
    /// alternatives cannot express the values this needs to hold. `Instant`
    /// subtraction panics on underflow, and on a machine booted two minutes
    /// ago "five minutes ago" is not a representable `Instant` at all.
    ///
    /// It is not folded from the log on purpose: liveness is about this
    /// process, so a replayed lease starts its TTL fresh.
    pub lease_seen_ms: i64,
    /// The frame last handed to a session, waiting for that session's next
    /// call to acknowledge it. See `delivery::offer`.
    pub last_offer: Option<crate::server::delivery::Offer>,
}

pub struct Shared {
    /// The mutation gate. Acquire before `log` or `core` for any state change.
    pub commit: Mutex<()>,
    pub log: Mutex<EventLog>,
    pub core: Mutex<Core>,
    pub sockets: crate::server::socket::PageSockets,
    pub secret: String,
    /// Derived from `secret`, so it survives a restart. Never the secret.
    pub page_cookie: String,
    pub port: u16,
    /// The origin every lease age is measured from. See [`Shared::now_ms`].
    epoch: Instant,
    stopping: AtomicBool,
    /// Requests being served right now. The accept loop waits for this to
    /// reach zero before returning, so a long poll that is about to answer
    /// "stopped" is not cut off by the process exiting underneath it.
    in_flight: AtomicUsize,
}

impl Shared {
    pub fn new(dir: &Path, secret: String, port: u16) -> Result<Shared> {
        let log = EventLog::open(dir)?;
        // The single source of truth, read once at start. Everything the
        // server knows about a review, it read from here.
        let review = crate::server::fold::fold(log.since(0));
        Ok(Shared {
            commit: Mutex::new(()),
            log: Mutex::new(log),
            core: Mutex::new(Core {
                review,
                bootstrap: HashMap::new(),
                last_request_at: Instant::now(),
                last_reviewer_activity_at: Instant::now(),
                lease_seen_ms: 0,
                last_offer: None,
            }),
            sockets: Default::default(),
            page_cookie: derive_credential(&secret, "page-cookie"),
            secret,
            port,
            epoch: Instant::now(),
            stopping: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        })
    }

    /// Monotonic milliseconds since this server started.
    ///
    /// The lease's clock. Signed so that every difference taken against it is
    /// ordinary integer arithmetic that cannot panic, and monotonic so that a
    /// wall-clock change cannot expire a lease or keep a dead one alive.
    pub fn now_ms(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Refuse further work, and tell every open page on the way out.
    ///
    /// `server.stopping` is **delivered, never logged**. It carries no state a
    /// restart needs to rebuild, and writing it down would mean the next
    /// server to read that log hands every agent a frame saying it is shutting
    /// down — while it is in fact running. It joins presence and nudges in the
    /// class of things the fold deliberately ignores.
    ///
    /// Agents hear it as spec 5's `stopped` status, which `poll` derives from
    /// this flag.
    pub fn request_stop(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        // Numbered as "after everything committed so far", because the page
        // orders what it receives by seq and this is the last thing it gets.
        let seq = self.log.lock().unwrap().last_seq();
        let event = Event {
            format: crate::server::event::EVENT_FORMAT.to_string(),
            seq,
            ts: crate::server::log::now_rfc3339(),
            artifact: String::new(),
            revision: 0,
            actor: Actor::Server,
            r#type: "server.stopping".to_string(),
            data: serde_json::json!({}),
        };
        crate::server::socket::broadcast(self, &crate::server::event::Frame::of(vec![event]));
    }
}

/// The accept loop. One thread per request, so a long poll blocks only its own
/// thread. A fixed worker pool would let N concurrent polls starve every other
/// route.
pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration) {
    loop {
        if shared.stopping() {
            break;
        }
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(request)) => {
                shared.in_flight.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    handle(&shared, request);
                    shared.in_flight.fetch_sub(1, Ordering::SeqCst);
                });
            }
            // `Server::unblock` also produces this, which is why the stopping
            // flag is checked at the top rather than trusting the timeout.
            Ok(None) => {
                if should_self_exit(&shared, idle) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    drain(&shared);
}

/// Give requests already in progress a moment to answer.
///
/// Without this, `stop` returns and the process exits while a long-polling
/// `await` is still writing the `stopped` result it just computed, and the
/// agent sees a dropped connection instead of a clean shutdown. Bounded,
/// because a page's WebSocket lives in a request that never finishes.
fn drain(shared: &Arc<Shared>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while shared.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Spec 4.2: exit when no page and no agent have been connected for the idle
/// window. Traffic is not the measure — an unauthenticated stranger must not
/// be able to hold the daemon open, and a page connected over a WebSocket
/// sends no further HTTP requests but is very much present.
fn should_self_exit(shared: &Arc<Shared>, idle: Duration) -> bool {
    if crate::server::socket::page_count(shared) > 0 {
        return false;
    }
    // An agent in poll mode sends one request every 90 seconds and nothing in
    // between, so the request clock alone would call it absent. The lease is
    // what says it is still there.
    if crate::server::lease::current(shared).is_some() {
        return false;
    }
    let quiet = {
        let core = shared.core.lock().unwrap();
        core.last_request_at.elapsed()
    };
    quiet > idle
}

fn handle(shared: &Arc<Shared>, request: Request) {
    if !host_ok(&request, shared.port) {
        let _ = request.respond(error_response(
            421,
            "bad_host",
            "this server answers only on 127.0.0.1 by address",
        ));
        return;
    }
    let url = request.url().to_string();

    if url == "/ws" {
        return crate::server::socket::handle_upgrade(shared, request);
    }
    if let Some(token) = url.strip_prefix("/b/") {
        return crate::server::page::handle_bootstrap(shared, request, token);
    }
    if let Some(rest) = url.strip_prefix("/a/") {
        if let Some(artifact) = rest.strip_suffix("/cmd") {
            return crate::server::ingress::handle_command(shared, request, artifact);
        }
        return crate::server::page::serve_page(shared, request, rest);
    }
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
        let (route, query) = split_query(rest);
        let route = route.to_string();
        return cli_route(shared, request, &route, &query);
    }
    let _ = request.respond(error_response(404, "not_found", "no such route"));
}

fn cli_route(shared: &Arc<Shared>, request: Request, route: &str, query: &Query) {
    match route {
        "await" => crate::server::poll::handle_await(shared, request, query),
        "events" => crate::server::poll::handle_events(shared, request, query),
        "ack" => crate::server::poll::handle_ack(shared, request, query),
        "status" => {
            let last_seq = shared.log.lock().unwrap().last_seq();
            let body = serde_json::json!({
                "ok": true,
                "port": shared.port,
                "last_seq": last_seq,
                "artifacts": [],
                // `Holder` has no token field, so this route cannot leak one.
                // Spec 5: `status --json` never prints the session token.
                "lease": crate::server::lease::current(shared),
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

/// A request's query string, percent-decoded.
pub type Query = HashMap<String, String>;

/// Split `await?timeout=90s` into its route and its query. tiny_http hands
/// over the raw request target, so this is the only place a `?` is parsed.
pub fn split_query(target: &str) -> (&str, Query) {
    match target.split_once('?') {
        Some((route, rest)) => (route, parse_query(rest)),
        None => (target, Query::new()),
    }
}

fn parse_query(raw: &str) -> Query {
    let mut out = Query::new();
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

/// Enough of the form encoding to carry a token, an agent name, and a number.
/// A stray `%` is left as itself rather than dropping the rest of the value.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// The bearer guard for every `/cli/` route.
pub fn bearer_ok(req: &Request, secret: &str) -> bool {
    let Some(value) = header(req, "Authorization") else {
        return false;
    };
    let Some(given) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(given.as_bytes(), secret.as_bytes())
}

/// Compared without an early return, so timing does not leak how much of a
/// credential was right. Used for the bearer secret and for session tokens.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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

/// The mutation gate: the only way to append to the log.
///
/// Holding it spans deciding, appending, and folding. That is the point.
/// Appending under the log lock, releasing it, and folding under `core` as two
/// separate critical sections lets two threads interleave — A appends, B
/// appends and folds, A folds — after which the in-memory `Review` is no
/// longer `fold(log)`. Nothing detects that at the time; it surfaces later as
/// a thread with the wrong status or an id handed out twice.
///
/// It also makes read-decide-write atomic, which ingress needs: assigning
/// `c-<n>` and checking a `client_id` for a duplicate have to happen in the
/// same breath as the append, or two tabs race.
///
/// Lock order: `commit` is outermost, then `log`, then `core`. Never hold
/// `core` across the append's `fsync` — this type does not.
pub struct Committer<'a> {
    shared: &'a Shared,
    _gate: std::sync::MutexGuard<'a, ()>,
}

impl<'a> Committer<'a> {
    pub fn open(shared: &'a Shared) -> Committer<'a> {
        let gate = shared.commit.lock().unwrap();
        Committer {
            shared,
            _gate: gate,
        }
    }

    /// Read the folded state while the gate is held, so a decision taken here
    /// is still true when the append lands.
    pub fn with_review<R>(&self, f: impl FnOnce(&Review) -> R) -> R {
        let core = self.shared.core.lock().unwrap();
        f(&core.review)
    }

    /// Append one event and fold it. The two happen under this gate, so the
    /// log and the `Review` move together.
    pub fn append(
        &self,
        artifact: &str,
        revision: u32,
        actor: Actor,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<Event> {
        // `log` is held across fsync — that is why it is its own lock — but
        // `core` is not held at the same time.
        let event = {
            let mut log = self.shared.log.lock().unwrap();
            log.append(artifact, revision, actor, kind, data)?
        };
        {
            let mut core = self.shared.core.lock().unwrap();
            crate::server::fold::apply(&mut core.review, &event);
        }
        Ok(event)
    }
}

/// Read the folded state without intending to change it. Takes `core` only.
pub fn with_review<R>(shared: &Shared, f: impl FnOnce(&Review) -> R) -> R {
    let core = shared.core.lock().unwrap();
    f(&core.review)
}
