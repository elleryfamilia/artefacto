//! The page's WebSocket. **Outbound only.**
//!
//! # Why outbound only
//!
//! `tiny_http::Request::upgrade` returns `Box<dyn ReadWrite + Send>`, and
//! `ReadWrite` is exactly `Read + Write` with a blanket impl
//! (`tiny_http-0.12.0/src/request.rs:503`). There is no `set_read_timeout`, no
//! `set_nonblocking`, no `AsRawFd`, and no downcast. The underlying socket is
//! unreachable.
//!
//! That rules out every design where one thread reads while another writes:
//! a blocking `read()` cannot be bounded, so a reader either holds the
//! socket's lock forever — deadlocking every broadcast, and through
//! `page_count` the accept loop itself — or the two threads race on a
//! `WebSocket` whose framing state is not safe to share.
//!
//! So the socket carries frames **server to page** and nothing else. One
//! thread owns it and only ever writes. Reviewer commands arrive over
//! authenticated HTTP instead (`POST /a/<artifact>/cmd`), which costs one
//! loopback round trip per comment and removes the entire class of bug.
//!
//! A second port for the socket would have been the other way out, and is
//! worse: a different port is a different origin, so the cookie would not be
//! sent and the page could not authenticate at all.
//!
//! # Locks
//!
//! Takes `sockets` only, and never across I/O: `broadcast` clones the channel
//! senders out under the lock, releases it, and only then sends.

use crate::server::event::Frame;
use crate::server::http::{error_response, header, Shared};
use crate::server::page::{cookie_ok, origin_ok_strict};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Role, WebSocket};
use tungstenite::Message;

/// A page that cannot keep up is disconnected rather than allowed to buffer
/// the whole event stream. Generous for a reviewer, bounded for the server.
const OUTBOUND_QUEUE: usize = 256;

/// How often the writer sends a Ping when there is nothing else to send.
///
/// With no reader, a disconnect is only visible on a failed write — and the
/// first write after the peer goes away usually succeeds, because it lands in
/// the kernel's send buffer. Without this, a page that closed silently would
/// stay in the registry until the next two broadcasts, which for a quiet
/// review could be a long time. `page_count` gates self-exit and the away
/// nudge, so a stale count is not harmless.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(20);

/// The first frame on every socket, carrying the page's own id.
pub const HELLO_FORMAT: &str = "artefacto.hello/1";

/// `Request::upgrade` hands back a boxed `ReadWrite`, which does not itself
/// implement `Read` and `Write`. Delegating through a newtype is the fix.
struct Sock(Box<dyn tiny_http::ReadWrite + Send>);

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

struct PageHandle {
    id: u64,
    tx: SyncSender<String>,
}

#[derive(Default)]
pub struct PageSockets {
    pages: Mutex<Vec<PageHandle>>,
    next_id: AtomicU64,
}

pub fn page_count(shared: &Shared) -> usize {
    shared.sockets.pages.lock().unwrap().len()
}

/// Send a frame to every connected page.
pub fn broadcast(shared: &Shared, frame: &Frame) {
    broadcast_except(shared, frame, u64::MAX)
}

/// Send to every connected page except `skip`. A page that just posted a
/// command already received the result directly; sending it the broadcast too
/// would make it count the change twice.
pub fn broadcast_except(shared: &Shared, frame: &Frame, skip: u64) {
    let text = serde_json::to_string(frame).expect("a frame always serializes");
    // Clone the senders out, then release the lock. Nothing touches a socket
    // while the registry is held, so one stalled tab cannot stall the others.
    let targets: Vec<(u64, SyncSender<String>)> = {
        let pages = shared.sockets.pages.lock().unwrap();
        pages
            .iter()
            .filter(|p| p.id != skip)
            .map(|p| (p.id, p.tx.clone()))
            .collect()
    };
    let mut gone = Vec::new();
    for (id, tx) in targets {
        match tx.try_send(text.clone()) {
            Ok(()) => {}
            // Disconnected, or too far behind to catch up. Either way this
            // page is finished; the reviewer reloads to resync.
            Err(TrySendError::Disconnected(_)) | Err(TrySendError::Full(_)) => gone.push(id),
        }
    }
    if !gone.is_empty() {
        let mut pages = shared.sockets.pages.lock().unwrap();
        pages.retain(|p| !gone.contains(&p.id));
    }
}

/// Drop every page's sender. Each writer thread delivers what is already
/// queued, sees the channel close, and closes its socket. Used by the stop.
pub fn close_all(shared: &Shared) {
    shared.sockets.pages.lock().unwrap().clear();
}

pub fn handle_upgrade(shared: &Arc<Shared>, request: Request) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_strict(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "exact origin required"));
        return;
    }
    if !handshake_ok(&request) {
        let _ = request.respond(error_response(
            400,
            "bad_handshake",
            "expected a websocket upgrade, version 13",
        ));
        return;
    }
    let key = header(&request, "Sec-WebSocket-Key").expect("checked by handshake_ok");
    let accept = derive_accept_key(key.as_bytes());
    let response = Response::empty(101)
        .with_header(Header::from_bytes(&b"Upgrade"[..], &b"websocket"[..]).expect("static"))
        .with_header(Header::from_bytes(&b"Connection"[..], &b"Upgrade"[..]).expect("static"))
        .with_header(
            Header::from_bytes(&b"Sec-WebSocket-Accept"[..], accept.as_bytes()).expect("accept"),
        );

    let stream = request.upgrade("websocket", response);
    let mut ws = WebSocket::from_raw_socket(Sock(stream), Role::Server, None);
    let id = shared.sockets.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::sync_channel::<String>(OUTBOUND_QUEUE);
    // A reviewer has been here, and is doing something. The away timer reads
    // the first; the idle timer reads the second, and without it a page opened
    // against a server older than the idle window would be nudged at once.
    // Marked **before** the page is counted, so a tick between the two cannot
    // see a page with no activity behind it. `core` is released before
    // `sockets` is taken; the lock order forbids holding both.
    crate::server::presence::page_arrived(shared, shared.now_ms());
    shared
        .sockets
        .pages
        .lock()
        .unwrap()
        .push(PageHandle { id, tx });

    // Always the first frame. The page needs its own id so it can put it in
    // the commands it POSTs; `broadcast_except` then skips it, and it does not
    // count its own change twice.
    //
    // It also carries who holds the lease right now. Presence is announced on
    // change only, so a page that connects after the agent attached would
    // otherwise never be told there is one. And the log's high-water mark, so
    // the page knows where the state it is about to fetch begins.
    let last_seq = shared.log.lock().unwrap().last_seq();
    let presence = crate::server::lease::current(shared)
        .map(|h| serde_json::json!({ "agent": h.agent, "mode": h.mode }));
    let hello = serde_json::json!({
        "format": HELLO_FORMAT,
        "page": id,
        "presence": presence,
        "last_seq": last_seq,
    })
    .to_string();
    if ws.send(Message::text(hello)).is_err() {
        shared.sockets.pages.lock().unwrap().retain(|p| p.id != id);
        return;
    }

    // This thread owns the socket for its whole life and only ever writes.
    // There is no reader, so nothing can be blocked by a page that is simply
    // being read rather than typed into.
    loop {
        let outcome = match rx.recv_timeout(HEARTBEAT) {
            Ok(text) => ws.send(Message::text(text)),
            Err(RecvTimeoutError::Timeout) => ws.send(Message::Ping(Vec::new().into())),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if outcome.is_err() {
            break;
        }
    }
    let _ = ws.close(None);
    let mut pages = shared.sockets.pages.lock().unwrap();
    pages.retain(|p| p.id != id);
}

/// The RFC handshake fields, checked before taking over the connection.
fn handshake_ok(req: &Request) -> bool {
    let upgrade = header(req, "Upgrade")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let connection = header(req, "Connection")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let version = header(req, "Sec-WebSocket-Version").unwrap_or_default();
    *req.method() == tiny_http::Method::Get
        && upgrade == "websocket"
        && connection.split(',').any(|part| part.trim() == "upgrade")
        && version.trim() == "13"
        && header(req, "Sec-WebSocket-Key").is_some()
}
