//! A real browser, driven over the Chrome DevTools Protocol.
//!
//! # Why not `--dump-dom`
//!
//! Rosita's browser smokes ran over `file://` with `--dump-dom`, and their
//! comments record why that could not be reused: a page served over a real
//! socket plus a virtual time budget makes Chrome dump an empty document in
//! CI. This harness never uses virtual time. It launches a headless Chromium
//! with a DevTools port, connects to the page's socket, and evaluates
//! JavaScript in the page with real waits. tungstenite is already a
//! dependency, so there is nothing new to build.
//!
//! # What it proves that a fake client cannot
//!
//! A fake WebSocket client proves the server's half. Only a browser proves
//! the page's: that the nonce CSP lets the script run, that the cookie set on
//! the bootstrap redirect reaches the socket handshake, that a frame becomes
//! something a reviewer sees.
//!
//! # Skipping
//!
//! Without a Chromium the tests print a skip line and pass, so a machine
//! without a browser still runs the rest of the suite. `ARTEFACTO_CHROME`
//! names a binary explicitly; `ARTEFACTO_REQUIRE_BROWSER=1` turns absence
//! into a failure, for the machines where the browser suite must run.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Where a Chromium may be. The Playwright caches are searched by glob
/// because their directory names carry a build number.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(explicit) = std::env::var("ARTEFACTO_CHROME") {
        out.push(PathBuf::from(explicit));
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for cache in [
        format!("{home}/Library/Caches/ms-playwright"),
        format!("{home}/.cache/ms-playwright"),
    ] {
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort();
        dirs.reverse();
        for dir in dirs {
            let name = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name.starts_with("chromium_headless_shell-") {
                out.push(dir.join("chrome-headless-shell-mac-arm64/chrome-headless-shell"));
                out.push(dir.join("chrome-headless-shell-mac-x64/chrome-headless-shell"));
                out.push(dir.join("chrome-headless-shell-linux64/chrome-headless-shell"));
                out.push(dir.join("chrome-linux/headless_shell"));
            } else if name.starts_with("chromium-") {
                out.push(dir.join(
                    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                ));
                out.push(dir.join("chrome-mac/Chromium.app/Contents/MacOS/Chromium"));
                out.push(dir.join("chrome-linux/chrome"));
            }
        }
    }
    // On Linux, Google Chrome before the distribution's `chromium-browser`:
    // on Ubuntu that name is a shell script that hands off to snap, which
    // is not there on a CI runner, so it exits without ever opening a port.
    // `is_browser` rejects such a shim by its contents whatever the order.
    for fixed in [
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
    ] {
        out.push(PathBuf::from(fixed));
    }
    out
}

/// A file that is a browser, not a shell script standing in for one. A
/// script whose first bytes are `#!` and that mentions snap is Ubuntu's
/// transitional `chromium-browser`, which starts nothing here.
fn is_browser(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 512];
    let n = file.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    !(head.starts_with(b"#!") && String::from_utf8_lossy(head).contains("snap"))
}

pub fn find_chromium() -> Option<PathBuf> {
    candidates().into_iter().find(|p| is_browser(p))
}

pub struct Browser {
    child: Child,
    /// Profile directory; dropped after the process, which is why it is
    /// declared after `child`.
    user_data: tempfile::TempDir,
    pub port: u16,
    binary: PathBuf,
}

impl Browser {
    /// `None` when no Chromium is installed (and none is required).
    pub fn launch() -> Option<Browser> {
        let Some(binary) = find_chromium() else {
            if std::env::var("ARTEFACTO_REQUIRE_BROWSER").as_deref() == Ok("1") {
                panic!("ARTEFACTO_REQUIRE_BROWSER is set and no Chromium was found");
            }
            eprintln!("artefacto browser test: no Chromium found, skipping");
            return None;
        };
        let user_data = tempfile::tempdir().expect("a profile directory");
        let log = std::fs::File::create(user_data.path().join("chrome.log")).expect("log");
        let headless = if binary.to_string_lossy().contains("headless") {
            "--headless"
        } else {
            "--headless=new"
        };
        let mut args = vec![
            headless.to_string(),
            "--remote-debugging-port=0".to_string(),
            format!("--user-data-dir={}", user_data.path().display()),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-extensions".to_string(),
            "--disable-gpu".to_string(),
            "--window-size=1280,900".to_string(),
        ];
        // A CI runner is a disposable machine, and recent Ubuntu images
        // restrict the unprivileged user namespaces Chrome's sandbox needs;
        // there, and only there, the sandbox is off. `CI` is set by GitHub
        // Actions and most other runners.
        if std::env::var("CI").is_ok() {
            args.push("--no-sandbox".to_string());
            args.push("--disable-dev-shm-usage".to_string());
        }
        args.push("about:blank".to_string());
        let child = Command::new(&binary)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap_or_else(|e| panic!("launching {}: {e}", binary.display()));
        let port_file = user_data.path().join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(20);
        let port = loop {
            if let Ok(text) = std::fs::read_to_string(&port_file) {
                if let Some(port) = text
                    .lines()
                    .next()
                    .and_then(|l| l.trim().parse::<u16>().ok())
                {
                    break port;
                }
            }
            if Instant::now() >= deadline {
                // The log is in a temp directory the job throws away, so
                // its tail goes into the failure where a reader can see it.
                let log_text = std::fs::read_to_string(user_data.path().join("chrome.log"))
                    .unwrap_or_default();
                let tail: Vec<&str> = log_text.lines().rev().take(20).collect();
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                panic!(
                    "{} did not open a DevTools port within 20s; the end of its log:\n{}",
                    binary.display(),
                    tail.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        Some(Browser {
            child,
            user_data,
            port,
            binary,
        })
    }

    fn devtools(&self, method: &str, path: &str) -> String {
        let raw = super::raw(
            self.port,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                self.port
            ),
        );
        raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
    }

    /// A fresh tab. Each tab is its own DevTools target with its own socket,
    /// so two tabs on one artifact are two independent pages, as they are
    /// for a reviewer.
    pub fn new_page(&self) -> Page {
        // Chrome wants PUT here since 111; older builds accept GET.
        let mut body = self.devtools("PUT", "/json/new?about:blank");
        if !body.trim_start().starts_with('{') {
            body = self.devtools("GET", "/json/new?about:blank");
        }
        let target: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("/json/new: {e}\n{body}"));
        let url = target["webSocketDebuggerUrl"]
            .as_str()
            .expect("a page target has a debugger url");
        Page::connect(url)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.user_data.path().join("chrome.log")).unwrap_or_default()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One tab, spoken to over its DevTools socket.
pub struct Page {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    next_id: u64,
    /// Console errors, uncaught exceptions, and browser log entries at error
    /// level — a CSP refusal lands here — collected while waiting on calls.
    errors: Vec<String>,
    /// Every console message, for diagnosing a failed wait.
    console: Vec<String>,
}

impl Page {
    fn connect(url: &str) -> Page {
        let (ws, _) = tungstenite::connect(url).expect("connect to the page's DevTools socket");
        let mut page = Page {
            ws,
            next_id: 1,
            errors: Vec::new(),
            console: Vec::new(),
        };
        page.set_timeout(Duration::from_secs(20));
        page.call("Runtime.enable", serde_json::json!({}));
        page.call("Log.enable", serde_json::json!({}));
        page.call("Page.enable", serde_json::json!({}));
        // A headless page has no window focus, so `element.focus()` would
        // not make it the active element and every caret assertion would
        // be about the harness rather than the page.
        page.call(
            "Emulation.setFocusEmulationEnabled",
            serde_json::json!({ "enabled": true }),
        );
        page
    }

    fn set_timeout(&mut self, d: Duration) {
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = self.ws.get_mut() {
            let _ = s.set_read_timeout(Some(d));
        }
    }

    /// One protocol call. Events that arrive while waiting are recorded.
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({ "id": id, "method": method, "params": params });
        self.ws
            .send(tungstenite::Message::text(msg.to_string()))
            .expect("send to devtools");
        loop {
            let text = match self.ws.read() {
                Ok(tungstenite::Message::Text(t)) => t.to_string(),
                Ok(_) => continue,
                Err(e) => panic!("devtools socket while waiting for {method}: {e}"),
            };
            let v: serde_json::Value = serde_json::from_str(&text).expect("devtools json");
            if v["id"] == id {
                if let Some(err) = v.get("error") {
                    panic!("{method} failed: {err}");
                }
                return v["result"].clone();
            }
            self.record_event(&v);
        }
    }

    fn record_event(&mut self, v: &serde_json::Value) {
        match v["method"].as_str().unwrap_or_default() {
            "Runtime.exceptionThrown" => {
                let d = &v["params"]["exceptionDetails"];
                let text = d["exception"]["description"]
                    .as_str()
                    .or(d["text"].as_str())
                    .unwrap_or("exception")
                    .to_string();
                self.errors.push(format!("exception: {text}"));
            }
            "Runtime.consoleAPICalled" => {
                let kind = v["params"]["type"].as_str().unwrap_or_default().to_string();
                let text = v["params"]["args"]
                    .as_array()
                    .map(|args| {
                        args.iter()
                            .map(|a| {
                                a["value"].as_str().map(str::to_string).unwrap_or_else(|| {
                                    a["description"].as_str().unwrap_or("?").to_string()
                                })
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                self.console.push(format!("{kind}: {text}"));
                if kind == "error" {
                    self.errors.push(format!("console.error: {text}"));
                }
            }
            "Log.entryAdded" => {
                let e = &v["params"]["entry"];
                let line = format!(
                    "{}/{}: {}",
                    e["source"].as_str().unwrap_or_default(),
                    e["level"].as_str().unwrap_or_default(),
                    e["text"].as_str().unwrap_or_default()
                );
                self.console.push(line.clone());
                if e["level"] == "error" {
                    self.errors.push(line);
                }
            }
            _ => {}
        }
    }

    /// Load `url` and wait for the document to finish loading.
    pub fn navigate(&mut self, url: &str) {
        self.call("Page.navigate", serde_json::json!({ "url": url }));
        self.wait_until("document.readyState === 'complete'", "the page loads");
    }

    /// Evaluate in the page. A promise is awaited; a throw is a panic that
    /// names the expression.
    pub fn eval(&mut self, expression: &str) -> serde_json::Value {
        let r = self.call(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
            }),
        );
        if let Some(d) = r.get("exceptionDetails") {
            let text = d["exception"]["description"]
                .as_str()
                .or(d["text"].as_str())
                .unwrap_or("threw");
            panic!("evaluating {expression:?}: {text}");
        }
        r["result"]["value"].clone()
    }

    /// The value of `expression` as text, for assertions on what is shown.
    pub fn text(&mut self, expression: &str) -> String {
        self.eval(&format!("String({expression})"))
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Poll until `expression` is truthy. Bounded, and the failure names what
    /// was waited for, along with the console, so a hang reads as a reason.
    pub fn wait_until(&mut self, expression: &str, what: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let v = self.eval(expression);
            let truthy = match &v {
                serde_json::Value::Null => false,
                serde_json::Value::Bool(b) => *b,
                serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
                serde_json::Value::String(s) => !s.is_empty(),
                _ => true,
            };
            if truthy {
                return v;
            }
            if Instant::now() > deadline {
                let state = self.eval(
                    "window.artefactoPlan && window.artefactoPlan.debug ? JSON.stringify(window.artefactoPlan.debug()) : null",
                );
                panic!(
                    "timed out waiting for {what} ({expression})\npage state: {state}\nconsole:\n{}",
                    self.console.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Pull in any events the browser has queued, without changing anything.
    pub fn sync(&mut self) {
        self.eval("0");
    }

    /// Errors seen so far: uncaught exceptions, `console.error`, and
    /// error-level browser log entries such as a CSP refusal.
    pub fn errors(&mut self) -> Vec<String> {
        self.sync();
        self.errors.clone()
    }

    pub fn console(&mut self) -> Vec<String> {
        self.sync();
        self.console.clone()
    }

    /// Simulate a click on the first element matching `selector`, through the
    /// element's own click so every listener on the path runs.
    pub fn click(&mut self, selector: &str) {
        let hit = self.eval(&format!(
            "(function(){{ const el = document.querySelector({sel}); if (!el) return false; el.click(); return true; }})()",
            sel = serde_json::to_string(selector).unwrap()
        ));
        assert_eq!(hit, true, "no element matches {selector}");
    }

    /// Put `value` into a text control and fire the events a keyboard would.
    pub fn type_into(&mut self, selector: &str, value: &str) {
        let ok = self.eval(&format!(
            "(function(){{ const el = document.querySelector({sel}); if (!el) return false; \
             el.focus(); el.value = {val}; \
             el.dispatchEvent(new Event('input', {{ bubbles: true }})); \
             el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()",
            sel = serde_json::to_string(selector).unwrap(),
            val = serde_json::to_string(value).unwrap()
        ));
        assert_eq!(ok, true, "no element matches {selector}");
    }

    /// A full-page PNG, for looking at what a reviewer would see.
    pub fn screenshot(&mut self, path: &std::path::Path) {
        let r = self.call(
            "Page.captureScreenshot",
            serde_json::json!({ "format": "png", "captureBeyondViewport": true }),
        );
        let data = r["data"].as_str().expect("screenshot data");
        let bytes = base64_decode(data);
        std::fs::write(path, bytes).expect("write screenshot");
    }

    /// Forget every cookie, so the next request is not signed in.
    pub fn clear_cookies(&mut self) {
        self.call("Network.enable", serde_json::json!({}));
        self.call("Network.clearBrowserCookies", serde_json::json!({}));
    }

    /// Cut the page's network from underneath it, to test reconnecting.
    pub fn set_offline(&mut self, offline: bool) {
        self.call("Network.enable", serde_json::json!({}));
        self.call(
            "Network.emulateNetworkConditions",
            serde_json::json!({
                "offline": offline, "latency": 0,
                "downloadThroughput": -1, "uploadThroughput": -1,
            }),
        );
    }
}

/// Unused by the harness itself; keeps `Read`/`Write` in scope for callers
/// that need to speak raw HTTP to the DevTools endpoint.
fn _keep_traits(_: &dyn Read, _: &dyn Write) {}

/// Standard base64, enough for a screenshot payload; no dependency needed.
fn base64_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, c) in ALPHABET.iter().enumerate() {
        lookup[*c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in text.bytes() {
        let v = lookup[b as usize];
        if v == 255 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}
