//! Phase 3 acceptance: bridge composition, capability enforcement, envelopes.

mod support;

use std::process::Command;

use support::{binary, guest_wasm, repo_root, Server};

/// The composition-test bridge, or None when it has not been built.
fn bridge_wasm() -> Option<std::path::PathBuf> {
    let bridge = repo_root().join("testing/fake-bridge/bridge.wasm");
    if bridge.exists() {
        Some(bridge)
    } else {
        eprintln!("skipping: run testing/fake-bridge/build.sh");
        None
    }
}

/// Write a host.toml with an arbitrary `[bridges]` block and run `--check`.
fn check_with_bridges(bridges: &str) -> (bool, String) {
    let Some(guest) = guest_wasm() else {
        return (true, String::new());
    };

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("host.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "acceptance"
wasm  = "{}"
world = "server"

[runtime]
instances-min = 1
instances-max = 2

{bridges}
"#,
            guest.display()
        ),
    )
    .unwrap();

    let output = Command::new(binary())
        .arg("--check")
        .arg(&config_path)
        .output()
        .expect("binary runs");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// --- SRVH-01 / SRVH-02 -----------------------------------------------------

#[test]
fn an_unsatisfied_capability_refuses_startup_and_names_the_key() {
    // The acceptance guest imports clean:fake-bridge/store. With no [bridges]
    // entry the server must refuse rather than start with the capability
    // silently off — that failure would otherwise surface in production on a
    // code path staging never exercised (SRVH-02).
    if guest_wasm().is_none() {
        return;
    }
    let (ok, stderr) = check_with_bridges("");

    assert!(!ok, "startup should have been refused");
    assert!(stderr.contains("clean:fake-bridge/store"), "{stderr}");
    assert!(stderr.contains("[bridges]"), "{stderr}");
    assert!(stderr.contains("SRVH-02"), "{stderr}");
}

#[test]
fn a_configured_bridge_satisfies_the_capability() {
    let (Some(_guest), Some(bridge)) = (guest_wasm(), bridge_wasm()) else {
        return;
    };
    let (ok, stderr) = check_with_bridges(&format!(
        "[bridges]\n\"clean:fake-bridge/store\" = \"{}\"",
        bridge.display()
    ));
    assert!(ok, "composition should have succeeded:\n{stderr}");
}

#[test]
fn a_bridge_that_does_not_export_its_key_is_rejected() {
    // Pointing a [bridges] key at the wrong component would otherwise compose
    // silently and fail at the first call (CLNH-20).
    let (Some(guest), Some(_bridge)) = (guest_wasm(), bridge_wasm()) else {
        return;
    };
    let (ok, stderr) = check_with_bridges(&format!(
        "[bridges]\n\"clean:session/store\" = \"{}\"",
        guest.display()
    ));

    assert!(!ok);
    assert!(stderr.contains("does not export"), "{stderr}");
}

#[test]
fn a_missing_bridge_file_names_the_config_key() {
    if guest_wasm().is_none() {
        return;
    }
    let (ok, stderr) =
        check_with_bridges("[bridges]\n\"clean:fake-bridge/store\" = \"./nowhere.wasm\"");

    assert!(!ok);
    assert!(stderr.contains("[bridges]"), "{stderr}");
    assert!(stderr.contains("clean:fake-bridge/store"), "{stderr}");
}

#[test]
fn a_core_module_is_not_accepted_as_a_bridge() {
    // A core wasm module is not a component; composing one would fail much
    // later and much less legibly (CLNH-19).
    if guest_wasm().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("core.wasm");
    // Core module preamble: version 1, layer 0.
    std::fs::write(&fake, b"\0asm\x01\0\0\0").unwrap();

    let (ok, stderr) = check_with_bridges(&format!(
        "[bridges]\n\"clean:fake-bridge/store\" = \"{}\"",
        fake.display()
    ));

    assert!(!ok);
    assert!(
        stderr.contains("not a valid Component Model component"),
        "{stderr}"
    );
}

// --- composition actually wires --------------------------------------------

#[test]
fn the_guest_can_call_a_composed_bridge() {
    // The decisive test: /counter's body comes from the bridge, so a non-zero
    // digit proves the call reached across the composition boundary rather
    // than the server merely starting up.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, body) = server.request("GET", "/counter");

    assert_eq!(status, 200);
    assert_eq!(body, "1", "expected the bridge's counter value");
}

#[test]
fn bridge_state_does_not_leak_between_requests() {
    // Each request gets a fresh instance from the pool, so the composed
    // bridge's state resets. That is the isolation guarantee of §1.2, not a
    // bug: a counter that kept climbing would mean one request could observe
    // another's state.
    let Some(server) = Server::start_composed() else {
        return;
    };
    for _ in 0..3 {
        assert_eq!(server.request("GET", "/counter").2, "1");
    }
}

#[test]
fn composing_a_bridge_does_not_disturb_the_other_routes() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(server.request("GET", "/").2, "hello world");
    assert_eq!(server.request("GET", "/users/42").2, "42");
}

// --- CSRF (§1.7) -----------------------------------------------------------

#[test]
fn a_safe_method_never_needs_a_csrf_token() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(server.request("GET", "/").0, 200);
}

#[test]
fn a_post_is_allowed_when_no_token_has_been_issued() {
    // Nothing to forge against yet; rejecting here would break the first POST
    // of any session that never called set-csrf.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request_with_body("POST", "/echo", b"x");
    assert_eq!(status, 200);
}

#[test]
fn a_post_carrying_a_csrf_cookie_but_no_header_is_rejected() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) =
        server.request_with_headers("POST", "/echo", b"x", &[("Cookie", "__Host-csrf=secret")]);
    assert_eq!(status, 403, "a forged POST must not reach the guest");
}

#[test]
fn a_post_with_a_matching_csrf_token_is_allowed() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, body) = server.request_with_headers(
        "POST",
        "/echo",
        b"payload",
        &[("Cookie", "__Host-csrf=secret"), ("X-CSRF-Token", "secret")],
    );
    assert_eq!(status, 200);
    assert_eq!(body, "payload");
}

#[test]
fn a_post_with_a_mismatched_csrf_token_is_rejected() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request_with_headers(
        "POST",
        "/echo",
        b"x",
        &[("Cookie", "__Host-csrf=secret"), ("X-CSRF-Token", "wrong")],
    );
    assert_eq!(status, 403);
}

#[test]
fn a_token_prefix_is_not_accepted() {
    // Guards the constant-time comparison against a length-only check.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request_with_headers(
        "POST",
        "/echo",
        b"x",
        &[("Cookie", "__Host-csrf=secret"), ("X-CSRF-Token", "sec")],
    );
    assert_eq!(status, 403);
}
