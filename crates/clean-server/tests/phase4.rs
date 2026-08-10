//! Phase 4 acceptance: reload triggers, admin API auth, dev socket.
//!
//! Reload is the operation most likely to take a production process down if it
//! goes wrong, so these tests care more about what happens when it fails than
//! when it succeeds.

mod support;

use std::time::Duration;

use support::Server;

/// Give a signal-driven or socket-driven reload time to land.
fn settle() {
    std::thread::sleep(Duration::from_millis(600));
}

// --- SIGHUP ----------------------------------------------------------------

#[cfg(unix)]
#[test]
fn sighup_reloads_without_interrupting_service() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(server.request("GET", "/").2, "hello world");

    server.signal(libc_sighup());
    settle();

    // The point of reload: the process keeps serving across it.
    let (status, _, body) = server.request("GET", "/");
    assert_eq!(status, 200);
    assert_eq!(body, "hello world");
}

#[cfg(unix)]
#[test]
fn repeated_sighups_are_survivable() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    for _ in 0..3 {
        server.signal(libc_sighup());
        settle();
    }
    assert_eq!(server.request("GET", "/").0, 200);
}

#[cfg(unix)]
fn libc_sighup() -> i32 {
    1
}

// --- admin API -------------------------------------------------------------

#[test]
fn the_admin_api_rejects_an_unauthenticated_reload() {
    // SRVH-08. An unauthenticated reload endpoint is a remote restart button.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.admin_post(r#"{"op":"reload-guest"}"#, None);
    assert_eq!(status, 401);
}

#[test]
fn the_admin_api_rejects_a_wrong_token() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.admin_post(r#"{"op":"reload-guest"}"#, Some("not-the-right-token"));
    assert_eq!(status, 401);
}

#[test]
fn the_admin_api_rejects_a_token_prefix() {
    // Guards the constant-time comparison against a length-only check.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.admin_post(r#"{"op":"reload-guest"}"#, Some("0123456789"));
    assert_eq!(status, 401);
}

#[test]
fn an_authenticated_reload_succeeds_and_acks() {
    // SRVH-07: every op is answered — the caller needs the ack to know the
    // reload landed.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, body) =
        server.admin_post(r#"{"op":"reload-guest"}"#, Some(Server::ADMIN_TOKEN));

    assert_eq!(status, 200);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON ack");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["op"], "reload-guest");
    assert!(parsed["duration-ms"].is_number());
}

#[test]
fn the_server_keeps_serving_across_an_admin_reload() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (_, _, _) = server.admin_post(r#"{"op":"reload-guest"}"#, Some(Server::ADMIN_TOKEN));
    settle();
    assert_eq!(server.request("GET", "/").2, "hello world");
}

#[test]
fn reload_chain_is_accepted() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (_, _, body) = server.admin_post(r#"{"op":"reload-chain"}"#, Some(Server::ADMIN_TOKEN));
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "ok");
}

#[test]
fn swap_middleware_is_refused_with_a_reason() {
    // SRVH-06 plus honesty: there is no [http-chain] to mutate, and reporting
    // success would claim a swap that never happened.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, body) = server.admin_post(
        r#"{"op":"swap-middleware","target":{"index":0},"replacement":"./x.wasm"}"#,
        Some(Server::ADMIN_TOKEN),
    );

    assert_eq!(status, 200, "a refusal is still a protocol answer");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "refused");
    assert!(parsed["reason"].is_string());
}

#[test]
fn an_unknown_op_is_an_error_not_a_crash() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (_, _, body) = server.admin_post(r#"{"op":"bogus"}"#, Some(Server::ADMIN_TOKEN));
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "error");
    assert!(
        parsed["error-message"]
            .as_str()
            .unwrap()
            .contains("unknown op"),
        "{body}"
    );
}

#[test]
fn malformed_json_is_an_error_not_a_crash() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (_, _, body) = server.admin_post("{not json", Some(Server::ADMIN_TOKEN));
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "error");
}

#[test]
fn reloading_a_different_guest_path_is_refused_rather_than_ignored() {
    // Accepting and ignoring the path would silently reload the wrong artifact,
    // which is worse than saying no.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (_, _, body) = server.admin_post(
        r#"{"op":"reload-guest","guest":"./somewhere-else.wasm"}"#,
        Some(Server::ADMIN_TOKEN),
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "error");
}

#[test]
fn the_admin_api_serves_only_its_own_path() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.admin_get("/");
    assert_eq!(status, 404);
}

#[test]
fn the_admin_api_requires_post() {
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.admin_get("/_admin/reload");
    assert_eq!(status, 405);
}

#[test]
fn the_main_listener_does_not_expose_the_admin_endpoint() {
    // The admin API lives on its own listener precisely so it is not reachable
    // from the public surface.
    let Some(server) = Server::start_with_admin() else {
        return;
    };
    let (status, _, _) = server.request("POST", "/_admin/reload");
    assert_ne!(status, 200, "admin must not be reachable on the main port");
}

// --- dev socket ------------------------------------------------------------

#[cfg(unix)]
#[test]
fn the_dev_socket_answers_without_authentication() {
    // SRVH-08 explicitly forbids requiring auth here; access control is the
    // socket's filesystem permissions.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let Some(response) = server.dev_socket_request(r#"{"op":"reload-guest"}"#) else {
        return;
    };

    let parsed: serde_json::Value = serde_json::from_str(&response).expect("JSON ack");
    assert_eq!(parsed["status"], "ok");
}

#[cfg(unix)]
#[test]
fn the_dev_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let Some(server) = Server::start_composed() else {
        return;
    };
    let Some(path) = server.dev_socket_path() else {
        return;
    };

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "the socket IS the access control; group/other access would let any \
         local user trigger a reload"
    );
}

#[cfg(unix)]
#[test]
fn the_dev_socket_is_removed_on_shutdown() {
    // A file left behind makes the next start trip over a stale socket.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let Some(path) = server.dev_socket_path() else {
        return;
    };
    assert!(path.exists());

    // SIGTERM, not the Drop SIGKILL: cleanup cannot run on a killed process,
    // and the guarantee under test is what a supervisor's stop does.
    server.shutdown_gracefully();
    std::thread::sleep(Duration::from_millis(300));

    assert!(!path.exists(), "stale socket left at {}", path.display());
}
