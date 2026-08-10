//! Diagnostics (§1.9): health, metrics, and trap snapshots.
//!
//! Everything here reports on a running server rather than changing it, which
//! is why it sits on the main listener rather than behind the admin API's
//! authentication — a load balancer needs `/_health` without credentials.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use clean_host_core::HealthReport;

/// Render the health report as JSON (§1.9).
///
/// Shape is an operator-facing contract: dashboards and load balancers script
/// against it, so fields are added rather than renamed.
pub fn health_json(
    report: &HealthReport,
    version: &str,
    trap_count: usize,
    last_trap: Option<&TrapSnapshot>,
) -> String {
    let mut out = String::from("{");

    // `composed` is the liveness signal: a host that never composed is not
    // serving anything, whatever else is true.
    out.push_str(&format!(
        r#""status":"{}","#,
        if report.composed { "ok" } else { "unavailable" }
    ));
    out.push_str(&format!(r#""composed":{},"#, report.composed));
    out.push_str(&format!(r#""version":"{}","#, escape(version)));
    out.push_str(&format!(r#""uptime-secs":{},"#, report.uptime.as_secs()));

    match &report.pool {
        Some(pool) => out.push_str(&format!(
            r#""pool":{{"instances-current":{},"instances-max":{},"instances-min":{},"checkouts-active":{},"checkouts-queued":{}}},"#,
            pool.instances_current,
            pool.instances_max,
            pool.instances_min,
            pool.checkouts_active,
            pool.checkouts_queued
        )),
        None => out.push_str(r#""pool":null,"#),
    }

    out.push_str(r#""bridges":["#);
    for (i, bridge) in report.bridges.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"interface":"{}","component":"{}"}}"#,
            escape(&bridge.interface),
            escape(&bridge.component)
        ));
    }
    out.push_str("],");

    // Recent traps: a host that is up and composed but trapping on every
    // request is not healthy in any sense an operator cares about. The count
    // says something broke; `last-trap` says what, which is the difference
    // between an alert and a diagnosis.
    out.push_str(&format!(r#""recent-traps":{trap_count},"#));
    match last_trap {
        Some(trap) => out.push_str(&format!(
            r#""last-trap":{{"correlation-id":"{}","path":"{}","detail":"{}"}},"#,
            escape(&trap.correlation_id),
            escape(&trap.path),
            escape(&trap.detail.replace('\n', " | "))
        )),
        None => out.push_str(r#""last-trap":null,"#),
    }

    // A reload that failed is the thing an operator most needs to see here:
    // the process is up, but it is not running what they think it is.
    match report.last_reload_success {
        Some(ok) => out.push_str(&format!(r#""last-reload-success":{ok}"#)),
        None => out.push_str(r#""last-reload-success":null"#),
    }

    out.push('}');
    out
}

/// Whether the report should answer 200 or 503.
///
/// A load balancer reads the status code, not the body, so an uncomposed host
/// must not look healthy.
pub fn health_is_ok(report: &HealthReport) -> bool {
    report.composed
}

/// Request counters, for the optional Prometheus endpoint.
///
/// Deliberately a handful of atomics rather than a metrics crate: the base
/// binary should not pay for a registry when `[server] metrics-path` is unset,
/// which is the default.
#[derive(Debug, Default)]
pub struct Metrics {
    requests_total: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    /// Cumulative latency, for computing an average without a histogram.
    latency_ms_total: AtomicU64,
    /// Guest traps, which are the signal worth alerting on.
    traps_total: AtomicU64,
    /// Requests shed because the pool was saturated.
    shed_total: AtomicU64,
}

impl Metrics {
    pub fn record(&self, status: u16, latency: Duration) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.latency_ms_total
            .fetch_add(latency.as_millis() as u64, Ordering::Relaxed);

        let bucket = match status {
            200..=299 => &self.responses_2xx,
            400..=499 => &self.responses_4xx,
            500..=599 => &self.responses_5xx,
            _ => return,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_trap(&self) {
        self.traps_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_shed(&self) {
        self.shed_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Render in Prometheus text exposition format.
    pub fn render(&self, pool: Option<clean_host_core::PoolHealth>) -> String {
        let requests = self.requests_total.load(Ordering::Relaxed);
        let latency_total = self.latency_ms_total.load(Ordering::Relaxed);

        let mut out = String::new();
        let mut counter = |name: &str, help: &str, value: u64| {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} counter\n"));
            out.push_str(&format!("{name} {value}\n"));
        };

        counter(
            "clean_server_requests_total",
            "Requests handled since start.",
            requests,
        );
        counter(
            "clean_server_responses_2xx_total",
            "Responses with a 2xx status.",
            self.responses_2xx.load(Ordering::Relaxed),
        );
        counter(
            "clean_server_responses_4xx_total",
            "Responses with a 4xx status.",
            self.responses_4xx.load(Ordering::Relaxed),
        );
        counter(
            "clean_server_responses_5xx_total",
            "Responses with a 5xx status.",
            self.responses_5xx.load(Ordering::Relaxed),
        );
        counter(
            "clean_server_guest_traps_total",
            "Guest invocations that trapped.",
            self.traps_total.load(Ordering::Relaxed),
        );
        counter(
            "clean_server_requests_shed_total",
            "Requests rejected because the instance pool was saturated.",
            self.shed_total.load(Ordering::Relaxed),
        );

        out.push_str("# HELP clean_server_request_latency_ms_total Cumulative request latency.\n");
        out.push_str("# TYPE clean_server_request_latency_ms_total counter\n");
        out.push_str(&format!(
            "clean_server_request_latency_ms_total {latency_total}\n"
        ));

        if let Some(pool) = pool {
            for (name, help, value) in [
                (
                    "clean_server_pool_instances_current",
                    "Instances that exist right now.",
                    pool.instances_current,
                ),
                (
                    "clean_server_pool_instances_max",
                    "Configured instance ceiling.",
                    pool.instances_max,
                ),
                (
                    "clean_server_pool_checkouts_active",
                    "Instances currently serving a request.",
                    pool.checkouts_active,
                ),
                (
                    "clean_server_pool_checkouts_queued",
                    "Requests waiting for an instance.",
                    pool.checkouts_queued,
                ),
            ] {
                out.push_str(&format!("# HELP {name} {help}\n"));
                out.push_str(&format!("# TYPE {name} gauge\n"));
                out.push_str(&format!("{name} {value}\n"));
            }
        }

        out
    }
}

/// A captured guest trap (§1.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapSnapshot {
    pub correlation_id: String,
    pub method: String,
    pub path: String,
    /// The trap message, including the wasm backtrace when the runtime
    /// supplied one.
    pub detail: String,
}

impl TrapSnapshot {
    /// Render as one structured line.
    ///
    /// Multi-line backtraces are folded onto one line: a log sink that splits
    /// on newlines would otherwise scatter one trap across several records and
    /// break correlation.
    pub fn render(&self) -> String {
        format!(
            "trap correlation_id={} method={} path={} detail={}",
            self.correlation_id,
            self.method,
            self.path,
            self.detail.replace('\n', " | ")
        )
    }
}

/// The most recent traps, for `/_health` and post-mortem inspection.
///
/// Bounded: a guest trapping on every request must not turn the snapshot store
/// into an unbounded leak.
#[derive(Debug)]
pub struct TrapLog {
    recent: Mutex<Vec<TrapSnapshot>>,
    capacity: usize,
}

impl TrapLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent: Mutex::new(Vec::new()),
            capacity,
        }
    }

    pub fn record(&self, snapshot: TrapSnapshot) {
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == self.capacity {
            recent.remove(0);
        }
        recent.push(snapshot);
    }

    pub fn len(&self) -> usize {
        self.recent.lock().unwrap().len()
    }

    /// Whether anything has trapped. Reads better than `len() == 0` at the
    /// call site, which is the whole reason clippy asks for it.
    pub fn is_empty(&self) -> bool {
        self.recent.lock().unwrap().is_empty()
    }

    /// The most recent trap, which is what an operator looks at first.
    pub fn last(&self) -> Option<TrapSnapshot> {
        self.recent.lock().unwrap().last().cloned()
    }
}

impl Default for TrapLog {
    fn default() -> Self {
        Self::new(32)
    }
}

/// Whether a guest error was a trap rather than a host-side failure.
///
/// Traps are the guest's fault and worth capturing; a pool timeout is the
/// host's own load condition and would be noise in the trap log.
pub fn is_trap(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    (lowered.contains("trap")
        || lowered.contains("unreachable")
        || lowered.contains("wasm backtrace"))
        && !lowered.contains("pool exhausted")
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clean_host_core::{BridgeHealth, PoolHealth};

    fn report(composed: bool) -> HealthReport {
        HealthReport {
            composed,
            bridges: vec![BridgeHealth {
                interface: "clean:session/store".into(),
                component: "./session.wasm".into(),
                version: String::new(),
            }],
            pool: Some(PoolHealth {
                instances_min: 1,
                instances_max: 8,
                instances_current: 3,
                checkouts_active: 1,
                checkouts_queued: 0,
            }),
            uptime: Duration::from_secs(42),
            last_reload_at: None,
            last_reload_success: None,
        }
    }

    #[test]
    fn a_healthy_report_renders_valid_json() {
        let json = health_json(&report(true), "0.3.0", 0, None);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["composed"], true);
        assert_eq!(parsed["version"], "0.3.0");
        assert_eq!(parsed["uptime-secs"], 42);
        assert_eq!(parsed["pool"]["instances-current"], 3);
        assert_eq!(parsed["bridges"][0]["interface"], "clean:session/store");
    }

    #[test]
    fn an_uncomposed_host_is_not_healthy() {
        // A load balancer reads the status code; an uncomposed host serves
        // nothing and must not be handed traffic.
        let mut r = report(false);
        r.pool = None;
        assert!(!health_is_ok(&r));

        let parsed: serde_json::Value =
            serde_json::from_str(&health_json(&r, "0.3.0", 0, None)).unwrap();
        assert_eq!(parsed["status"], "unavailable");
        assert!(parsed["pool"].is_null());
    }

    #[test]
    fn a_failed_reload_is_visible_in_health() {
        // The process is up but is not running what the operator thinks.
        let mut r = report(true);
        r.last_reload_success = Some(false);

        let parsed: serde_json::Value =
            serde_json::from_str(&health_json(&r, "0.3.0", 0, None)).unwrap();
        assert_eq!(parsed["last-reload-success"], false);
    }

    #[test]
    fn a_quote_in_a_bridge_path_cannot_break_the_json() {
        let mut r = report(true);
        r.bridges[0].component = r#"./we"ird.wasm"#.into();
        let json = health_json(&r, "0.3.0", 0, None);
        serde_json::from_str::<serde_json::Value>(&json).expect("still valid JSON");
    }

    // --- metrics -----------------------------------------------------------

    #[test]
    fn metrics_count_requests_by_status_class() {
        let m = Metrics::default();
        m.record(200, Duration::from_millis(5));
        m.record(204, Duration::from_millis(5));
        m.record(404, Duration::from_millis(1));
        m.record(500, Duration::from_millis(9));

        let text = m.render(None);
        assert!(text.contains("clean_server_requests_total 4"), "{text}");
        assert!(
            text.contains("clean_server_responses_2xx_total 2"),
            "{text}"
        );
        assert!(
            text.contains("clean_server_responses_4xx_total 1"),
            "{text}"
        );
        assert!(
            text.contains("clean_server_responses_5xx_total 1"),
            "{text}"
        );
        assert!(
            text.contains("clean_server_request_latency_ms_total 20"),
            "{text}"
        );
    }

    #[test]
    fn every_metric_declares_help_and_type() {
        // Prometheus tolerates their absence, but a scraper's UI is unusable
        // without them.
        let text = Metrics::default().render(None);
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let name = line.split_whitespace().next().unwrap();
            assert!(
                text.contains(&format!("# HELP {name} ")),
                "no HELP for {name}"
            );
            assert!(
                text.contains(&format!("# TYPE {name} ")),
                "no TYPE for {name}"
            );
        }
    }

    #[test]
    fn pool_gauges_appear_only_when_a_pool_exists() {
        let m = Metrics::default();
        assert!(!m.render(None).contains("clean_server_pool_"));

        let text = m.render(Some(PoolHealth {
            instances_min: 1,
            instances_max: 8,
            instances_current: 3,
            checkouts_active: 2,
            checkouts_queued: 1,
        }));
        assert!(
            text.contains("clean_server_pool_instances_current 3"),
            "{text}"
        );
        assert!(
            text.contains("clean_server_pool_checkouts_queued 1"),
            "{text}"
        );
    }

    #[test]
    fn traps_and_shed_requests_are_counted_separately() {
        // They mean different things: a trap is the guest's fault, shedding is
        // the host under load.
        let m = Metrics::default();
        m.record_trap();
        m.record_shed();
        m.record_shed();

        let text = m.render(None);
        assert!(text.contains("clean_server_guest_traps_total 1"), "{text}");
        assert!(
            text.contains("clean_server_requests_shed_total 2"),
            "{text}"
        );
    }

    // --- traps -------------------------------------------------------------

    #[test]
    fn a_trap_snapshot_carries_the_request_context() {
        let snapshot = TrapSnapshot {
            correlation_id: "req-0001".into(),
            method: "POST".into(),
            path: "/checkout".into(),
            detail: "wasm trap: unreachable".into(),
        };
        let line = snapshot.render();
        assert!(line.contains("correlation_id=req-0001"), "{line}");
        assert!(line.contains("path=/checkout"), "{line}");
    }

    #[test]
    fn a_multiline_backtrace_stays_on_one_line() {
        // A sink that splits on newlines would scatter one trap across records.
        let snapshot = TrapSnapshot {
            correlation_id: "req-2".into(),
            method: "GET".into(),
            path: "/".into(),
            detail: "wasm trap: unreachable\n  at foo\n  at bar".into(),
        };
        assert!(!snapshot.render().contains('\n'));
    }

    #[test]
    fn recent_traps_appear_in_health() {
        // A host that is up and composed but trapping on every request is not
        // healthy in any sense an operator cares about.
        let trap = TrapSnapshot {
            correlation_id: "req-9".into(),
            method: "POST".into(),
            path: "/checkout".into(),
            detail: "wasm trap: unreachable\n  at foo".into(),
        };
        let json = health_json(&report(true), "0.3.0", 1, Some(&trap));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["recent-traps"], 1);
        // The count says something broke; the detail says what.
        assert_eq!(parsed["last-trap"]["correlation-id"], "req-9");
        assert_eq!(parsed["last-trap"]["path"], "/checkout");
        // A multi-line backtrace must not break the JSON.
        assert!(parsed["last-trap"]["detail"]
            .as_str()
            .unwrap()
            .contains('|'));
    }

    #[test]
    fn a_fresh_trap_log_is_empty() {
        let log = TrapLog::new(4);
        assert!(log.is_empty());

        log.record(TrapSnapshot {
            correlation_id: "req-1".into(),
            method: "GET".into(),
            path: "/".into(),
            detail: "trap".into(),
        });
        assert!(!log.is_empty());
    }

    #[test]
    fn the_trap_log_is_bounded() {
        // A guest trapping on every request must not leak.
        let log = TrapLog::new(3);
        for i in 0..10 {
            log.record(TrapSnapshot {
                correlation_id: format!("req-{i}"),
                method: "GET".into(),
                path: "/".into(),
                detail: "trap".into(),
            });
        }
        assert_eq!(log.len(), 3);
        // The newest are kept, not the oldest.
        assert_eq!(log.last().unwrap().correlation_id, "req-9");
    }

    #[test]
    fn guest_traps_are_distinguished_from_host_load_conditions() {
        assert!(is_trap("wasm trap: unreachable"));
        assert!(is_trap("error while executing: wasm backtrace:"));
        // Pool exhaustion is the host under load, not the guest misbehaving;
        // counting it as a trap would bury the real signal.
        assert!(!is_trap("pool exhausted after 5s"));
        assert!(!is_trap("host is shutting down"));
    }
}
