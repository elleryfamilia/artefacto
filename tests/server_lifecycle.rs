//! `serve`, `stop`, `status`, and the daemon.
//!
//! The test that carries this file is `a_daemonized_server_actually_answers`.
//! Every other lifecycle test passes against a `--foreground` server, so a
//! daemon that starts and serves nothing hides from all of them. Two separate
//! reviews found exactly that bug in a plan for this code, in two different
//! forms: first a server built before the fork (no accept thread survives
//! `fork`), then a fork that closed the bound listener.

mod support;
use support::*;

#[test]
fn status_without_a_server_exits_4() {
    let repo = Repo::new();
    let out = repo.run(&["status", "--json"]);
    assert_eq!(out.code, 4, "agents branch on this code");
    assert!(out.stdout.contains("\"ok\":false"));
}

#[test]
fn a_daemonized_server_actually_answers() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let port = repo.port();

    let body = get(port, "/healthz");
    repo.stop();

    assert_eq!(
        status_of(&body),
        200,
        "the daemon must serve, not merely exist"
    );
    assert!(body.contains("\"ok\":true"));
}

#[test]
fn the_daemon_detaches_from_the_caller() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let pid = repo.pid();
    let ppid = parent_of(pid);
    repo.stop();

    assert_ne!(ppid, std::process::id(), "the daemon must not be our child");
    // On a machine with a subreaper the parent may not be pid 1, so the
    // binding assertion is "not us"; pid 1 is the common case.
    assert!(ppid <= 1 || !is_alive(ppid) || ppid != std::process::id());
}

#[test]
fn status_immediately_after_serve_is_not_a_race() {
    // `serve` returns only once the grandchild has signalled readiness over
    // the pipe. Without that, this is a coin flip.
    for _ in 0..10 {
        let repo = Repo::new();
        repo.run(&["serve", "--no-open"]).success();
        let out = repo.run(&["status", "--json"]);
        repo.stop();
        assert_eq!(out.code, 0, "status raced serve: {}", out.stderr);
    }
}

#[test]
fn serve_is_idempotent_while_a_server_is_live() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let first = repo.pid();
    repo.run(&["serve", "--no-open"]).success();
    let second = repo.pid();
    repo.stop();
    assert_eq!(
        first, second,
        "a second serve must not start a second daemon"
    );
}

#[test]
fn a_restart_rebinds_the_same_port_and_keeps_the_secret() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let (first_port, first_secret) = (repo.port(), repo.secret());
    repo.stop();

    repo.run(&["serve", "--no-open"]).success();
    let (second_port, second_secret) = (repo.port(), repo.secret());
    repo.stop();

    assert_eq!(
        first_port, second_port,
        "an open page must be able to reconnect"
    );
    assert_eq!(
        first_secret, second_secret,
        "and its cookie must still be valid"
    );
}

#[test]
fn stop_does_not_discard_the_port_and_secret() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    repo.stop();
    assert!(
        repo.server_json().exists(),
        "server.json carries what the next start reuses; its dead pid already reads as 'no server'"
    );
    assert!(repo.run(&["status", "--json"]).code == 4);
}

#[test]
fn a_dead_pid_in_the_file_lets_serve_start_a_real_server() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let live = repo.pid();
    repo.stop();

    // Rewrite the file pointing at a process that is gone.
    let raw = std::fs::read_to_string(repo.server_json()).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v["pid"] = serde_json::json!(0);
    std::fs::write(repo.server_json(), v.to_string()).unwrap();

    repo.run(&["serve", "--no-open"]).success();
    let fresh = repo.pid();
    repo.stop();
    assert_ne!(fresh, live);
}

#[test]
fn status_json_never_prints_the_secret() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let secret = repo.secret();
    let out = repo.run(&["status", "--json"]);
    repo.stop();
    assert!(
        !out.stdout.contains(&secret),
        "a credential must not be obtainable from something that only reads status"
    );
}

#[test]
fn two_concurrent_serves_start_one_daemon() {
    let repo = Repo::new();
    let mut a = repo.spawn(&["serve", "--no-open"]);
    let mut b = repo.spawn(&["serve", "--no-open"]);
    assert!(a.wait().expect("wait a").success());
    assert!(b.wait().expect("wait b").success());

    let pid = repo.pid();
    assert!(
        is_alive(pid),
        "the startup lock keeps them from racing into two daemons"
    );
    repo.stop();
    wait_for(|| !is_alive(pid), "the one daemon should stop");
}

#[test]
fn the_exact_loopback_host_is_required() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let port = repo.port();

    let good = get(port, "/healthz");
    let localhost = raw(
        port,
        &format!("GET /healthz HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"),
    );
    let foreign = raw(
        port,
        "GET /healthz HTTP/1.1\r\nHost: evil.example.com\r\nConnection: close\r\n\r\n",
    );
    repo.stop();

    assert_eq!(status_of(&good), 200);
    assert_eq!(
        status_of(&localhost),
        421,
        "localhost resolves to 127.0.0.1 on real machines, so this check is the only \
         thing rejecting a rebinding attacker"
    );
    assert_eq!(status_of(&foreign), 421);
}

#[test]
fn a_cli_route_needs_the_bearer_secret() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let port = repo.port();
    let secret = repo.secret();

    let without = get(port, "/cli/status");
    let wrong = raw(
        port,
        &format!(
            "GET /cli/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Authorization: Bearer 0000\r\nConnection: close\r\n\r\n"
        ),
    );
    let right = raw(
        port,
        &format!(
            "GET /cli/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Authorization: Bearer {secret}\r\nConnection: close\r\n\r\n"
        ),
    );
    repo.stop();

    assert_eq!(status_of(&without), 401);
    assert!(
        without.contains("\"code\":\"unauthorized\""),
        "errors are JSON, not HTML"
    );
    assert_eq!(status_of(&wrong), 401);
    assert_eq!(status_of(&right), 200);
}

#[test]
fn no_response_carries_cors_headers() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let port = repo.port();
    let mut seen = Vec::new();
    for path in ["/healthz", "/cli/status", "/nope"] {
        seen.push(raw(
            port,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                 Origin: http://evil.example.com\r\nConnection: close\r\n\r\n"
            ),
        ));
    }
    repo.stop();
    for r in seen {
        assert!(!r.to_ascii_lowercase().contains("access-control-allow"));
    }
}

#[test]
fn an_unknown_route_is_a_json_404() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let r = get(repo.port(), "/nope");
    repo.stop();
    assert_eq!(status_of(&r), 404);
    assert!(r.contains("\"ok\":false"));
}
