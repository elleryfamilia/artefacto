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
    /// Mint a fresh one-time link to an artifact's page and open the browser.
    Open(OpenArgs),
    /// Wait for something the agent should act on, then print one JSON result.
    #[command(name = "await")]
    Await(AwaitArgs),
    /// Print frames as NDJSON: the backlog, or the live stream with --follow.
    Events(EventsArgs),
    /// Acknowledge part of a frame explicitly.
    Ack(AckArgs),
    /// Answer the reviewer, in a thread or on the page.
    Reply(ReplyArgs),
    /// Mark a thread addressed or declined.
    Resolve(ResolveArgs),
}

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// Which artifact to open. May be omitted when the server has exactly
    /// one.
    #[arg(long)]
    pub artifact: Option<String>,
    /// Print the link without opening a browser.
    #[arg(long)]
    pub no_open: bool,
    /// Emit machine-readable JSON. Implies --no-open.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ReplyArgs {
    /// The session token. Spec 4.2: every agent mutation carries it.
    #[arg(long)]
    pub session: String,
    /// Reply inside this thread.
    #[arg(long, conflicts_with = "artifact")]
    pub thread: Option<String>,
    /// Reply at page level on this artifact. May be omitted when the server
    /// has exactly one.
    #[arg(long)]
    pub artifact: Option<String>,
    /// Post a banner rather than a message. Spec 6.3 requires a `nudge` event;
    /// spec 5's `reply` surface needs this flag added to it.
    #[arg(long)]
    pub nudge: bool,
    /// The text. Use --stdin to read it from a pipe instead.
    #[arg(required_unless_present = "stdin")]
    pub text: Option<String>,
    /// Read the text from standard input.
    #[arg(long, conflicts_with = "text")]
    pub stdin: bool,
}

#[derive(Args, Debug)]
#[command(group = clap::ArgGroup::new("verdict").required(true))]
pub struct ResolveArgs {
    /// The thread id, such as `c-1`.
    pub thread: String,
    /// The session token.
    #[arg(long)]
    pub session: String,
    /// Name the artifact when the same thread id exists on more than one.
    #[arg(long)]
    pub artifact: Option<String>,
    /// The plan changed in response.
    #[arg(long, group = "verdict")]
    pub changed: bool,
    /// The point was considered and not acted on.
    #[arg(long, group = "verdict")]
    pub declined: bool,
    /// Why, in the reviewer's thread.
    #[arg(long)]
    pub note: Option<String>,
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
    /// The `seq` of the previous result, once you have acted on it. This is
    /// what moves the cursor: a call without it is handed the same frame
    /// again, so a crash between receiving and acting loses nothing.
    #[arg(long)]
    pub ack: Option<u64>,
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
    /// The `seq` of the previous result, once you have acted on it.
    #[arg(long)]
    pub ack: Option<u64>,
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

/// `Default` is how `push` starts a server without restating every flag.
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
    /// How long the reviewer may be quiet with the page open before the agent
    /// is nudged. `off` disables it.
    #[arg(long, default_value = "15m", value_parser = parse_window)]
    pub idle: Window,
    /// How long every page may stay closed, with the review unsubmitted,
    /// before the agent is told the reviewer is away. `off` disables it.
    #[arg(long, default_value = "5m", value_parser = parse_window)]
    pub away: Window,
}

impl Default for ServeArgs {
    fn default() -> ServeArgs {
        ServeArgs {
            port: None,
            foreground: false,
            no_open: false,
            idle: Window(Some(std::time::Duration::from_secs(15 * 60))),
            away: Window(Some(std::time::Duration::from_secs(5 * 60))),
        }
    }
}

/// A duration, or nothing at all.
///
/// A newtype rather than a bare `Option<Duration>`, because clap reads
/// `Option<T>` on a field as "this argument is optional" and then tries to
/// downcast the parsed value to `T`. With a `value_parser` that yields the
/// `Option` itself, that mismatch is a panic at parse time, not a compile
/// error — which is exactly how it was found.
#[derive(Debug, Clone, Copy)]
pub struct Window(pub Option<std::time::Duration>);

/// A duration, or `off`. Spec 16: both nudge timers "can be set to `off`".
pub fn parse_window(raw: &str) -> Result<Window, String> {
    if raw.trim().eq_ignore_ascii_case("off") {
        return Ok(Window(None));
    }
    parse_duration(raw).map(|d| Window(Some(d)))
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
