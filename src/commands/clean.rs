//! `artefacto clean`: drop sent reviews from the log, rotate the session
//! secret, keep the index.
//!
//! Spec 6.7: "`clean` truncates the log for artifacts whose review was
//! submitted." Spec 4.2: "it never renumbers." Spec 8: the secret is
//! "rotated by `clean`". Spec 4.4: the index "survives everything". The
//! server is the log's only writer while it lives, so a running server is
//! stopped first over its authenticated port, and the startup lock is held
//! while the log is rewritten so no `serve` can start on top of it. Nothing
//! that belongs to the user is touched: the plan files, the static renders,
//! the feedback documents beside them.

use crate::server::log::{self, Cleaned};
use crate::server::state_dir::{self, ServerFile, StartupLock};
use anyhow::{Context, Result};

pub fn clean(json: bool) -> Result<()> {
    let dir = crate::commands::serve::current_state_dir()?;
    if !dir.exists() {
        return report(json, false, &Cleaned::default(), false);
    }
    // A log the server would refuse is checked before anything is changed:
    // stopping the server and then failing would leave it stopped, the
    // secret unrotated, and a JSON caller with nothing on stdout. A torn
    // tail passes here, as it does at a restart.
    log::check(&dir).context(
        "the event log cannot be read, and clean would not repair it; nothing was changed",
    )?;
    let mut stopped = false;
    if state_dir::read_server_file(&dir).is_some() {
        crate::commands::serve::stop().context("stopping the server")?;
        if state_dir::read_server_file(&dir).is_some() {
            return Err(crate::commands::Exit::new(
                2,
                "the server did not stop; stop it and run clean again",
            )
            .into());
        }
        stopped = true;
    }
    let Some(_lock) = StartupLock::acquire(&dir) else {
        return Err(crate::commands::Exit::new(
            2,
            "another `artefacto serve` is starting; try again in a moment",
        )
        .into());
    };
    let cleaned = if dir.join("events.ndjson").exists() {
        log::clean(&dir).context("rewriting the event log")?
    } else {
        Cleaned::default()
    };
    // A new secret under the same port: the next `serve` rebinds where open
    // pages will look, and every cookie and bearer minted so far is dead.
    let rotated = match state_dir::read_server_file_any(&dir) {
        Some(previous) => {
            state_dir::write_server_file(
                &dir,
                &ServerFile {
                    pid: 0,
                    port: previous.port,
                    secret: state_dir::new_secret(),
                    started_at: crate::time::now_rfc3339(),
                },
            )?;
            true
        }
        None => false,
    };
    report(json, stopped, &cleaned, rotated)
}

fn report(json: bool, stopped: bool, cleaned: &Cleaned, rotated: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "server_stopped": stopped,
                "removed": cleaned.removed,
                "kept": cleaned.kept,
                "events_removed": cleaned.events_removed,
                "secret_rotated": rotated,
            })
        );
        return Ok(());
    }
    if stopped {
        println!("stopped the server");
    }
    if cleaned.removed.is_empty() {
        println!("no sent reviews to remove; the log is unchanged");
    } else {
        println!(
            "removed {} sent {} from the log: {} ({} events)",
            cleaned.removed.len(),
            plural(cleaned.removed.len(), "review"),
            cleaned.removed.join(", "),
            cleaned.events_removed
        );
    }
    if !cleaned.kept.is_empty() {
        println!(
            "kept {} open {}: {}",
            cleaned.kept.len(),
            plural(cleaned.kept.len(), "review"),
            cleaned.kept.join(", ")
        );
    }
    if rotated {
        println!("rotated the session secret; open pages need a fresh link: artefacto open");
    } else {
        println!("no session secret to rotate; no server has run here");
    }
    Ok(())
}

fn plural(n: usize, unit: &str) -> String {
    if n == 1 {
        unit.to_string()
    } else {
        format!("{unit}s")
    }
}
