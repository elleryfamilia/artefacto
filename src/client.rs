//! The CLI's side of the loopback conversation.
//!
//! Four lines of HTTP rather than a client crate: one connection per call,
//! `Connection: close`, and a bearer secret read from a file only the user can
//! read. Nothing here is a general HTTP client and nothing should make it one.
//!
//! Two things it does carry:
//!
//! - **Exit codes are part of the contract.** An agent branches on them, so
//!   "no server" is 4 and a lease refusal is 6, and both come back as an
//!   [`Exit`] the caller propagates rather than as prose on stderr.
//! - **`await` reconnects on its own** (spec 5), so a caller can ask for a
//!   call to be retried until an absolute deadline instead of surfacing a
//!   dropped connection as a failure.

use crate::commands::Exit;
use crate::server::state_dir::{self, ServerFile};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// Exit 4: there is no server. Agents branch on it.
pub const EXIT_NO_SERVER: i32 = 4;
/// Exit 6: the lease is held by another agent, or this token was superseded.
pub const EXIT_LEASE: i32 = 6;

pub struct Client {
    server: ServerFile,
}

impl Client {
    /// The running server for the current repository, or exit 4.
    pub fn connect() -> Result<Client> {
        let dir = state_dir::state_dir(&state_dir::repo_root(&std::env::current_dir()?)?);
        match state_dir::read_server_file(&dir) {
            Some(server) => Ok(Client { server }),
            None => Err(Exit::new(
                EXIT_NO_SERVER,
                "no server is running for this repository; run `artefacto serve`",
            )
            .into()),
        }
    }

    pub fn port(&self) -> u16 {
        self.server.port
    }

    /// One authenticated request. `read_timeout` bounds the wait, so a long
    /// poll has to be given room for its own deadline plus slack.
    pub fn call(
        &self,
        method: &str,
        route: &str,
        query: &[(&str, String)],
        read_timeout: Duration,
    ) -> Result<serde_json::Value> {
        let target = format!("/cli/{route}{}", query_string(query));
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.server.port))
            .with_context(|| format!("connecting to 127.0.0.1:{}", self.server.port))?;
        stream.set_read_timeout(Some(read_timeout))?;
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n",
            self.server.port, self.server.secret
        )?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let body = raw
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(body).with_context(|| {
            format!("the server answered with something that is not JSON: {raw}")
        })?;
        check(value)
    }

    /// Retry a call until `deadline`, treating a dropped connection as
    /// something to try again rather than as a failure.
    ///
    /// Spec 5: `await` "reconnects on its own: if the connection drops or the
    /// server restarts mid-wait, it retries against the same cursor until its
    /// absolute deadline". A refusal is **not** retried — a held lease will
    /// still be held a second later, and hammering it is worse than saying so.
    pub fn call_until(
        &self,
        method: &str,
        route: &str,
        query: &[(&str, String)],
        deadline: Instant,
    ) -> Result<serde_json::Value> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let attempt = self.call(method, route, query, remaining + Duration::from_secs(5));
            match attempt {
                Ok(value) => return Ok(value),
                Err(e) if e.downcast_ref::<Exit>().is_some() => return Err(e),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
    }
}

/// Turn the server's error body into the exit code its caller owes the agent.
fn check(value: serde_json::Value) -> Result<serde_json::Value> {
    if value.get("ok").and_then(|o| o.as_bool()) != Some(false) {
        return Ok(value);
    }
    let code = value
        .pointer("/error/code")
        .and_then(|c| c.as_str())
        .unwrap_or("error");
    let message = value
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .unwrap_or("the server refused the call");
    let exit = match code {
        "lease_held" | "lease_superseded" => EXIT_LEASE,
        _ => 2,
    };
    Err(Exit::new(exit, message).into())
}

fn query_string(pairs: &[(&str, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect();
    format!("?{}", body.join("&"))
}

/// Everything outside the unreserved set is escaped. Agent names are the only
/// field here a person chooses, and one with a space or an ampersand in it
/// must not be able to forge a second parameter.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_escapes_everything_a_name_could_smuggle() {
        let q = query_string(&[("agent", "my agent&takeover=1".to_string())]);
        assert_eq!(q, "?agent=my%20agent%26takeover%3D1");
        assert!(
            !q.contains("&takeover"),
            "a crafted agent name must not forge a second parameter"
        );
    }

    #[test]
    fn a_lease_refusal_becomes_exit_6() {
        let body = serde_json::json!({
            "ok": false,
            "error": { "code": "lease_held", "message": "held by claude" }
        });
        let err = check(body).expect_err("a refusal is an error");
        let exit = err.downcast_ref::<Exit>().expect("an exit code");
        assert_eq!(exit.code, EXIT_LEASE);
        assert!(exit.to_string().contains("claude"));
    }

    #[test]
    fn an_ok_body_passes_through() {
        let body = serde_json::json!({ "ok": true, "status": "timeout" });
        assert_eq!(check(body.clone()).unwrap(), body);
    }
}
