//! Microbenchmarks for the per-request hot path (§1.8, PLAN.md Layer D).
//!
//! These measure the work the server does on *every* request, outside the
//! guest call: matching a route, rendering a cookie, checking a CSRF token.
//! They are the pieces a throughput regression would most likely come from,
//! and unlike the end-to-end numbers they are not dominated by the network or
//! by Wasmtime.
//!
//! Layer D does not gate CI: a benchmark regression triggers a look, not a
//! build failure, because these numbers are noisy on shared runners.

use std::hint::black_box;
use std::time::Instant;

/// Report one measurement in the format the other suites print.
fn bench(name: &str, iterations: u32, mut body: impl FnMut()) {
    // Warm caches and let the CPU settle before measuring.
    for _ in 0..(iterations / 10).max(1) {
        body();
    }

    let started = Instant::now();
    for _ in 0..iterations {
        body();
    }
    let elapsed = started.elapsed();

    let per_op = elapsed / iterations;
    let per_sec = if per_op.as_nanos() > 0 {
        1_000_000_000f64 / per_op.as_nanos() as f64
    } else {
        f64::INFINITY
    };

    println!(
        "{name:<44} {:>9.0} ns/op   {:>12.0} ops/sec",
        per_op.as_nanos() as f64,
        per_sec
    );
}

fn main() {
    println!("clean-server hot-path microbenchmarks");
    println!(
        "{:<44} {:>9}      {:>12}",
        "benchmark", "latency", "throughput"
    );
    println!("{}", "-".repeat(74));

    routing_benchmarks();
    security_benchmarks();
    framing_benchmarks();

    println!();
    println!("These measure per-request host work only — no guest call, no");
    println!("network. See docs/performance.md for the end-to-end numbers and");
    println!("for what this hardware does and does not certify.");
}

fn routing_benchmarks() {
    use clean_server::routing::{Match, Route, Router};

    // A table shaped like a real application: some literals, some parameters,
    // one wildcard, ordered so the interesting cases are not first.
    let routes: Vec<Route> = [
        ("GET", "/", 0u32),
        ("GET", "/health", 1),
        ("GET", "/users", 2),
        ("GET", "/users/:id", 3),
        ("GET", "/users/:id/posts", 4),
        ("GET", "/users/:id/posts/:post", 5),
        ("POST", "/users", 6),
        ("GET", "/orgs/:org/repos/:repo", 7),
        ("GET", "/static/*path", 8),
        ("GET", "/me", 9),
    ]
    .into_iter()
    .map(|(method, path, id)| Route {
        method: method.to_string(),
        path: path.to_string(),
        handler_id: id,
        csrf: true,
    })
    .collect();

    let router = Router::new(routes, "/");

    bench("routing: literal root", 200_000, || {
        black_box(router.match_route(black_box("GET"), black_box("/")));
    });

    bench("routing: one path parameter", 200_000, || {
        black_box(router.match_route(black_box("GET"), black_box("/users/4821")));
    });

    bench("routing: two path parameters", 200_000, || {
        black_box(router.match_route(black_box("GET"), black_box("/orgs/clean/repos/server")));
    });

    bench("routing: wildcard tail", 200_000, || {
        black_box(router.match_route(black_box("GET"), black_box("/static/css/site.css")));
    });

    bench("routing: miss (404 scan)", 200_000, || {
        // The worst case: every route is examined before giving up.
        black_box(router.match_route(black_box("GET"), black_box("/nothing/here/at/all")));
    });

    // Guards the specificity ordering: /me must not degrade as the table grows.
    bench("routing: literal beats parameter", 200_000, || {
        let m = router.match_route(black_box("GET"), black_box("/me"));
        debug_assert!(matches!(m, Match::Found { handler_id: 9, .. }));
        black_box(m);
    });
}

fn security_benchmarks() {
    use clean_server::envelope::{csrf_rejection, read_cookie, set_cookie_header, CookieOptions};

    let options = CookieOptions {
        path: Some("/".into()),
        http_only: true,
        secure: true,
        ..Default::default()
    };

    bench("cookie: render Set-Cookie", 200_000, || {
        black_box(set_cookie_header(
            black_box("session"),
            black_box("8f14e45fceea167a5a36dedd4bea2543"),
            black_box(&options),
        ))
        .ok();
    });

    // A realistic header: the wanted cookie is not the first one.
    let cookie_header =
        "theme=dark; lang=en-GB; __Host-csrf=8f14e45fceea167a5a36dedd4bea2543; sid=abc123";

    bench("cookie: read one of four", 500_000, || {
        black_box(read_cookie(black_box(cookie_header), black_box("sid")));
    });

    // Runs on every unsafe method, so it is squarely on the hot path.
    bench("csrf: validate a matching token", 500_000, || {
        black_box(csrf_rejection(
            black_box("POST"),
            black_box(Some(cookie_header)),
            black_box(Some("8f14e45fceea167a5a36dedd4bea2543")),
        ));
    });

    bench("csrf: safe method short-circuit", 1_000_000, || {
        black_box(csrf_rejection(
            black_box("GET"),
            black_box(Some(cookie_header)),
            black_box(None),
        ));
    });
}

fn framing_benchmarks() {
    use clean_server::sockets::SseEvent;

    let event = SseEvent {
        event_type: "message".into(),
        data: "the quick brown fox jumps over the lazy dog".into(),
        id: "42".into(),
        retry_millis: None,
    };

    bench("sse: frame one event", 500_000, || {
        black_box(event.frame());
    });

    let multiline = SseEvent {
        event_type: "log".into(),
        data: "line one\nline two\nline three\nline four".into(),
        id: String::new(),
        retry_millis: None,
    };

    bench("sse: frame multi-line data", 500_000, || {
        black_box(multiline.frame());
    });
}
