//! `artefacto update` — move to the latest release, or just say whether one
//! exists.
//!
//! Works for installs the artefacto installer did, which leave a receipt.
//! Anything else is told how it was probably installed rather than being
//! refused with an error, because "I cannot update this" is information, not a
//! failure of the command.

use crate::cli::UpdateArgs;
use crate::commands::Exit;
use crate::update::{self, Outcome, INSTALL_COMMAND};
use anyhow::Result;

/// Exit codes. Agents branch on these, so they are contract rather than
/// whatever `Result` happens to produce:
///
/// - `0` — up to date, or updated, or (with `--check`) a newer one exists.
/// - `4` — this copy is not one artefacto may replace.
/// - `5` — the release host could not be reached.
///
/// `--check` finding an update is **not** a failure. A script that wants to
/// branch on it reads the line; a script that wants to know whether the check
/// worked reads the code.
const NOT_MANAGED: i32 = 4;
const NO_ANSWER: i32 = 5;

pub fn run(args: &UpdateArgs) -> Result<()> {
    match update::perform(args.check)? {
        Outcome::Updated { from, to } => {
            match from {
                Some(f) => println!("updated artefacto {f} -> {to}"),
                None => println!("updated artefacto to {to}"),
            }
            // The running process keeps its own copy of the file: replacing a
            // path does not disturb a process already running from it. So this
            // invocation is still the old version, and the next one is not.
            println!("the next artefacto you run is the new one");
            Ok(())
        }
        Outcome::AlreadyCurrent => {
            println!(
                "artefacto {} is the latest release",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        Outcome::UpdateAvailable => {
            println!(
                "a newer artefacto is available; this one is {}",
                env!("CARGO_PKG_VERSION")
            );
            println!("run `artefacto update` to install it");
            Ok(())
        }
        Outcome::NotManaged => {
            // Not an error in the sense of something going wrong: this is the
            // approved answer to "who gets a button". The tool that installed
            // it should be the tool that updates it.
            eprintln!("this artefacto was not installed by the artefacto installer, so it cannot update itself");
            eprintln!("if you installed it with cargo, use cargo; otherwise reinstall with:");
            eprintln!("  {INSTALL_COMMAND}");
            Err(Exit::new(NOT_MANAGED, "not an installer-based install").into())
        }
        Outcome::NotThisCopy { installed } => {
            eprintln!(
                "this is not the installed artefacto, so it was left alone: the receipt points at {installed}"
            );
            eprintln!("run the installed one to update it");
            Err(Exit::new(NOT_MANAGED, "not the installed copy").into())
        }
        Outcome::Unknown => {
            eprintln!("could not reach the release host, so there is no way to tell");
            Err(Exit::new(NO_ANSWER, "no answer from the release host").into())
        }
    }
}
