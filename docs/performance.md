# Performance

Measurements against the §1.8 envelope, how they were taken, and — the part
that matters most — what they do not establish.

## What §1.8 asks for

| Target | Value |
|---|---|
| Sustained throughput | 10 000–20 000 req/s on 4 cores, mixed handlers doing 1–3 DB queries |
| Cold start | Under 100 ms, spawn to first request served |
| Instance checkout | Sub-millisecond from a warm pool |
| Baseline memory | Under 50 MB per guest instance |

The spec is explicit that these are "targets the reference implementation is
expected to meet on commodity hardware, not contractual guarantees."

## Results

Measured on an Apple M2 (4 performance + 4 efficiency cores, 24 GB), macOS,
release build, loopback networking.

### End-to-end throughput

`testing/bench/run.sh` — 50 connections, 6 s per route, 100% success on every
route.

| Route | req/s | p50 | p99 |
|---|---|---|---|
| `GET /` (hello world) | 33 232 | 1.39 ms | 4.80 ms |
| `GET /users/:id` (path parameter) | 32 709 | 1.40 ms | 4.89 ms |
| `GET /counter` (composed bridge call) | 32 735 | 1.41 ms | 4.84 ms |
| `GET /_health` (no guest call) | 70 470 | 0.69 ms | 1.50 ms |
| `POST /echo` (1 KB body) | 30 499 | 1.52 ms | 5.02 ms |

### Cold start

`testing/bench/coldstart.sh` — 10 runs, spawn to first served request.

Best 53 ms, average 58 ms, worst 63 ms. Every run under the 100 ms target.

This figure *includes* the measurement harness's own overhead: the script
shells out to `python3` twice per run for a comparable clock, which costs tens
of milliseconds on its own. The real cold start is meaningfully lower; treat
63 ms as a ceiling rather than an estimate.

### Instance checkout

`cargo bench --bench pool` in `clean-host-core`.

| Operation | Cost |
|---|---|
| Checkout + return, warm pool | 126 ns |
| Checkout + return, single instance | 88 ns |
| Eight concurrent checkouts | 559 ns |
| Checkout that grows the pool | 252 ns |
| Health snapshot | 6 ns |

Against a sub-millisecond target, that is roughly four orders of magnitude of
headroom. Instance creation is free in this benchmark — it measures the pool's
own bookkeeping, so a real checkout adds the runtime's instantiate cost
whenever the pool has to grow.

### Per-request host work

`cargo bench --bench hot_path` in `clean-server`. Everything the server does
outside the guest call.

| Operation | Cost |
|---|---|
| Routing, literal root | 75 ns |
| Routing, one path parameter | 199 ns |
| Routing, two path parameters | 226 ns |
| Routing, wildcard tail | 171 ns |
| Routing, miss (full table scan) | 90 ns |
| Render `Set-Cookie` | 197 ns |
| Read one cookie of four | 118 ns |
| Validate a CSRF token | 132 ns |
| CSRF safe-method short-circuit | 28 ns |
| Frame one SSE event | 431 ns |

At 33 000 req/s, the whole routing-plus-security path costs well under 1% of a
request. Nothing here is close to being the bottleneck, which is why Phase 7
made no optimisations: there was no gap to close.

## What these numbers do NOT establish

**They are not a §1.8 certification.** The envelope names 4-core hardware
running "mixed handlers doing 1–3 DB queries and light computation per
request". The acceptance guest does no I/O at all — no database, no session
store, no outbound call. What is measured is *host overhead*, which is an upper
bound on what a real application could reach, not a prediction of what one
will.

A real handler making two database round trips will be dominated by those round
trips. §1.8 says so directly: "Real numbers depend on… whether bridge calls hit
an external backplane whose latency dominates the request."

**The hardware is not the reference hardware.** An Apple M2 is ARM with 4
performance plus 4 efficiency cores; the envelope implies a 4-core commodity
machine without saying which. These figures are not comparable to a run on
different silicon, and should not be quoted as though they were.

**Loopback is not a network.** No NIC, no TLS handshake, no real client
latency. The p99 figures describe queueing inside the server, not what a user
would experience.

**Memory is not measured here.** §1.8's "under 50 MB per guest instance" target
has no measurement in this document yet.

## Reproducing

```bash
cargo build --release
./testing/fake-guest/build.sh
./testing/fake-bridge/build.sh

./testing/bench/run.sh            # throughput; DURATION/CONNECTIONS to tune
./testing/bench/coldstart.sh      # cold start; RUNS to tune
cargo bench --bench hot_path      # per-request host work
(cd ../clean-host-core && cargo bench --bench pool)
```

`run.sh` needs `oha` (`cargo install oha`).

## Why these do not gate CI

PLAN.md Layer D: "Runs in CI but doesn't gate — a regression triggers a review,
not a build failure (perf tests are noisy)." Shared CI runners have variable
neighbours, and a throughput threshold there would either be set so loose it
catches nothing or so tight it fails at random. The benchmarks are run and
their output printed, so a regression is visible to anyone reading the log.
