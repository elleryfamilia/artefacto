//! Detaching from the terminal.
//!
//! Ordering is the whole content of this module.
//!
//! The listener is a raw [`std::net::TcpListener`], bound **before** the fork.
//! A file descriptor survives `fork`; a `tiny_http::Server` does not, because
//! constructing one spawns an accept thread and `fork` keeps only the calling
//! thread. The `tiny_http::Server` is therefore built in the grandchild, from
//! the inherited descriptor.
//!
//! Two mistakes this module exists to avoid, both of which produce a daemon
//! that starts, reports success, and serves nothing:
//!
//! 1. Building the server before forking. The grandchild then has a listening
//!    socket with no acceptor.
//! 2. Closing "inherited" descriptors with a blind range loop. That closes the
//!    listener too, and it double-closes descriptors Rust still owns, so a
//!    later `open` can silently land on a number something else is using. This
//!    module closes nothing by number: `dup2` redirects the three standard
//!    descriptors, and everything else is dropped by its owner.
//!
//! The parent does not exit until the grandchild signals readiness on a pipe,
//! so a command run immediately after `serve` always finds a live server.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::path::Path;

extern "C" {
    fn fork() -> i32;
    fn setsid() -> i32;
}

/// What the caller should do next. The parent must **return**, not continue
/// into the run loop: an earlier design had `daemonize` claim it "returns only
/// in the grandchild" while the parent branch fell through, which is not a
/// thing a single return type can express.
pub enum ForkOutcome {
    /// This process is the original caller. `serve` returns success.
    Parent,
    /// This process is the detached grandchild. It must report through the
    /// `Readiness` exactly once, then serve.
    Child(Readiness),
}

/// The grandchild's one-shot channel back to the waiting parent.
///
/// Every startup step between `daemonize` and serving must report through
/// this. A step that fails and simply returns leaves the parent reading EOF,
/// which it can only describe as "the server died", losing the real reason.
pub struct Readiness {
    write_fd: RawFd,
}

impl Readiness {
    /// The server is bound, recorded, and about to accept.
    pub fn ready(self) {
        let _ = write_fd(self.write_fd, b"K");
        close(self.write_fd);
        std::mem::forget(self);
    }

    /// Report why startup failed, then leave. The parent prints this verbatim.
    pub fn fail(self, message: &str) -> ! {
        let mut payload = Vec::with_capacity(message.len() + 1);
        payload.push(b'E');
        payload.extend_from_slice(message.as_bytes());
        let _ = write_fd(self.write_fd, &payload);
        close(self.write_fd);
        std::mem::forget(self);
        std::process::exit(1);
    }
}

impl Drop for Readiness {
    /// A `Readiness` that is dropped without reporting is a bug: the parent
    /// would wait for a message that never comes. Say so on the way past.
    fn drop(&mut self) {
        let _ = write_fd(self.write_fd, b"Estartup ended without reporting readiness");
        close(self.write_fd);
    }
}

/// Fork twice, detach, and hand the grandchild a `Readiness`.
///
/// `keep` names descriptors the grandchild needs — the bound listener above
/// all. They are not touched. Nothing else is closed by number either; see the
/// module docs.
pub fn daemonize(log_path: &Path, keep: &[RawFd]) -> Result<ForkOutcome> {
    // Open both files before forking. After a fork there is exactly one
    // thread, and an allocator lock held by a thread that no longer exists
    // would deadlock the child on its first allocation.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let devnull = std::fs::File::open("/dev/null").context("opening /dev/null")?;
    let (read_fd, write_fd) = pipe()?;

    // SAFETY: the standard double-fork sequence. Every syscall is checked, and
    // -1 is an error rather than being mistaken for the parent branch.
    let first = unsafe { fork() };
    match first {
        -1 => {
            close(read_fd);
            close(write_fd);
            bail!("fork failed: {}", std::io::Error::last_os_error());
        }
        0 => {}
        _ => {
            close(write_fd);
            return parent_wait(read_fd).map(|()| ForkOutcome::Parent);
        }
    }

    // First child.
    if unsafe { setsid() } < 0 {
        let e = std::io::Error::last_os_error();
        let _ = write_fd_str(write_fd, &format!("Esetsid failed: {e}"));
        std::process::exit(1);
    }

    // The second fork gives up session leadership, so the daemon can never
    // acquire a controlling terminal.
    let second = unsafe { fork() };
    match second {
        -1 => {
            let e = std::io::Error::last_os_error();
            let _ = write_fd_str(write_fd, &format!("Esecond fork failed: {e}"));
            std::process::exit(1);
        }
        0 => {}
        // The intermediate child leaves immediately. `_exit`, not `exit`: it
        // shares the parent's atexit handlers and buffered streams, and
        // running them twice is exactly the kind of thing that flushes a
        // buffer into a log file twice.
        _ => unsafe { libc::_exit(0) },
    }

    // Grandchild. Redirect the three standard descriptors and let every other
    // owned file be dropped normally.
    unsafe {
        libc::dup2(devnull.as_raw_fd(), libc::STDIN_FILENO);
        libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO);
        libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
    }
    drop(devnull);
    drop(log);
    close(read_fd);
    debug_assert!(
        keep.iter().all(|fd| *fd > 2),
        "stdio is redirected, not kept"
    );
    let _ = keep;

    Ok(ForkOutcome::Child(Readiness { write_fd }))
}

/// Block until the grandchild reports. Any outcome other than a `K` is an
/// error the caller should print.
fn parent_wait(read_fd: RawFd) -> Result<()> {
    // SAFETY: `read_fd` is an owned descriptor from `pipe`, handed over here.
    let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .context("waiting for the server to start")?;
    match buf.first() {
        Some(b'K') => Ok(()),
        Some(b'E') => bail!("{}", String::from_utf8_lossy(&buf[1..])),
        _ => bail!("the server exited during startup; see server.log in the state directory"),
    }
}

fn pipe() -> Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a valid two-element array for the duration of the call.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        bail!("pipe failed: {}", std::io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

fn write_fd(fd: RawFd, bytes: &[u8]) -> std::io::Result<()> {
    // SAFETY: borrowed for the length of this call; `into_raw_fd` hands the
    // descriptor straight back so the `File` never closes it on drop.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = file.write_all(bytes);
    let _ = std::os::unix::io::IntoRawFd::into_raw_fd(file);
    result
}

fn write_fd_str(fd: RawFd, s: &str) -> std::io::Result<()> {
    write_fd(fd, s.as_bytes())
}

fn close(fd: RawFd) {
    // SAFETY: called once per descriptor this module owns.
    unsafe {
        libc::close(fd);
    }
}
