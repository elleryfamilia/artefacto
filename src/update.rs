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
