//! Command-line surface. Types only — no behaviour lives here.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "artefacto",
    version,
    about = "Interactive artifacts between you and your coding agent"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Work with plan artifacts.
    Plan(PlanArgs),
    /// Start the review server for this repository.
    Serve(ServeArgs),
    /// Stop the running server.
    Stop,
    /// Report what the server is doing.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Wait for something the agent should act on, then print one JSON result.
    #[command(name = "await")]
    Await(AwaitArgs),
    /// Print frames as NDJSON: the backlog, or the live stream with --follow.
    Events(EventsArgs),
    /// Acknowledge part of a frame explicitly.
    Ack(AckArgs),
}

/// `<n>` seconds, or `<n>s`, `<n>m`, `<n>h`. Spec 5 writes timeouts as `90s`
/// and `15m`, so the CLI has to read them.
pub fn parse_duration(raw: &str) -> Result<std::time::Duration, String> {
    let raw = raw.trim();
    let (digits, scale) = match raw.strip_suffix(['s', 'S']) {
        Some(d) => (d, 1),
        None => match raw.strip_suffix(['m', 'M']) {
            Some(d) => (d, 60),
            None => match raw.strip_suffix(['h', 'H']) {
                Some(d) => (d, 3600),
                None => (raw, 1),
            },
        },
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("`{raw}` is not a duration; write it as 90s, 5m, or 1h"))?;
    Ok(std::time::Duration::from_secs(n * scale))
}

/// Flags every agent-side command shares. Declared once so `await` and
/// `events` cannot drift apart.
#[derive(Args, Debug)]
pub struct AwaitArgs {
    /// How long to wait before returning `timeout`.
    #[arg(long, default_value = "90s", value_parser = parse_duration)]
    pub timeout: std::time::Duration,
    /// Start from this sequence number instead of the lease's own cursor.
    #[arg(long)]
    pub since: Option<u64>,
    /// Wake only for this artifact.
    #[arg(long)]
    pub artifact: Option<String>,
    /// The lease name. One agent acts at a time, per name.
    #[arg(long, default_value = "agent")]
    pub agent: String,
    /// The session token from a previous call. Omitting it takes a fresh
    /// lease; presenting it refreshes the one you already hold.
    #[arg(long)]
    pub session: Option<String>,
    /// Take the lease from whoever holds it.
    #[arg(long)]
    pub takeover: bool,
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    /// Stay attached and print frames as they happen.
    #[arg(long)]
    pub follow: bool,
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long)]
    pub artifact: Option<String>,
    #[arg(long, default_value = "agent")]
    pub agent: String,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub takeover: bool,
}

#[derive(Args, Debug)]
pub struct AckArgs {
    /// The sequence number to acknowledge up to.
    #[arg(long)]
    pub seq: u64,
    /// The session token. Spec 4.2: every agent mutation carries it.
    #[arg(long)]
    pub session: String,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Bind this port instead of the recorded one.
    #[arg(long)]
    pub port: Option<u16>,
    /// Stay in the foreground instead of daemonizing.
    #[arg(long)]
    pub foreground: bool,
    /// Do not open a browser.
    #[arg(long)]
    pub no_open: bool,
}

#[derive(Args, Debug)]
pub struct PlanArgs {
    #[command(subcommand)]
    pub action: PlanAction,
}

#[derive(Subcommand, Debug)]
pub enum PlanAction {
    /// Validate one or more plan files.
    Check {
        /// Plan files to validate.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Prune unknown fields and report them as warnings instead of errors.
        #[arg(long)]
        lenient: bool,
    },
    /// Render a plan to a self-contained HTML file.
    Render {
        /// The plan file to render.
        file: PathBuf,
        /// Where to write the HTML. Relative paths anchor to the current directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Do not open the rendered file in a browser.
        #[arg(long)]
        no_open: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report whether a rendered file is fresh for a plan.
    Status {
        /// The plan file.
        file: PathBuf,
        /// The rendered HTML to compare against. Defaults to `plan.html`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Publish a plan to the review server as a new revision.
    Push(PushArgs),
    /// Print the plan schema reference.
    Schema,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// The plan file to publish.
    pub file: PathBuf,
    /// The session token from a previous call. Omit it on the first push:
    /// push then takes the lease itself and returns the token to use.
    #[arg(long)]
    pub session: Option<String>,
    /// The lease name.
    #[arg(long, default_value = "agent")]
    pub agent: String,
    /// Take the lease from whoever holds it.
    #[arg(long)]
    pub takeover: bool,
    /// The revision this push was made against. Required after the first
    /// push, unless --force. It is the revision **you last saw**, never one
    /// read back from the server.
    #[arg(long)]
    pub base_revision: Option<u32>,
    /// Publish even if the server has moved on.
    #[arg(long)]
    pub force: bool,
    /// A JSON file of `{thread, status, note}` entries, applied with this
    /// revision in the same commit.
    #[arg(long)]
    pub resolutions: Option<PathBuf>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Do not open a browser on the first push.
    #[arg(long)]
    pub no_open: bool,
}
