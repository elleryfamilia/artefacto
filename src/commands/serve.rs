//! `serve`, `stop`, `status`, and `open`.

use crate::cli::{OpenArgs, ServeArgs};
use crate::server::daemon::{self, ForkOutcome};
use crate::server::http::{self, Shared, SELF_EXIT};
use crate::server::log::now_rfc3339;
use crate::server::state_dir::{self, ServerFile, StartupLock};
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Exit code for "there is no server". Agents branch on it, so it is part of
/// the contract rather than an implementation detail.
pub use crate::client::EXIT_NO_SERVER;

pub fn serve(args: &ServeArgs) -> Result<()> {
    let dir = current_state_dir()?;
    std::fs::create_dir_all(&dir)?;

    // One winner. Two concurrent `serve` calls would otherwise both find no
    // server and both start a daemon.
    let _lock = match StartupLock::acquire(&dir) {
        Some(lock) => lock,
        None => match wait_for_lock_or_peer(&dir)? {
            Startup::Peer => return Ok(()),
            Startup::Lock(lock) => lock,
        },
    };
    if state_dir::read_server_file(&dir).is_some() {
        return Ok(());
    }

    // Reuse the recorded port and secret even after a clean shutdown, so an
    // open page reconnects and its cookie stays valid.
    let previous = state_dir::read_server_file_any(&dir);
    let secret = previous
        .as_ref()
        .map(|p| p.secret.clone())
        .unwrap_or_else(state_dir::new_secret);

    // A RAW listener, bound before the fork. A file descriptor survives fork;
    // a `tiny_http::Server` does not, because building one spawns an accept
    // thread and fork keeps only the calling thread.
    let listener = bind_preferring(args.port.or(previous.as_ref().map(|p| p.port))).context(
        "could not bind a loopback port; if this environment forbids listening sockets, \
         run `artefacto serve --foreground` in your own terminal",
    )?;
    let port = listener.local_addr()?.port();

    let readiness = if args.foreground {
        None
    } else {
        match daemon::daemonize(&dir.join("server.log"), &[listener.as_raw_fd()])? {
            // The parent's job is done the moment the grandchild says it is
            // serving. Returning here rather than falling through is the
            // whole point of the outcome type.
            ForkOutcome::Parent => return Ok(()),
            ForkOutcome::Child(readiness) => Some(readiness),
        }
    };

    // Everything below runs in the grandchild. Every failure between here and
    // accepting must be reported through `readiness`, or the parent sees only
    // EOF and can say nothing useful.
    let nudges = crate::server::presence::Nudges {
        idle: args.idle.0,
        away: args.away.0,
    };
    let started = start(&dir, listener, port, secret, nudges);
    let (shared, server) = match started {
        Ok(pair) => pair,
        Err(e) => match readiness {
            Some(r) => r.fail(&format!("{e:#}")),
            None => return Err(e),
        },
    };

    if let Some(r) = readiness {
        r.ready();
    }
    http::run(shared, server, SELF_EXIT);

    // `server.json` is deliberately left behind: it carries the port and
    // secret the next start reuses, and its now-dead pid already reads as
    // "no server".
    Ok(())
}

/// Record the server, then build the `tiny_http::Server` from the inherited
/// descriptor. Both steps are here so a failure in either reaches the pipe.
fn start(
    dir: &Path,
    listener: TcpListener,
    port: u16,
    secret: String,
    nudges: crate::server::presence::Nudges,
) -> Result<(Arc<Shared>, Arc<tiny_http::Server>)> {
    state_dir::write_server_file(
        dir,
        &ServerFile {
            pid: std::process::id(),
            port,
            secret: secret.clone(),
            started_at: now_rfc3339(),
        },
    )?;
    let shared = Arc::new(Shared::with_nudges(dir, secret, port, nudges)?);
    let server = tiny_http::Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("building the http server: {e}"))?;
    Ok((shared, Arc::new(server)))
}

/// Prefer the recorded port so an open page reconnects. If it is taken, fall
/// back to any free port; the page shows "restarted on a new port" once its
/// retries run out.
fn bind_preferring(preferred: Option<u16>) -> Result<TcpListener> {
    if let Some(p) = preferred {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", p)) {
            return Ok(l);
        }
    }
    TcpListener::bind(("127.0.0.1", 0)).context("binding 127.0.0.1")
}

enum Startup {
    /// Another process brought a server up while this one waited.
    Peer,
    /// The lock came free with no server recorded: this process starts one.
    Lock(StartupLock),
}

/// Someone else holds the startup lock: another `serve`, or a `clean`
/// rewriting the log. Wait briefly for either outcome — a server appears,
/// or the lock comes free — rather than racing, and rather than waiting
/// only for a server that a `clean` will never start.
fn wait_for_lock_or_peer(dir: &Path) -> Result<Startup> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if state_dir::read_server_file(dir).is_some() {
            return Ok(Startup::Peer);
        }
        if let Some(lock) = StartupLock::acquire(dir) {
            return Ok(Startup::Lock(lock));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("another artefacto holds the startup lock and did not release it within 10s")
}

pub fn stop() -> Result<()> {
    let dir = current_state_dir()?;
    let Some(server) = state_dir::read_server_file(&dir) else {
        // Already stopped is not a failure.
        return Ok(());
    };
    // Ask over the authenticated port. No blind SIGTERM fallback: a pid can be
    // reused, and signalling an unrelated process is worse than failing.
    request(&server, "stop").context("asking the server to stop")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if state_dir::read_server_file(&dir).is_none() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

pub fn status(json: bool) -> Result<()> {
    let dir = current_state_dir()?;
    let Some(server) = state_dir::read_server_file(&dir) else {
        if json {
            println!(
                "{}",
                serde_json::json!({ "ok": false, "error": { "code": "no_server" } })
            );
        } else {
            eprintln!("no server is running for this repository");
        }
        std::process::exit(EXIT_NO_SERVER);
    };
    let body = request(&server, "status")?;
    let mut value: serde_json::Value = serde_json::from_str(&body)?;
    // The state directory is useful enough for debugging to be part of the
    // output, and it is not a secret.
    value["state_dir"] = serde_json::json!(dir.to_string_lossy());
    if json {
        println!("{value}");
    } else {
        print!("{}", status_text(&value));
    }
    Ok(())
}

/// The JSON, as lines a person reads. Same facts, no token.
fn status_text(value: &serde_json::Value) -> String {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let mut out = format!(
        "port {}  last_seq {}  state {}\n",
        value["port"],
        value["last_seq"],
        s(&value["state_dir"])
    );
    let artifacts = value["artifacts"].as_array().cloned().unwrap_or_default();
    if artifacts.is_empty() {
        out.push_str("no artifacts yet; push a plan\n");
    }
    for a in &artifacts {
        out.push_str(&format!(
            "{}  \"{}\"  revision {}  threads: {} open, {} unanchored  submitted: {}\n",
            s(&a["id"]),
            s(&a["title"]),
            a["revision"],
            a["open_threads"],
            a["unanchored_threads"],
            if a["submitted"] == true { "yes" } else { "no" }
        ));
    }
    match value["lease"].as_object() {
        Some(lease) => out.push_str(&format!(
            "agent: {} ({}, {}s ago, acked {})\n",
            s(&lease["agent"]),
            s(&lease["mode"]),
            lease["age_secs"],
            lease["acked_seq"]
        )),
        None => out.push_str("agent: none\n"),
    }
    let reviewer = &value["reviewer"];
    out.push_str(&format!(
        "reviewer: {} page(s) open{}\n",
        reviewer["pages"],
        if reviewer["away"] == true {
            ", away"
        } else if reviewer["idle"] == true {
            ", idle"
        } else {
            ""
        }
    ));
    out.push_str(&format!("follow: {}\n", s(&value["follow"]["command"])));
    out
}

/// `artefacto open`: a fresh one-time link, and the browser on it.
///
/// The page sends a reviewer here when its link is spent or the server went
/// away, so this starts the server if none is running — the log is still
/// there after a self-exit, and a reviewer told to run `serve` first and then
/// `open` has been given two commands where one would do. It starts one only
/// when that log holds an artifact: with nothing ever pushed there is nothing
/// to open, and a daemon started just to say so would sit idle for half an
/// hour. A link is printed on stdout whatever else happens, so a caller who
/// cannot open a browser (an agent sandbox, a remote shell) still has it.
pub fn open(args: &OpenArgs) -> Result<()> {
    let dir = current_state_dir()?;
    if state_dir::read_server_file(&dir).is_none() && !crate::server::log::has_artifact(&dir) {
        return Err(crate::commands::Exit::new(
            2,
            "there is nothing to open yet; push a plan first",
        )
        .into());
    }
    serve(&ServeArgs {
        no_open: true,
        ..Default::default()
    })
    .context("starting the review server")?;
    let client = crate::client::Client::connect()?;
    let mut query = Vec::new();
    if let Some(artifact) = &args.artifact {
        query.push(("artifact", artifact.clone()));
    }
    let result = client.call("POST", "open", &query, Duration::from_secs(10))?;
    let url = result["url"].as_str().unwrap_or_default().to_string();
    if args.json {
        println!("{result}");
    } else {
        println!("{url}");
    }
    if crate::commands::plan::should_open(args.no_open, args.json) {
        crate::paths::open_browser(&url);
    }
    Ok(())
}

/// A one-shot authenticated GET. The CLI's whole client surface for now; it
/// stays here rather than pulling in an HTTP client crate for four lines.
fn request(server: &ServerFile, route: &str) -> Result<String> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", server.port))
        .with_context(|| format!("connecting to 127.0.0.1:{}", server.port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /cli/{route} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\
         Connection: close\r\n\r\n",
        server.port, server.secret
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    Ok(raw.rsplit("\r\n\r\n").next().unwrap_or("").to_string())
}

pub(crate) fn current_state_dir() -> Result<std::path::PathBuf> {
    let root = state_dir::repo_root(&std::env::current_dir()?)?;
    Ok(state_dir::state_dir(&root))
}
