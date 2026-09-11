//! Where the server keeps its files, and whether a recorded server is real.
//!
//! Takes no locks. Reads the environment only through `state_dir`, which
//! `main` calls; everything else takes the base as a parameter so tests never
//! mutate process-global state.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// What must exist before the event log can be read. Everything else about a
/// running server is folded from the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFile {
    pub pid: u32,
    pub port: u16,
    pub secret: String,
    pub started_at: String,
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!("not inside a git repository: {}", cwd.display());
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// `<base>/artefacto/<16 hex of sha256(repo path)>`.
pub fn state_dir_in(base: &Path, repo_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    base.join("artefacto").join(&hash[..16])
}

pub fn state_dir(repo_root: &Path) -> PathBuf {
    state_dir_in(&ambient_base(), repo_root)
}

fn ambient_base() -> PathBuf {
    // Not `if let ... { if ... }`: clippy rejects the nesting, and let-chains
    // need edition 2024, which this crate does not use.
    let xdg = std::env::var("XDG_STATE_HOME").unwrap_or_default();
    if !xdg.is_empty() {
        return PathBuf::from(xdg);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local").join("state")
}

pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS must provide randomness");
    hex(&bytes)
}

/// A second credential from the same secret, so the page's cookie survives a
/// restart without ever being the bearer secret. Domain-separated by
/// `purpose`, so one credential leaking does not yield another.
pub fn derive_credential(secret: &str, purpose: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(purpose.as_bytes());
    hasher.update([0u8]);
    hasher.update(secret.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Signal 0 asks "could I signal this process" without sending one.
///
/// Two known limits, both acceptable here: a process owned by another user
/// fails with EPERM and reads as dead, and a reused pid reads as alive. The
/// server is per-user and per-repository, and `stop` confirms identity over
/// the authenticated port before signalling anything.
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering a signal. No memory is touched.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// The file as written, whether or not its process still exists. `serve` uses
/// this to recover the port and secret after a clean shutdown.
pub fn read_server_file_any(dir: &Path) -> Option<ServerFile> {
    let raw = fs::read_to_string(dir.join("server.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// `None` for absent, corrupt, or dead — every caller treats those the same.
pub fn read_server_file(dir: &Path) -> Option<ServerFile> {
    let f = read_server_file_any(dir)?;
    if is_alive(f.pid) {
        Some(f)
    } else {
        None
    }
}

/// Written to a temp file at 0600, synced, then renamed over the target. A
/// truncate-in-place would leave a half-written file readable after a crash,
/// and would inherit a loosened mode from the file already there.
pub fn write_server_file(dir: &Path, f: &ServerFile) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let final_path = dir.join("server.json");
    let tmp_path = dir.join("server.json.tmp");

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let file = opts
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    serde_json::to_writer_pretty(&file, f)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(())
}

/// An advisory exclusive lock, held for as long as the value lives. Two
/// `serve` invocations racing would otherwise both find no server and both
/// start a daemon.
pub struct StartupLock {
    _file: fs::File,
}

impl StartupLock {
    pub fn acquire(dir: &Path) -> Option<StartupLock> {
        fs::create_dir_all(dir).ok()?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("startup.lock"))
            .ok()?;
        // SAFETY: flock on a valid owned descriptor. LOCK_NB returns rather
        // than blocking when another process holds it.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Some(StartupLock { _file: file })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample(pid: u32) -> ServerFile {
        ServerFile {
            pid,
            port: 4321,
            secret: new_secret(),
            started_at: "2026-09-09T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn state_dir_is_keyed_by_repo_path() {
        let base = Path::new("/tmp/base");
        let a = state_dir_in(base, Path::new("/tmp/one"));
        let b = state_dir_in(base, Path::new("/tmp/two"));
        assert_ne!(a, b, "different repos must not share a state directory");
        assert_eq!(
            a,
            state_dir_in(base, Path::new("/tmp/one")),
            "must be stable"
        );
        assert!(a.starts_with(base));
        let leaf = a.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(leaf.len(), 16, "16 hex characters of the repo-path digest");
        assert!(leaf.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn secret_is_256_bits_of_hex() {
        let s = new_secret();
        assert_eq!(s.len(), 64);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(s, new_secret());
    }

    #[test]
    fn the_page_credential_is_derived_not_random() {
        let secret = new_secret();
        let a = derive_credential(&secret, "page-cookie");
        assert_eq!(
            a,
            derive_credential(&secret, "page-cookie"),
            "same secret, same cookie"
        );
        assert_ne!(
            a,
            derive_credential(&secret, "other"),
            "different purposes must not collide"
        );
        assert_ne!(
            a, secret,
            "the cookie must never be the bearer secret itself"
        );
        assert_ne!(a, derive_credential(&new_secret(), "page-cookie"));
    }

    #[test]
    fn server_file_round_trips_at_0600() {
        let dir = tempfile::tempdir().unwrap();
        let f = sample(std::process::id());
        write_server_file(dir.path(), &f).unwrap();

        let mode = std::fs::metadata(dir.path().join("server.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the file carries the secret");

        let back = read_server_file(dir.path()).expect("our own pid is alive");
        assert_eq!(back.port, 4321);
        assert_eq!(back.secret, f.secret);
    }

    #[test]
    fn rewriting_repairs_a_loosened_mode() {
        let dir = tempfile::tempdir().unwrap();
        write_server_file(dir.path(), &sample(std::process::id())).unwrap();
        let path = dir.path().join("server.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_server_file(dir.path(), &sample(std::process::id())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "an atomic rename replaces the file, so the mode cannot be inherited from the old one"
        );
    }

    #[test]
    fn a_dead_pid_reads_as_no_server_but_keeps_the_port_and_secret() {
        let dir = tempfile::tempdir().unwrap();
        let f = sample(0); // pid 0 is never a live user process
        write_server_file(dir.path(), &f).unwrap();

        assert!(
            read_server_file(dir.path()).is_none(),
            "a dead pid means no server"
        );
        let recovered = read_server_file_any(dir.path()).expect("the file itself is still there");
        assert_eq!(recovered.port, 4321, "a restart must rebind the same port");
        assert_eq!(
            recovered.secret, f.secret,
            "and keep the page's cookie valid"
        );
    }

    #[test]
    fn a_corrupt_file_reads_as_no_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.json"), b"{not json").unwrap();
        assert!(read_server_file(dir.path()).is_none());
        assert!(read_server_file_any(dir.path()).is_none());
    }

    #[test]
    fn the_startup_lock_excludes_a_second_holder() {
        let dir = tempfile::tempdir().unwrap();
        let first = StartupLock::acquire(dir.path()).expect("first caller wins");
        assert!(
            StartupLock::acquire(dir.path()).is_none(),
            "two concurrent serve calls must not both start a daemon"
        );
        drop(first);
        assert!(
            StartupLock::acquire(dir.path()).is_some(),
            "released on drop"
        );
    }
}
