# Clean Server — Implementation Plan

Build plan for `clean-server`, the reference Clean host for HTTP applications. Derived from `foundation/02 components/hosts/clean-server/01-server.md`, `foundation/02 components/hosts/clean-host-core/01-specification.md` (the shared runtime library every host delegates to), `foundation/02 components/hosts/00-host-model.md` (the three-layer guest/bridges/hosts model), and `foundation/03 platform/16-host-contract-validation.md` for load-time verification obligations.

Server's job, one sentence: **own the HTTP surface — listener, request parsing, response writing, WebSocket, SSE — and delegate everything else (composition, bridges, WASI, pooling, capability manifest) to `clean-host-core`.** The server is a thin I/O layer on top of the shared runtime library.

Because the specs put so much weight on the "concrete host is thin, shared library is fat" split (host model §3, clean-host-core §1), **the biggest architectural decision on day one is where `clean-host-core` lives** — same repo, sibling repo, or vendored. That decision shapes the plan and is called out in §9 open questions with a proposal.

M0 target: **`clean-server` binary implementing the `server` world of `clean:host@0.1.0`, serving one HTTP endpoint from a compiled Clean guest.** Bridges compose from `host.toml`; sessions/data/kv/jobs/mail/realtime are stub or in-process backends in M0. Production backends (Redis session, Postgres data, etc.) land in M1+.

---

## 1. Language and toolchain

**Choice: Rust.**

Rationale:

1. **Wasmtime is Rust.** `clean-server` runs Clean guest components under Wasmtime with async, epoch interruption, and pooling allocator (server §1.4.3). That's a native Rust dependency; every other language ships a wrapper with more friction and less feature parity.
2. **`clean-host-core` reference implementation is Rust** (host-core §14: `clean-host-core-rs` + `clean-host-core-wasmtime`). Server links this as a library. Same language, one call.
3. **10k–20k requests/sec target** (server §1.8) demands zero-copy request parsing and native async. Rust's `hyper` / `axum` / `tokio` stack is the industry reference for this envelope. Node hits it with a lot of care; Go hits it but the CGO bridge to Wasmtime hurts; Rust gets it directly.
4. **Sub-100ms cold start** (server §1.8) — no VM warmup, no interpreter JIT phase.
5. **Shared crates.** `cln-shared` from manager (Platform 13 diagnostic format, `build_manifest.rs` for reading `.clapp` / `.serve` manifests when framework invokes server for `cln run`). Same argument as manager and framework — one wire format across the stack.

**Reference-stack picks (subject to ADR-0002 per Architecture Boundaries §2.6):**

- HTTP: `hyper` v1 directly (not `axum` — we own routing and don't need framework overhead), `hyper-util` for common bits.
- Async runtime: `tokio` with the multi-thread scheduler.
- TLS: `rustls` + `tokio-rustls`. Reject `native-tls`/OpenSSL — the whole point of the deployment shape (§1.2) is zero shared-object deps beyond libc.
- WebSocket: `tokio-tungstenite`.
- SSE: hand-rolled (small, no crate needed).
- Wasmtime: current stable, with the async, epoch-interruption, and pooling-allocator features enabled. Version pinned in `clean-host-core-wasmtime`.
- TOML: `toml` for `host.toml` reads (read-only — no round-trip needed here).
- Logging: `tracing` + `tracing-subscriber`, feeding `wasi:logging` → structured sink per `clean:host/log`.
- Metrics: `metrics` + `metrics-exporter-prometheus` behind a feature flag (server §1.9).
- Per-OS signal handling: `tokio::signal` (SIGHUP for reload, SIGTERM for drain).

---

## 2. Crate / module layout

Single Cargo workspace. **Key open question:** does `clean-host-core` live inside this workspace, or as a separate sibling repo `clean-host-core/`? Proposal (see §9): separate repo, published as a crate on our internal registry; every host binary depends on it as a pinned version. Reason: five hosts (server, worker, cli, browser, edge) all consume it — putting it inside `clean-server/` couples every other host to changes here.

Layout assumes `clean-host-core` is a sibling repo, published as `clean-host-core` (the lib) + `clean-host-core-wasmtime` (the Wasmtime adapter). Both are dependencies below.

```
clean-server/
├── Cargo.toml                          # workspace root
├── PLAN.md                             # this file
├── host.wit                            # the concrete host's WIT (HCV-02, host-contract §16)
│
├── crates/
│   ├── clean-server/                   # the binary; thin I/O layer
│   │   └── src/
│   │       ├── main.rs                 # argv parsing (host.toml path), startup, shutdown
│   │       ├── startup.rs              # constructs clean-host-core::Host, registers envelopes,
│   │       │                           # calls compose(), starts listener
│   │       ├── config.rs               # [server] block schema (server §1.6 / schema/server-block.toml.md)
│   │       ├── listener.rs             # TCP accept loop, TLS termination, HTTP/1 + HTTP/2 protocol select
│   │       ├── request_loop.rs         # per-connection request handling (server §1.4.2 steps 1–7)
│   │       ├── routing.rs              # path + method matching against guest-registered routes
│   │       ├── request_marshal.rs      # HTTP request → clean:host/request WIT record
│   │       ├── response_marshal.rs     # clean:host/response WIT record → HTTP response bytes
│   │       ├── websocket.rs            # WebSocket upgrade + per-socket outbound queue (server §1.4)
│   │       ├── sse.rs                  # SSE response initiation + keep-alive framing
│   │       ├── errors.rs               # trap → structured log + optional forward to errors.cleanlanguage.dev
│   │       └── signals.rs              # SIGHUP → reload, SIGTERM → graceful drain
│   │
│   ├── server-envelope/                # host-side envelope impls (server §1.5)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── session_http.rs         # clean:session/http-envelope: set-cookie, csrf, read-cookie
│   │       └── realtime_sockets.rs     # clean:realtime/sockets: deliver, close, queued-bytes
│   │
│   ├── server-admin/                   # clean:host/admin + /_admin/* endpoints
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── endpoint.rs             # bind admin listener (loopback by default, server §1.7)
│   │       ├── reload.rs               # POST /_admin/reload wire format (server §1.10.2)
│   │       ├── swap.rs                 # per-middleware swap (SRVH-03..SRVH-06, dev-mode only)
│   │       └── auth.rs                 # admin API auth (bearer token from [server] admin-auth)
│   │
│   ├── server-dev-socket/              # local reload socket for `cln dev` (server §1.10.2)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── unix.rs                 # $XDG_RUNTIME_DIR/clean-server-<pid>.sock (Linux/macOS)
│   │       └── windows.rs              # loopback TCP fallback for Windows
│   │
│   ├── server-diagnostics/             # health + metrics + capability manifest surfacing
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── health.rs               # GET /_health (server §1.9)
│   │       └── metrics.rs              # Prometheus-format /_metrics (optional, [server] metrics-path)
│   │
│   └── server-cli/                     # small helper binary for `cln doctor` style checks
│       └── src/                        # NOT the primary binary — that's clean-server
│           ├── main.rs                 # e.g. `clean-server check host.toml` for validation without starting
│           └── check.rs
│
├── testing/
│   ├── fake-guest/                     # tiny Clean-guest-shaped wasm that echoes requests
│   ├── fake-bridge/                    # bridge stub implementing one of the standard interfaces
│   ├── fixtures/                       # sample host.toml files, TLS test certs
│   └── conformance/                    # runs the Platform 16 host conformance suite
│
└── docs/
    └── (empty initially; ADR-0002 lands here)
```

**Why this shape:**

- `clean-server` binary is deliberately small — startup, listener, request marshaling, signal handling. Everything below the HTTP layer is delegated to `clean-host-core` (per CH-01: the library never owns I/O; server §1.11: "composition, bridge lifecycle, WASI, config parsing, instance pooling, capability manifest emission — all owned by clean-host-core").
- `server-envelope` is a separate crate because envelope implementations have a stable WIT interface with `clean-host-core::register_envelope` (host-core §8). Isolating them makes it obvious which HTTP-shaped state (cookies, CSRF, per-socket queues) the server owns vs. what bridges own.
- `server-admin` and `server-dev-socket` are separate because they're distinct trust models: admin API is authenticated + network-reachable; dev socket is filesystem-permission-gated + local-only (SRVH-08). Mixing them in one module is how those security properties get accidentally shared.
- `server-diagnostics` is separate because metrics are behind a feature flag — the base binary shouldn't pay the Prometheus dependency cost when metrics are off.

**Spec → module map:**

| Spec section | Module |
|---|---|
| §1.3 world implementation (clean:host/routing, request, response, websocket, sse) | `clean-server::routing`, `request_marshal`, `response_marshal`, `websocket`, `sse` |
| §1.4 HTTP listener + request flow | `clean-server::listener`, `request_loop` |
| §1.5 envelope impls | `server-envelope` |
| §1.6 [server] config block | `clean-server::config` |
| §1.7 security model (CSRF, cookies, TLS) | `server-envelope::session_http` (CSRF), `clean-server::listener` (TLS), `clean-server::config` (secure defaults) |
| §1.9 diagnostics (health, metrics, capability manifest) | `server-diagnostics` |
| §1.10 reload triggers | `server-admin::reload`, `server-dev-socket`, `clean-server::signals` |
| §1.10.1 per-middleware hot-swap (SRVH-03..SRVH-06) | `server-admin::swap` |
| §1.10.2 reload channel wire protocol (SRVH-07/08) | `server-admin::reload`, `server-dev-socket` |
| SRVH-01 (opt-in capabilities) | Enforced by clean-host-core's load-time contract; server doesn't add parallel checks |
| SRVH-02 (absent config = capability off) | Enforced by clean-host-core; server just refuses to start with a diagnostic |
| Platform 16 (host contract validation, HCV-06 CI parity) | `host.wit` at repo root + a CI job invoking `clean-host-core`'s shared parity-check tool |

---

## 3. Public API shape

Server is a binary, not a library. Nothing consumes it. The relevant public surface is:

**1. `host.wit` at the repo root** — HCV-02: this file declares the world the server fulfills. Every bridge validates against it; the compiler validates every project targeting the server world against it. It is a stable published contract, versioned per Platform 08.

**2. The `clean-server` binary CLI:**

```
clean-server <config-path>                    # start with host.toml at <config-path>
clean-server --check <config-path>            # validate config + guest imports, exit 0/1
clean-server --version                        # print version
```

That's the entire CLI. `cln run` (Manager §00.13) invokes this — that's how a `.clapp` or `.serve` bundle becomes a running HTTP endpoint.

**3. The reload channel wire protocol (server §1.10.2)** — canonical JSON, three ops (`reload-guest`, `reload-chain`, `swap-middleware`). This IS a stable API surface because `cln dev`, `cln reload`, and third-party dev-mode tools all speak it. Owned by [`schema/reload-channel.json.md`](https://github.com/cleanlanguage/foundation/blob/main/02%20components/hosts/clean-server/schema/reload-channel.json.md).

**4. `/_health`, `/_metrics`, `/_admin/*` endpoints** — the observability surface. Documented in server §1.9. Shape is fixed; anything an operator scripts against these MUST continue to work across minor versions.

Every other surface (internal crate APIs, module boundaries) is unstable and can change.

---

## 4. Build order

**Phase 0 — Skeleton + `clean-host-core` dependency.** *Blocks on the decision in §9 open question #1.*

- Cargo workspace per §2. `clean-server` binary compiles, prints `--version`, exits.
- `Cargo.toml` depends on `clean-host-core` and `clean-host-core-wasmtime` at pinned versions (or path deps to a sibling repo, depending on §9 outcome).
- `host.wit` at repo root — minimal skeleton declaring the `server` world of `clean:host@0.1.0` with just `clean:host/routing`, `request`, `response` (no bridges, no websocket, no sse yet).
- CI job that runs `clean-host-core`'s parity-check tool against `host.wit`. Fails until we implement the WIT-declared interfaces.

**Phase 1 — Hello-world HTTP.** *M0 milestone target.*

- `clean-server::config` reads `host.toml` (delegating `[host]`/`[guest]`/`[runtime]`/`[bridges]` to `clean-host-core::HostConfig`, parsing `[server]` locally).
- `clean-server::startup` constructs `clean-host-core::Host`, calls `compose()` — no bridges yet, just a bare guest with WASI + `clean:host/*`.
- `clean-server::listener` binds on `[server] listen` (default `127.0.0.1:3000`), accepts HTTP/1.1 (no TLS yet).
- `clean-server::request_loop` per §1.4.2 steps 1–7: parse, route (via a stub routing table — one hardcoded `GET /` for M0), checkout instance, invoke guest handler, marshal response.
- `clean-server::request_marshal` + `response_marshal` — minimal `clean:host/request` and `clean:host/response` mapping (method, path, headers, body).
- SIGTERM → `Host::shutdown(30s)`.

**M0 acceptance:** compile a Clean guest that exports a `GET /` handler returning "hello world", write a `host.toml`, run `clean-server host.toml`, curl `http://127.0.0.1:3000/`, see "hello world". Bridges: none composed. Reload: unimplemented.

**Phase 2 — Real routing + TLS + full HTTP surface.**

- Dynamic routing table populated from guest's `clean:host/routing` registrations at startup (guests declare routes; server matches them).
- TLS termination via `[server] tls` config (`rustls`).
- HTTP/2 via `hyper` protocol negotiation.
- WebSocket upgrade + `server-envelope::realtime_sockets` (implements the envelope but no realtime bridge composed yet — envelope calls just enqueue to per-connection buffers that the connection handler drains).
- SSE response framing.
- Body size limits, request timeouts, `queue-depth` 503 behavior.

**Phase 3 — Envelope + bridge composition.**

- `server-envelope::session_http` — cookies, CSRF token issuance & validation.
- Compose the session bridge (stub in-process implementation for tests; real Redis/Postgres bridges land in M1+ as they're separate repos).
- Compose data-bridge, kv-bridge, jobs-bridge (enqueue-only — worker is a separate host binary), mail-bridge, realtime-bridge.
- SRVH-01 enforcement: every capability the guest imports MUST have a `[bridges]` entry, else refuse to start with a diagnostic pointing at the missing key.
- Extend `host.wit` to declare every interface the world advertises. Update CI parity check.

**Phase 4 — Reload + hot-swap.**

- `clean-server::signals`: SIGHUP → `Host::reload()`.
- `server-admin::endpoint` binds `[server] admin-listen` (default loopback).
- `server-admin::reload` implements `POST /_admin/reload` per §1.10.2 wire protocol.
- `server-dev-socket` binds the local reload socket; framework's `cln dev` reload path lands here.
- `server-admin::swap` implements per-middleware hot-swap (SRVH-03..SRVH-06) — dev-mode only, refused in `deployment-mode = "production"`.

**Phase 5 — Diagnostics + observability.**

- `server-diagnostics::health` — `GET /_health` surfacing `clean-host-core::HealthReport`.
- Structured logs via `clean:host/log` → `tracing` → stderr (or JSON file per `[host] log-format`).
- `server-diagnostics::metrics` — optional Prometheus endpoint behind `[server] metrics-path` config.
- Trap snapshots (server §1.9): capture request context + WASM stack on guest trap, log locally or forward to `errors.cleanlanguage.dev` per config.

**Phase 6 — Security hardening + conformance.**

- CSRF enforcement on unsafe methods (opt-out per route via route-level attribute).
- HTML-escape default on framework-provided response helpers (framework-side; server just doesn't do anything that would re-un-escape).
- `[server] allow-plaintext = true` warning in `cln doctor` (manager-side; server just enforces the flag).
- Run the full Platform 16 host conformance suite. Fix every miss.

**Phase 7 — Performance targets.**

- Benchmark against §1.8 envelope: 10k–20k req/s on 4-core reference hardware.
- Profile any gap. Likely candidates: request parsing (hyper knobs), instance checkout (host-core pool tuning), envelope call marshaling.
- Only after M0–M6 land — premature optimization here is a distraction.

---

## 5. Testing strategy

Four layers because server has more surface than framework or manager.

**Layer A — unit tests per module.** Config parser, request/response marshaling, routing table matching, cookie serialization, CSRF token generation.

**Layer B — server-in-process tests with fake guest.** `testing/fake-guest/` compiles a minimal Clean-guest-shaped wasm that exports a handler and calls back into fake bridges. Boot `clean-server` in-process via a test harness, hit it with a real HTTP client, verify end-to-end. Every SRVH-* rule gets a test at this layer.

**Layer C — Platform 16 conformance suite.** `testing/conformance/` runs the shared host-conformance suite against `clean-server`. This is the go/no-go gate for shipping — a host that fails conformance doesn't ship (host model §6, principle P8).

**Layer D — performance benchmarks.** `criterion` for microbenchmarks (request parse, marshal); `wrk` / `oha` / `bombardier` for end-to-end throughput. Runs in CI but doesn't gate — a regression triggers a review, not a build failure (perf tests are noisy).

**Cross-cutting:**
- **Determinism.** Structured logs must be byte-deterministic given the same request sequence, so tests can assert against log output.
- **TLS testing.** `rcgen` for on-the-fly test certs; no hardcoded PEMs.
- **Windows.** `server-dev-socket` has a Windows branch; needs a per-OS test matrix. Same as manager.

---

## 6. Milestones

**M0 — HTTP hello-world.** *~3 weeks from starting, blocks on Phase 0 decision.*

Deliverables:
- Workspace, `host.wit` skeleton, CI parity check.
- Phase 1 complete: bare-guest HTTP/1 endpoint served, SIGTERM shutdown.
- Layer A tests for config, marshaling, routing.
- One Layer B test proving end-to-end HTTP → guest → response works.

Explicit non-goals: TLS, WebSocket, SSE, any bridge, reload, admin API, metrics.

**M1 — Full HTTP surface + bridges.** *~6 weeks after M0.*

Deliverables:
- Phase 2 complete: TLS, HTTP/2, WebSocket, SSE, dynamic routing.
- Phase 3 complete: envelope impls + all six standard bridges compose (with stub backends).
- `host.wit` extended to full `server` world of `clean:host@0.1.0`. CI parity check passes.
- Layer B test suite covering every WIT interface.

**M2 — Reload + observability + conformance.** *~4 weeks after M1.*

Deliverables:
- Phase 4 complete: SIGHUP reload, admin API, dev socket, per-middleware swap.
- Phase 5 complete: `/_health`, structured logs, optional metrics, trap snapshots.
- Phase 6 complete: security defaults, HCV-06 CI parity check, full Platform 16 conformance suite passing.

**M3 — Production-ready.** *~4 weeks after M2.*

Deliverables:
- Phase 7 performance work; §1.8 targets met on reference hardware.
- Deployment guides for Docker, systemd, launchd (docs, not code).
- Real production bridge implementations coordinated with bridge repos (Redis session bridge, Postgres data bridge, etc.). These are separate repos; this milestone just says "clean-server integrates with them cleanly."

**M4+ — Beyond v1.** `wasi:http/middleware` composition patterns (§1.4.4), fancier reload strategies (bridge hot-swap without full recompose), distributed tracing (`on_checkout`/`on_return` hooks).

---

## 7. Special considerations

Two things about clean-server that don't have parallels in framework or manager.

**Cross-repo coupling with `clean-host-core`.** Every WIT-interface addition or breaking change in `clean-host-core` forces a coordinated server release. This is fine architecturally (host model §3: hosts delegate to shared library) but requires discipline: pin `clean-host-core` version in `Cargo.toml`, bump deliberately, run conformance suite on every upgrade.

**Server is downstream of every bridge repo.** Sessions, data, kv, jobs, mail, realtime each live in their own repos with their own release cycles. Server composes them at startup by reading `.wasm` files listed in `host.toml [bridges]`. It doesn't Cargo-depend on them. So bridge repos ship independently — the server doesn't need a rebuild when a bridge cuts a new version, only a config update in whatever `host.toml` an operator writes.

This means M0 is achievable without any real bridge existing: we compose zero bridges, run a guest that imports zero capabilities beyond WASI. Every bridge is additive later.

---

## 8. Open questions

Answers proposed; confirm before Phase 0 (item #1) and Phase 3 (rest).

1. **Where does `clean-host-core` live?** Options:
   - **(a) Sibling repo `clean-host-core/`, published as a crate.** Every host (server, worker, cli, browser, edge) depends on it as a pinned crate version. **Proposal: this one.** Rationale: five hosts consume it; putting it inside `clean-server/` couples every other host to server releases; separate repo lets `clean-host-core` version independently per its own semver rules.
   - **(b) Inside `clean-server/` workspace, other hosts vendor or path-dep it.** Simpler build story for M0, but makes cross-host changes painful.
   - **(c) Inside `foundation/` as authoritative Rust ref implementation.** Blurs the "foundation is docs" boundary.

   **BLOCKING.** This decides the Cargo.toml on day one.

2. **`host.toml` location.** Spec says "clean-host-core reads a TOML file whose location is passed in by the concrete host" (host-core CLNH-10). Proposal: `clean-server` CLI takes it as a positional arg (`clean-server host.toml`). No search-path magic. If invoked via `cln run <clapp>`, the framework/manager extracts the bundled `host.toml` from the archive and passes the path.

3. **Guest wasm location under `cln run`.** `.clapp` and `.serve` bundles carry `app.wasm` / `wasm/server.wasm`. Manager extracts them to `~/.cln/cache/run/<sha>/`. Proposal: manager rewrites `[guest] wasm = "..."` in the extracted `host.toml` to point at the absolute path in the cache directory before launching server. Confirms server doesn't need archive-awareness.

4. **Version of the `server` world clean-server ships.** Spec says `@0.1` (server §1.3). Proposal: pin exactly that in `host.wit` at repo root; bump per Platform 08. M0 declares only a subset (routing, request, response); Phase 3 fills in the rest.

5. **Which bridges are shipped as stub in-process implementations for tests?** Real bridge impls live in separate repos and land at M3+. For M1/M2 testing, proposal: `testing/fake-bridge/` ships one canned implementation of each of the six standard interfaces (session, data, kv, jobs, mail, realtime), all in-memory, all zero-config. Not production; test-only. Confirm scope is one-per-interface, not multiple backends per.

6. **How does `clean-server` learn its own version for the capability manifest?** Same answer as framework and manager: `env!("CARGO_PKG_VERSION")`. Confirm.

7. **What does `clean-server` do if the guest wasm is unreachable at startup?** File not at `[guest] wasm`, or file isn't a valid component. Proposal: `HostError::Config` at startup, exit non-zero, structured error on stderr pointing at the config key + resolved path. No retry, no fallback. Aligns with CH-05 (no silent fallbacks).

8. **Windows-first vs Linux-first for M0.** Server runs on Linux, macOS, Windows (§1.2). Proposal: Linux + macOS in M0; Windows in M1 alongside per-OS reload socket work. `server-dev-socket::windows.rs` is meaningful work — 5% of code but 30% of the platform test matrix.

9. **Trap forwarding to `errors.cleanlanguage.dev`.** Server §1.9 says traps are "logged locally or forwarded per operator configuration." What does "per operator configuration" look like? Proposal: `[server] trap-report-url = "https://errors.cleanlanguage.dev/api/v1/traps"` opt-in, with `trap-report-consent = "full" | "code-only" | "off"` mirroring the client-side consent model. If omitted, traps only go to logs. Confirm.

10. **Admin API auth.** `POST /_admin/reload` requires the same auth as the reload channel (SRVH-08), but the spec doesn't say what that auth *is*. Proposal: bearer token from `[server] admin-auth = { bearer = "..." }` config. No auth = admin API refuses to start (loudly). Confirm.

---

## Metadata

- **Author:** server session (Ivo Pasco, 2026-08-09)
- **Status:** Draft for review
- **Owned decisions locked before writing:** Rust; Wasmtime via `clean-host-core-wasmtime`; hyper-based HTTP stack; rustls for TLS; server = HTTP host only in M0 (worker/cli/browser/edge deferred to their own repos); shared `cln-shared` crate for wire types.
- **Depends on:** `clean-host-core` (blocks Phase 0 pending §9 question #1); `cln-shared` from manager (for `.clapp` / `.serve` manifest reading if we do that here — currently manager extracts and rewrites, so server may not need this).
- **Depended on by:** manager's `cln run` path (invokes `clean-server` binary); framework's `cln dev` path (invokes `clean-server` and talks to it via the reload socket).
- **Next step after review:** decide §9 question #1 (clean-host-core location), convert accepted plan into ADR-0002 for the server, scaffold Cargo workspace, land Phase 0.
