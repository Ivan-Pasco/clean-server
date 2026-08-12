//! M0 acceptance, automated (PLAN.md Layer B).
//!
//! Boots the real binary and drives it with a real HTTP client. This is the
//! test that proves the whole path — config, composition, route registration,
//! request marshaling, guest invocation, response marshaling — works end to
//! end.
//!
//! The guest is `testing/fake-guest/guest.wasm`, built by its `build.sh` from
//! a world that imports the same interfaces `host.wit` declares.

mod support;

use std::process::Command;

use support::{binary, guest_wasm, header, repo_root, Server};

#[test]
fn m0_acceptance_get_root_returns_hello_world() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, body) = server.request("GET", "/");

    assert_eq!(status, 200);
    assert_eq!(body, "hello world");
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/plain; charset=utf-8")
    );
}

#[test]
fn unregistered_path_is_404() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, _, _) = server.request("GET", "/nothing-here");
    assert_eq!(status, 404);
}

#[test]
fn wrong_method_is_405_with_an_allow_header() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, _) = server.request("POST", "/");
    assert_eq!(status, 405);
    assert_eq!(header(&headers, "allow"), Some("GET"));
}

#[test]
fn head_returns_headers_without_a_body() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, body) = server.request("HEAD", "/");
    assert_eq!(status, 200);
    assert!(body.is_empty(), "HEAD must not carry a body, got {body:?}");
    // The length still describes what a GET would return.
    assert_eq!(header(&headers, "content-length"), Some("11"));
}

#[test]
fn the_same_instance_serves_repeated_requests_cleanly() {
    // Instances are reset and returned to the pool between requests
    // (CLNH-29). If reset were broken, a later response would differ.
    let Some(server) = Server::start() else {
        return;
    };
    for _ in 0..10 {
        let (status, _, body) = server.request("GET", "/");
        assert_eq!(status, 200);
        assert_eq!(body, "hello world");
    }
}

#[test]
fn missing_guest_wasm_fails_startup_with_a_pointed_error() {
    // §8 question #7: a missing guest is a startup error naming the config key
    // and the resolved path — never a silent fallback (CH-05).
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("host.toml");
    std::fs::write(
        &config_path,
        r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "missing"
wasm  = "./does-not-exist.wasm"
world = "server"
"#,
    )
    .unwrap();

    let output = Command::new(binary())
        .arg("--check")
        .arg(&config_path)
        .output()
        .expect("binary runs");

    assert!(!output.status.success(), "startup should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[guest] wasm"), "{stderr}");
    assert!(stderr.contains("does-not-exist.wasm"), "{stderr}");
}

#[test]
fn a_bridge_pointing_at_a_missing_file_names_the_config_key() {
    // Phase 3 composes bridges, so a `[bridges]` entry is no longer refused
    // outright — but a path that does not resolve is still a startup error,
    // and the diagnostic must name the key so the operator can fix the line
    // rather than guess which of several bridges is wrong (CLNH-18).
    let Some(guest) = guest_wasm() else {
        return;
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

[bridges]
"clean:session/store" = "./nowhere.wasm"
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

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[bridges]"), "{stderr}");
    assert!(stderr.contains("clean:session/store"), "{stderr}");
}

#[test]
fn hcv_06_parity_holds_between_host_wit_and_the_linker() {
    // The same check CI runs (§16.14). Declared interfaces must match
    // registered ones exactly, in both directions.
    let output = Command::new(binary())
        .arg("parity")
        .arg("--wit")
        .arg(repo_root().join("host.wit"))
        .output()
        .expect("binary runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "HCV-06 parity failed:\n{stdout}");
}
