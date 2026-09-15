//! Self-update via [`axoupdater`], cargo-dist's own updater.
//!
//! artefacto's releases are built by cargo-dist and axoupdater is cargo-dist's
//! updater, so the two agree about the release's shape by construction. That is
//! the whole reason to use it rather than reading `dist-manifest.json` by hand:
//! every asset name, target triple, checksum, archive layout and receipt
//! convention is the same code's opinion on both sides.
//!
//! It works off the **install receipt** the shell installer writes to the
//! config dir (`~/.config/artefacto/`). A binary installed any other way
//! (`cargo install`, a package manager, hand-copied) has no receipt, so
//! self-update degrades to [`Outcome::NotManaged`]: artefacto never replaces a
//! binary it did not install, and says how this one probably arrived instead.
//!
//! Nothing here is on any command's critical path. See [`crate::commands::update`]
//! for the command, and note that no request is made except by an explicit
//! `artefacto update`.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The app name axoupdater uses to find the receipt and the releases.
const APP: &str = "artefacto";

/// Opt out of any automatic check. The hard kill switch, named beside
/// `ARTEFACTO_NO_SKILL_INSTALL` because it is the same kind of promise: the
/// thing that happens on its own is suppressible, and the thing you typed
/// still does what you asked.
pub const NO_CHECK_ENV: &str = "ARTEFACTO_NO_UPDATE_CHECK";

/// The installer that put a managed copy here, and the way back to one.
pub const INSTALL_COMMAND: &str =
    "curl -LsSf https://github.com/elleryfamilia/artefacto/releases/latest/download/artefacto-installer.sh | sh";

/// What [`perform`] did, or why it could not.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The binary was replaced. `from` is the prior version when the receipt
    /// knew it.
    Updated { from: Option<String>, to: String },
    /// Already the newest release — nothing to do.
    AlreadyCurrent,
    /// `--check` only: a newer release exists and was not installed.
    UpdateAvailable,
    /// No install receipt, so this copy is not one artefacto may replace.
    NotManaged,
    /// There is a receipt, but it is for a binary somewhere else — this is a
    /// development build, a copy, or a second checkout.
    ///
    /// Worth its own outcome rather than folding into `AlreadyCurrent`.
    /// axoupdater's `is_update_needed` returns *false* in this case, which
    /// through a naive wrapper reads as "you are on the latest release" — a
    /// confident answer to a question it never asked. It also never reached
    /// the network. Saying which copy would be replaced is the only honest
    /// report, and it is what keeps `cargo run -- update` from looking like
    /// it did something.
    NotThisCopy { installed: String },
    /// The release host could not be reached, or answered something
    /// unreadable. Never an error: not knowing whether a new version exists
    /// is a smaller problem than a command that fails because GitHub is down.
    Unknown,
}

/// Run the update, or with `check_only` just report whether one exists.
///
/// The only function in artefacto that makes an outbound request, and it runs
/// only from `artefacto update`, which somebody typed.
pub fn perform(check_only: bool) -> Result<Outcome> {
    use axoupdater::AxoUpdater;

    let mut updater = AxoUpdater::new_for(APP);
    // No receipt means this copy was not installed by the installer, so
    // there is nothing artefacto is entitled to replace. Not an error: the
    // person gets told how they probably installed it and what to run.
    if updater.load_receipt().is_err() {
        return Ok(Outcome::NotManaged);
    }
    // The receipt exists, but is it for *this* binary? axoupdater installs
    // into the receipt's prefix, not into wherever the running executable
    // sits, so without this a `cargo run -- update` from a checkout would
    // replace the copy on the developer's PATH. Its own guard already refuses
    // that, but it refuses by reporting "no update needed", which is
    // indistinguishable from being current. Ask first and say so plainly.
    if !updater
        .check_receipt_is_for_this_executable()
        .unwrap_or(false)
    {
        return Ok(Outcome::NotThisCopy {
            installed: updater
                .install_prefix_root()
                .map(|p| p.to_string())
                .unwrap_or_else(|_| "the installed copy".to_string()),
        });
    }
    // The receipt says *how* this copy was installed. It does not reliably say
    // *what* is installed: it records the version the installer wrote and does
    // not follow a binary that changed underneath it, so on this machine it
    // still reads 0.1.0. The running binary cannot be wrong about its own
    // version, and by this point the eligibility check has established that
    // the running binary *is* the installed one.
    if let Ok(running) = env!("CARGO_PKG_VERSION").parse::<axoupdater::Version>() {
        let _ = updater.set_current_version(running);
    }
    if check_only {
        return Ok(match updater.is_update_needed_sync() {
            Ok(true) => Outcome::UpdateAvailable,
            Ok(false) => Outcome::AlreadyCurrent,
            Err(_) => Outcome::Unknown,
        });
    }
    match updater.run_sync() {
        Ok(Some(result)) => Ok(Outcome::Updated {
            from: result.old_version.map(|v| v.to_string()),
            to: result.new_version.to_string(),
        }),
        Ok(None) => Ok(Outcome::AlreadyCurrent),
        Err(_) => Ok(Outcome::Unknown),
    }
}

/// Whether the automatic check is allowed to run at all. Any value disables
/// it; an empty one counts, because `ARTEFACTO_NO_UPDATE_CHECK=` reads as
/// "off" to everyone who types it.
pub fn check_suppressed() -> bool {
    std::env::var_os(NO_CHECK_ENV).is_some()
}

// ---- the check that nothing waits on ---------------------------------------
//
// Nothing in artefacto ever blocks on the network. Reading the verdict is a
// file read; *producing* it happens in a process nobody is waiting for. So
// there is no timeout to pick, no thread that can hang, and no command that
// gets slower because GitHub is slow. The whole price is that a brand-new
// release is noticed one invocation late.

/// How often the verdict is re-asked.
const DAILY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the last completed check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A newer release exists.
    Available,
    /// This binary is the newest release.
    Current,
    /// The check could not complete: offline, or not a copy artefacto may
    /// replace. Also the honest placeholder written before a refresher is
    /// spawned, which doubles as its retry backoff if the child never lands.
    Failed,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Available => "available",
            Verdict::Current => "current",
            Verdict::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Verdict> {
        match s {
            "available" => Some(Verdict::Available),
            "current" => Some(Verdict::Current),
            "failed" => Some(Verdict::Failed),
            _ => None,
        }
    }
}

/// The persisted result: when, by which binary version, and what it found.
///
/// The version stamp is what makes this correct across an update. An
/// "available" computed by 0.2.0 must never nudge the 0.3.0 that replaced it,
/// and a cache from any other version is simply ignored — which also means
/// the state self-heals the moment a new binary runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckCache {
    pub at: SystemTime,
    pub version: String,
    pub verdict: Verdict,
}

/// The one decision, pure over `(cache, now)`: no clock of its own, no
/// filesystem, no environment. Returns `(a newer one exists, go and re-ask)`.
///
/// The first half always comes from the last known verdict, even when it is
/// due for a refresh: stale-but-known beats silent. The second half is simply
/// "the verdict is missing, or old, or was written by a different binary".
pub fn decide(cache: Option<&CheckCache>, now: SystemTime) -> (bool, bool) {
    let mine = cache.filter(|c| c.version == env!("CARGO_PKG_VERSION"));
    let available = mine.is_some_and(|c| c.verdict == Verdict::Available);
    let stale = match mine {
        None => true,
        // A clock that went backwards counts as due rather than as never due
        // again: `duration_since` fails in exactly that case.
        Some(c) => now.duration_since(c.at).map(|d| d >= DAILY).unwrap_or(true),
    };
    (available, stale)
}

/// Where the verdict lives: beside the repositories' state directories rather
/// than inside any one of them, so this is one answer per machine.
pub fn cache_path() -> PathBuf {
    crate::server::state_dir::shared_dir().join("update-check")
}

/// Read the verdict. One line: `<unix-secs> <version> <verdict>`. Anything
/// unreadable is `None`, which reads as "due for a refresh" — never an error,
/// because a corrupt cache file is not worth failing a command over.
pub fn read_cache(path: &Path) -> Option<CheckCache> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let secs: u64 = parts.next()?.parse().ok()?;
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let version = parts.next()?.to_string();
    let verdict = Verdict::parse(parts.next()?)?;
    Some(CheckCache {
        at,
        version,
        verdict,
    })
}

/// Persist the verdict. Best-effort: a machine whose state directory cannot
/// be written still runs reviews, it just re-asks more often than it needs to.
pub fn write_cache(path: &Path, cache: &CheckCache) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secs = cache
        .at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    std::fs::write(
        path,
        format!("{secs} {} {}\n", cache.version, cache.verdict.as_str()),
    )
}

/// What to tell a page: `Available`, or nothing at all.
///
/// One small file read and no other work. Deliberately does **not** start a
/// refresher: this runs while a page request is being served, and a request
/// handler is the wrong place to fork a process. Keeping the verdict warm is
/// [`tick_check`]'s job, on the server's own clock.
pub fn cached_verdict() -> Option<Verdict> {
    if check_suppressed() {
        return None;
    }
    let (available, _) = decide(read_cache(&cache_path()).as_ref(), SystemTime::now());
    available.then_some(Verdict::Available)
}

/// Keep the verdict warm. Called from the server's tick loop and nowhere else.
///
/// Performs no request itself: the most it does is one file read, one file
/// write, and a `fork`/`exec` of a process it never waits for.
pub fn tick_check() {
    if check_suppressed() {
        return;
    }
    if claim_refresh(&cache_path(), SystemTime::now()) {
        spawn_refresher();
    }
}

/// Take the right to go and ask, stamping the cache if the answer is yes.
///
/// Separated from the spawn so the rule can be tested without forking
/// anything, because the rule is the whole of the concurrency story: the stamp
/// lands **before** the child starts, which closes the window immediately, so
/// a burst of ticks produces one refresher rather than a swarm of them. The
/// child then overwrites the stamp with the real answer.
///
/// `Failed` is the honest placeholder — it claims nothing — and it doubles as
/// the retry backoff if the child dies before writing.
fn claim_refresh(path: &Path, now: SystemTime) -> bool {
    let (_, stale) = decide(read_cache(path).as_ref(), now);
    if !stale {
        return false;
    }
    let _ = write_cache(
        path,
        &CheckCache {
            at: now,
            version: env!("CARGO_PKG_VERSION").to_string(),
            verdict: Verdict::Failed,
        },
    );
    true
}

/// Launch `artefacto update --refresh-cache` detached: nothing inherited,
/// nothing waited on, its own process group so it outlives a short-lived
/// parent. Best-effort — a spawn that fails just means the stamped window
/// retries later.
fn spawn_refresher() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["update", "--refresh-cache"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    if let Ok(child) = cmd.spawn() {
        drop(child);
    }
}

/// The detached child's whole job: ask, and write down the answer for next
/// time. Silent and infallible by design — its only observable effect is the
/// file the next tick reads.
pub fn refresh_cache() {
    let verdict = match perform(true) {
        Ok(Outcome::UpdateAvailable) => Verdict::Available,
        Ok(Outcome::AlreadyCurrent) => Verdict::Current,
        // Not managed, not this copy, or no answer: all "there is nothing to
        // tell the reviewer", and all worth re-asking tomorrow rather than
        // never again.
        _ => Verdict::Failed,
    };
    let _ = write_cache(
        &cache_path(),
        &CheckCache {
            at: SystemTime::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verdict,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kill_switch_is_any_value_at_all() {
        // A guard that only counted "1" would quietly ignore the two spellings
        // everyone actually reaches for.
        for value in ["1", "true", "", "0", "no"] {
            temp_env(NO_CHECK_ENV, Some(value), || {
                assert!(
                    check_suppressed(),
                    "{NO_CHECK_ENV}={value:?} must turn the check off"
                );
            });
        }
        temp_env(NO_CHECK_ENV, None, || {
            assert!(!check_suppressed(), "unset leaves the check on");
        });
    }

    fn cache(version: &str, verdict: Verdict, ago: Duration) -> CheckCache {
        CheckCache {
            at: SystemTime::now() - ago,
            version: version.to_string(),
            verdict,
        }
    }

    const MINE: &str = env!("CARGO_PKG_VERSION");
    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn a_fresh_verdict_is_reported_and_not_re_asked() {
        let now = SystemTime::now();
        let c = cache(MINE, Verdict::Available, HOUR);
        assert_eq!(decide(Some(&c), now), (true, false));
        let c = cache(MINE, Verdict::Current, HOUR);
        assert_eq!(decide(Some(&c), now), (false, false));
    }

    #[test]
    fn a_day_old_verdict_still_speaks_while_a_new_one_is_fetched() {
        // Stale-but-known beats silent: the reviewer is told what was true
        // yesterday while today's answer is being fetched behind them.
        let c = cache(MINE, Verdict::Available, HOUR * 25);
        assert_eq!(decide(Some(&c), SystemTime::now()), (true, true));
    }

    #[test]
    fn a_verdict_from_another_version_is_ignored_entirely() {
        // The case this exists for: 0.2.0 wrote "available", then updated
        // itself to 0.3.0. Reading that verdict would have the new binary
        // nudging about the release it already is.
        let c = cache("0.0.1-other", Verdict::Available, HOUR);
        assert_eq!(
            decide(Some(&c), SystemTime::now()),
            (false, true),
            "another version's verdict must not be believed, and must be replaced"
        );
    }

    #[test]
    fn no_cache_at_all_is_a_refresh_and_never_a_claim() {
        assert_eq!(decide(None, SystemTime::now()), (false, true));
    }

    #[test]
    fn a_clock_that_went_backwards_is_due_rather_than_never_due_again() {
        // `duration_since` fails when `at` is in the future, and the tempting
        // `unwrap_or(ZERO)` would read as "asked just now" -- freezing the
        // check until the clock caught up.
        let c = CheckCache {
            at: SystemTime::now() + HOUR * 48,
            version: MINE.to_string(),
            verdict: Verdict::Current,
        };
        assert_eq!(decide(Some(&c), SystemTime::now()).1, true);
    }

    #[test]
    fn the_cache_survives_a_round_trip_and_shrugs_off_rubbish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("update-check");
        let written = CheckCache {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            version: "1.2.3".to_string(),
            verdict: Verdict::Available,
        };
        write_cache(&path, &written).unwrap();
        assert_eq!(read_cache(&path), Some(written), "a round trip is lossless");

        // Every shape of damage reads as "no verdict", never as a panic and
        // never as a confident answer.
        for junk in [
            "",
            "   ",
            "not-a-number x current",
            "123",
            "123 1.2.3",
            "123 1.2.3 sideways",
        ] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(read_cache(&path), None, "{junk:?} must read as no verdict");
        }
        assert_eq!(read_cache(Path::new("/nonexistent/update-check")), None);
    }

    #[test]
    fn the_stamp_lands_before_the_child_so_a_burst_starts_one_refresher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check");
        let now = SystemTime::now();

        assert!(
            claim_refresh(&path, now),
            "nothing cached, so the first tick goes and asks"
        );
        let stamped = read_cache(&path).expect("the stamp is written before the child starts");
        assert_eq!(
            stamped.verdict,
            Verdict::Failed,
            "the placeholder claims nothing: it is not yet an answer"
        );

        // The burst. Every one of these is a tick arriving before the child
        // has had time to write anything.
        for _ in 0..10 {
            assert!(
                !claim_refresh(&path, now),
                "the window is already closed, so no second child is started"
            );
        }

        // And the placeholder is its own backoff: a child that died without
        // writing does not wedge the check forever.
        assert!(
            claim_refresh(&path, now + DAILY),
            "a day later it asks again, even though the last attempt failed"
        );
    }

    /// Set or clear one variable for the duration of `f`. The suite runs
    /// threads in one process, so this is only sound while nothing else reads
    /// the same variable concurrently — which is why the only test using it is
    /// the one that owns this name.
    fn temp_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let previous = std::env::var_os(key);
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match previous {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}
