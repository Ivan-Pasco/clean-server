//! Phase 5 acceptance: health, metrics, guest logging, trap capture.

mod support;

use support::{header, Server};

// --- /_health --------------------------------------------------------------

#[test]
fn health_reports_a_composed_host_as_ok() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, headers, body) = server.request("GET", "/_health");

    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("application/json"));

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["composed"], true);
    assert!(parsed["version"].is_string());
    assert!(parsed["uptime-secs"].is_number());
}

#[test]
fn health_reports_the_pool_state() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (_, _, body) = server.request("GET", "/_health");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert!(parsed["pool"]["instances-max"].is_number());
    assert!(parsed["pool"]["instances-current"].is_number());
}

#[test]
fn health_lists_composed_bridges() {
    // An operator needs to see which bridges are actually live, not which ones
    // the config mentions.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (_, _, body) = server.request("GET", "/_health");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    let bridges = parsed["bridges"].as_array().expect("a bridges array");
    assert!(
        bridges
            .iter()
            .any(|b| b["interface"] == "clean:fake-bridge/store"),
        "{body}"
    );
}

#[test]
fn health_is_never_cached() {
    // A cached health check reports a host that may already be gone.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (_, headers, _) = server.request("GET", "/_health");
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
}

#[test]
fn health_needs_no_authentication() {
    // A load balancer has no credentials; requiring them would make the check
    // unusable for the one caller that matters most.
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(server.request("GET", "/_health").0, 200);
}

#[test]
fn a_guest_route_cannot_shadow_the_health_path() {
    // §1.9's endpoints belong to the server. If a guest could claim /_health,
    // a deploy could silently disable its own liveness check.
    let Some(server) = Server::start_with("[server]\nhealth-path = \"/\"") else {
        return;
    };
    let (status, headers, _) = server.request("GET", "/");

    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/json"),
        "the server's health check must win over the guest's route"
    );
}

// --- /_metrics -------------------------------------------------------------

#[test]
fn metrics_are_off_unless_configured() {
    // The base binary should not expose an endpoint nobody asked for.
    let Some(server) = Server::start_composed() else {
        return;
    };
    assert_eq!(server.request("GET", "/_metrics").0, 404);
}

#[test]
fn metrics_render_in_prometheus_text_format() {
    let Some(server) = Server::start_metrics() else {
        return;
    };
    // Generate some traffic to count.
    server.request("GET", "/");
    server.request("GET", "/does-not-exist");

    let (status, headers, body) = server.request("GET", "/_metrics");

    assert_eq!(status, 200);
    assert!(
        header(&headers, "content-type")
            .unwrap_or_default()
            .starts_with("text/plain"),
        "{headers:?}"
    );
    assert!(body.contains("clean_server_requests_total"), "{body}");
    assert!(
        body.contains("# TYPE clean_server_requests_total counter"),
        "{body}"
    );
}

#[test]
fn metrics_count_responses_by_status_class() {
    let Some(server) = Server::start_metrics() else {
        return;
    };
    server.request("GET", "/");
    server.request("GET", "/nope");

    let (_, _, body) = server.request("GET", "/_metrics");
    let value = |name: &str| -> u64 {
        body.lines()
            .find(|l| l.starts_with(name) && !l.starts_with('#'))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no {name} in:\n{body}"))
    };

    assert!(value("clean_server_responses_2xx_total") >= 1);
    assert!(value("clean_server_responses_4xx_total") >= 1);
}

#[test]
fn metrics_expose_the_pool_gauges() {
    let Some(server) = Server::start_metrics() else {
        return;
    };
    let (_, _, body) = server.request("GET", "/_metrics");
    assert!(body.contains("clean_server_pool_instances_max"), "{body}");
    assert!(
        body.contains("# TYPE clean_server_pool_instances_max gauge"),
        "{body}"
    );
}

#[test]
fn a_custom_metrics_path_is_honoured() {
    let Some(server) = Server::start_with_metrics_path("/internal/prom") else {
        return;
    };
    assert_eq!(server.request("GET", "/internal/prom").0, 200);
    // And the default path is not silently also served.
    assert_eq!(server.request("GET", "/_metrics").0, 404);
}

// --- guest logging ---------------------------------------------------------

#[test]
fn a_guest_log_record_reaches_the_host_log() {
    // `/log` emits one structured record through clean:host/log.
    let Some(server) = Server::start_composed() else {
        return;
    };
    let (status, _, _) = server.request("GET", "/log");
    assert_eq!(status, 200);

    let logs = server.stderr_so_far();
    assert!(
        logs.contains("hello from the guest"),
        "guest record missing from host log:\n{logs}"
    );
}

#[test]
fn a_guest_log_record_carries_its_fields() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    server.request("GET", "/log");

    let logs = server.stderr_so_far();
    assert!(
        logs.contains("route=log-demo"),
        "field missing from record:\n{logs}"
    );
}

#[test]
fn a_guest_log_record_is_tied_to_its_request() {
    // Without the correlation id a guest record cannot be matched to the
    // request that produced it, which is most of its value.
    let Some(server) = Server::start_composed() else {
        return;
    };
    server.request("GET", "/log");

    let logs = server.stderr_so_far();
    assert!(
        logs.contains("correlation_id"),
        "no correlation id on the guest record:\n{logs}"
    );
}

// --- request logging -------------------------------------------------------

#[test]
fn every_request_produces_one_structured_log_line() {
    let Some(server) = Server::start_composed() else {
        return;
    };
    server.request("GET", "/");

    let logs = server.stderr_so_far();
    assert!(logs.contains("clean_server::request"), "{logs}");
    assert!(logs.contains("status=200"), "{logs}");
    assert!(logs.contains("latency_ms"), "{logs}");
}
