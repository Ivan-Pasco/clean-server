//! Phase 6 acceptance: CSRF opt-out, escaping pass-through, plaintext policy,
//! and the CMOD-03 conformance gate.

mod support;

use std::process::Command;

use support::{binary, guest_wasm, repo_root, Server};

// --- per-route CSRF opt-out (§1.7) -----------------------------------------

/// A CSRF cookie without the matching header — a forged POST.
const FORGED: &[(&str, &str)] = &[("Cookie", "__Host-csrf=secret")];

#[test]
fn a_protected_route_still_rejects_a_forged_post() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request_with_headers("POST", "/echo", b"x", FORGED);
    assert_eq!(status, 403);
}

#[test]
fn a_route_that_opted_out_accepts_the_same_request() {
    // `/hook` registers with `csrf = false`: a webhook verifying an HMAC has
    // no token to present and no browser form to protect.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, body) =
        server.request_with_headers("POST", "/hook", b"webhook payload", FORGED);

    assert_eq!(status, 200);
    assert_eq!(body, "webhook payload", "the request must reach the guest");
}

#[test]
fn opting_one_route_out_does_not_weaken_the_others() {
    // The failure that would matter: an opt-out leaking across routes.
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(
        server.request_with_headers("POST", "/hook", b"x", FORGED).0,
        200
    );
    assert_eq!(
        server.request_with_headers("POST", "/echo", b"x", FORGED).0,
        403,
        "a neighbouring route must stay protected"
    );
}

#[test]
fn an_opted_out_route_still_accepts_a_valid_token() {
    // Opting out means "do not require", not "reject if present".
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request_with_headers(
        "POST",
        "/hook",
        b"x",
        &[("Cookie", "__Host-csrf=secret"), ("X-CSRF-Token", "secret")],
    );
    assert_eq!(status, 200);
}

// --- escaping pass-through -------------------------------------------------

#[test]
fn the_server_does_not_unescape_a_guest_response_body() {
    // PLAN.md Phase 6: escaping is the framework's job; the server's
    // obligation is to not undo it. A body that arrives escaped must leave
    // escaped, byte for byte.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let escaped = b"&lt;script&gt;alert(1)&lt;/script&gt;";
    let (status, _, body) = server.request_with_body("POST", "/echo", escaped);

    assert_eq!(status, 200);
    assert_eq!(
        body.as_bytes(),
        escaped,
        "the server must not decode entities on the way out"
    );
}

#[test]
fn the_server_does_not_escape_a_body_the_guest_meant_literally() {
    // The inverse error: escaping on the server's own initiative would corrupt
    // a guest serving JSON or binary content.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let raw = br#"{"html":"<b>bold</b>","amp":"a&b"}"#;
    let (_, _, body) = server.request_with_body("POST", "/echo", raw);

    assert_eq!(body.as_bytes(), raw, "the body must pass through verbatim");
}

#[test]
fn a_response_body_survives_bytes_that_are_not_utf8_text() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    // Control bytes and high-bit bytes that a naive transform would mangle.
    let raw: Vec<u8> = vec![0x01, 0x22, 0x5c, 0x7f, 0x41, 0x42];
    let (status, _, body) = server.request_with_body("POST", "/echo", &raw);

    assert_eq!(status, 200);
    assert_eq!(body.as_bytes(), raw.as_slice());
}

// --- plaintext policy (§1.7) ------------------------------------------------

/// Run `--check` against a config and return (succeeded, stderr).
fn check_config(host_block: &str, server_block: &str) -> (bool, String) {
    let Some(guest) = guest_wasm() else {
        return (true, String::new());
    };
    let bridge = repo_root().join("testing/fake-bridge/bridge.wasm");

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
{host_block}

[guest]
name  = "acceptance"
wasm  = "{}"
world = "server"

[runtime]
instances-min = 1
instances-max = 2

[bridges]
"clean:fake-bridge/store" = "{}"

{server_block}
"#,
            guest.display(),
            bridge.display()
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

#[test]
fn production_plaintext_is_refused_without_an_explicit_opt_in() {
    // §1.7. `cln doctor` warns about the opt-in; the server's own job is to
    // refuse when nobody has taken it.
    if guest_wasm().is_none() {
        return;
    }
    let (ok, stderr) = check_config("deployment-mode = \"production\"", "[server]");

    assert!(!ok, "production plaintext should have been refused");
    assert!(stderr.contains("plaintext"), "{stderr}");
    // The diagnostic must name every way out, or an operator is left guessing.
    assert!(stderr.contains("allow-plaintext"), "{stderr}");
    assert!(stderr.contains("trust-proxy-headers"), "{stderr}");
}

#[test]
fn production_plaintext_is_allowed_once_opted_in() {
    if guest_wasm().is_none() {
        return;
    }
    let (ok, stderr) = check_config(
        "deployment-mode = \"production\"",
        "[server]\nallow-plaintext = true",
    );
    assert!(ok, "an explicit opt-in must be honoured:\n{stderr}");
}

#[test]
fn a_terminating_proxy_satisfies_the_plaintext_rule() {
    if guest_wasm().is_none() {
        return;
    }
    let (ok, stderr) = check_config(
        "deployment-mode = \"production\"",
        "[server]\ntrust-proxy-headers = true",
    );
    assert!(
        ok,
        "a TLS-terminating proxy is not plaintext exposure:\n{stderr}"
    );
}

#[test]
fn development_plaintext_needs_no_opt_in() {
    if guest_wasm().is_none() {
        return;
    }
    let (ok, _) = check_config("deployment-mode = \"development\"", "[server]");
    assert!(ok);
}

// --- CMOD-03 conformance gate ----------------------------------------------

/// Run the conformance subcommand and return (exit ok, stdout).
fn run_conformance() -> (bool, String) {
    let output = Command::new(binary())
        .arg("conformance")
        .arg("--wit")
        .arg(repo_root().join("host.wit"))
        .arg("--corpus")
        .arg(repo_root().join("tests/cln/conformance"))
        .output()
        .expect("binary runs");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn conformance_verifies_the_advertised_world_is_provided() {
    // CMOD-03 check 3, which can run today.
    let (_, stdout) = run_conformance();
    assert!(
        stdout.contains("world imports are all provided"),
        "{stdout}"
    );
    assert!(
        stdout.contains("advertised interfaces provided"),
        "check 3 did not report a count:\n{stdout}"
    );
}

#[test]
fn conformance_verifies_nothing_outside_the_world_is_registered() {
    // CMOD-03 check 4. An undeclared registration is reachable by a guest that
    // knows its name while being invisible to every static check.
    let (_, stdout) = run_conformance();
    assert!(
        stdout.contains("no extra-world imports accepted"),
        "{stdout}"
    );
    assert!(
        stdout.contains("no registrations outside the world"),
        "{stdout}"
    );
}

#[test]
fn conformance_reports_incomplete_while_the_corpus_is_absent() {
    // The point of the whole design: a partial run must not read as a pass.
    let (ok, stdout) = run_conformance();

    assert!(
        !ok,
        "an incomplete conformance run must exit non-zero:\n{stdout}"
    );
    assert!(stdout.contains("INCOMPLETE"), "{stdout}");
    assert!(stdout.contains("2 of 4"), "{stdout}");
}

#[test]
fn conformance_says_why_the_corpus_checks_did_not_run() {
    // A reader six months from now needs to know whether this is a bug or a
    // known gap.
    let (_, stdout) = run_conformance();
    assert!(stdout.contains("SKIPPED"), "{stdout}");
    assert!(
        stdout.contains("does not exist yet"),
        "the skip must explain itself:\n{stdout}"
    );
}

#[test]
fn conformance_never_claims_a_pass_it_did_not_earn() {
    let (ok, stdout) = run_conformance();
    assert!(!ok);
    assert!(
        !stdout.contains("RESULT: CONFORMS"),
        "a partial run claimed conformance:\n{stdout}"
    );
}
