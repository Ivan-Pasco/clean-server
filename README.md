# clean-server

The reference Clean host for HTTP applications.

`clean-server` owns the HTTP surface — listener, request parsing, response
writing, and later WebSocket and SSE — and delegates everything else
(composition, bridges, WASI, pooling, config parsing, capability manifest) to
[`clean-host-core`](../clean-host-core/).

Specification: `foundation/02 components/hosts/clean-server/01-server.md`.
Build plan: [PLAN.md](PLAN.md).

## Quick start

```bash
# Build the acceptance guest and the bridge it imports (needs wasm-tools).
./testing/fake-guest/build.sh
./testing/fake-bridge/build.sh

# Serve it.
cargo run --bin clean-server -- testing/fixtures/hello-world/host.toml

# In another shell:
curl http://127.0.0.1:3000/     # -> hello world
```

## CLI

```
clean-server <CONFIG>                 # start with host.toml at <CONFIG>
clean-server --check <CONFIG>         # validate config + guest, exit 0/1
clean-server parity --wit host.wit    # HCV-06 CI check
clean-server conformance              # CMOD-03 shipping gate
clean-server --version
```

The config path is a positional argument with no search-path magic. When
invoked through `cln run`, the manager extracts the bundled `host.toml` and
passes its path.

## `host.wit` is the contract

[`host.wit`](host.wit) at the repo root is the authoritative declaration of what
this host provides (HCV-02). The framework fetches it to validate projects at
build time; `clean-server` compares a guest's embedded WIT against it before
instantiating, refusing non-compliant guests with `COM017` rather than letting
them trap.

CI enforces both halves of HCV-06 on every commit: `host.wit` must parse, and
it must match the interfaces the wasmtime `Linker` actually registers — no
missing entries, no extra entries, and no no-op or throwing stubs.

## Status — Phase 6 complete

Working: HTTP/1.1 and HTTP/2 listeners, TLS termination with ALPN protocol
negotiation, dynamic routing with path parameters and wildcards, WebSocket
upgrade with a bounded per-socket outbound queue, Server-Sent Events, body-size
limits, per-request timeouts via epoch interruption, queue-depth load shedding,
`--check`, SIGTERM graceful drain, structured per-request logs, capability
manifest emission, and the HCV-06 parity check.

Bridge composition works: `clean-host-core` discovers each `[bridges]` entry,
validates it really exports what it promised, and composes it into the guest
with WAC. A guest that imports a capability with no matching `[bridges]` entry
is refused at startup with a diagnostic naming the exact key to add (SRVH-01 /
SRVH-02) — never started with the capability silently off.

Both host-side envelopes are implemented: `session-envelope` (cookies with
HttpOnly and Secure floors, CSRF issuance and constant-time validation) and
`realtime-sockets` (delivery, force-close and queue depth on the socket
registry). CSRF is enforced in the listener, so an unsafe method with a bad
token is rejected before the guest runs rather than depending on every handler
to remember.

`host.wit` declares seven interfaces, each really registered in the wasmtime
`Linker`. `clean:host/admin` lands in Phase 4 and is added when it is
implemented, never before.

Reload has three triggers, all speaking one wire protocol so `cln dev`,
`cln reload` and cluster orchestrators share a transport: **SIGHUP** for
process supervisors, an authenticated **`POST /_admin/reload`** on its own
loopback listener, and a **local dev socket** at
`$XDG_RUNTIME_DIR/clean-server-<pid>.sock`. A failed reload keeps the previous
guest serving (CLNH-53) rather than taking the process down.

The two trust models are deliberately different. The admin API is
network-reachable, so it requires a bearer token and refuses to start without
one (SRVH-08). The dev socket must *not* require auth per the same rule — its
access control is filesystem permissions, so it is created `0600`, removed on
exit, and never started in production.

Diagnostics are live (§1.9). `GET /_health` reports liveness, pool state,
composed bridges, the last reload's outcome and any recent trap — answering 503
rather than 200 when the host has not composed, because a load balancer reads
the status code. `[server] metrics-path` optionally exposes Prometheus text;
it is off by default so the base binary pays nothing for it. Guests and bridges
emit structured records through `clean:host/log`, each tagged with the
correlation id of the request that produced it, and a guest trap is captured
with its request context rather than vanishing into a stack trace.

CSRF is enforced in the listener on unsafe methods, and a route may opt out at
registration (`csrf = false`) when it authenticates callers another way — a
webhook checking an HMAC has no token to present and no browser form to
protect. A malformed options record leaves CSRF *on*: a parse slip must not
silently disable a security control.

### Conformance is INCOMPLETE, and says so

`clean-server conformance` runs the CMOD-03 gate (Platform 15 §10.1). Two of
its four checks work today and pass: every interface the world advertises is
really provided, and nothing outside the world is registered. The other two
need the canonical corpus at `tests/cln/conformance/`, which does not exist and
cannot be populated until the compiler emits Component Model components.

The suite therefore reports `INCOMPLETE` and **exits non-zero**. It does not
report green, because a host that has not run every check has not been shown to
conform, and a passing gate that never ran half its checks is worse than no
gate. CI runs it with `continue-on-error` so the gap stays visible on every
build without blocking one; when the corpus lands, that line comes out.

Not yet implemented, by design: the five remaining standard bridges (data, kv,
jobs, mail, realtime — no component exists for any of them yet) and
per-middleware hot-swap. `swap-middleware` is answered with a
`refused` rather than a false success: there is no `[http-chain]` to mutate
(the block is not in the canonical `host.toml` schema, and
`wasi:http/middleware` is unavailable in the toolchain).

### Protocol selection

Under TLS, ALPN decides: the handshake reports `h2` or `http/1.1`. Without TLS,
HTTP/2 has no negotiation, so h2c is detected from the connection preface
rather than assumed — otherwise enabling `h2` would break every plaintext
HTTP/1.1 client.

### The acceptance guest is hand-written WAT

`testing/fake-guest/` is a Component Model component written directly in WAT.
It serves five routes covering every interface: `GET /`, `GET /users/:id`,
`GET /events` (SSE), `GET /ws` (WebSocket), and `POST /echo`.
The installed compiler (cln 0.33.154) emits core wasm modules rather than
components and generates no `clean:host/*` imports, so it cannot yet build a
guest for the server world. The fixture imports the same interfaces `host.wit`
declares, so it functions as a contract test and will be replaced by a real
`cln build` output once the compiler emits components.

## Repository layout

```
host.wit                          the published contract (HCV-02)
crates/clean-server/              the binary
  src/config.rs                   the [server] block
  src/guest.rs                    clean:host/* Linker registration
  src/routing.rs                  route matching, params, wildcards
  src/startup.rs                  Host construction, compose, dispatch
  src/listener.rs                 hyper listener, TLS, protocol select
  src/tls.rs                      rustls acceptor and ALPN
  src/websocket.rs                upgrade handshake and socket task
  src/sockets.rs                  outbound queues, backpressure, SSE framing
  src/envelope.rs                 cookies, CSRF, envelope rendering
  src/conformance.rs              the CMOD-03 gate
  src/diagnostics.rs              health, metrics, trap snapshots
  src/reload.rs                   reload-channel wire protocol
  src/admin.rs                    reload triggers: admin API, dev socket
  src/main.rs                     CLI, signals, parity subcommand
  tests/support/mod.rs            shared end-to-end harness
  tests/acceptance.rs             M0 acceptance
  tests/phase2.rs                 routing, TLS, h2, SSE, WebSocket, limits
  tests/phase3.rs                 composition, SRVH-01/02, CSRF
  tests/phase4.rs                 reload triggers, admin auth, dev socket
  tests/phase5.rs                 health, metrics, guest logging
  tests/phase6.rs                 CSRF opt-out, escaping, conformance
testing/fake-guest/               the acceptance guest + build.sh
testing/fake-bridge/              a component to compose, for tests
testing/fixtures/hello-world/     the acceptance host.toml
```

Crates the plan defers to later phases (`server-envelope`, `server-admin`,
`server-dev-socket`, `server-diagnostics`) are not scaffolded yet — empty
crates would be noise.

## Performance

Measured on an Apple M2: **33 000 req/s** on the hello-world route with 100%
success, **53–63 ms** cold start, **126 ns** instance checkout. All three sit
inside the §1.8 envelope.

Those numbers describe host overhead on this machine, not a §1.8
certification — the acceptance guest does no I/O, so a real handler making
database calls will be dominated by those instead. [docs/performance.md](docs/performance.md)
gives the full results, the method, and what they do not establish.

```bash
./testing/bench/run.sh          # end-to-end throughput (needs oha)
./testing/bench/coldstart.sh    # cold start
cargo bench --bench hot_path    # per-request host work
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --bin clean-server -- parity --wit host.wit
```

`clean-host-core` is consumed as a path dependency from a sibling checkout
while both repos are pre-1.0; clone it next to this one. These become git or
registry pins at M1.

CI lays the sibling out the same way, which needs credentials: `clean-host-core`
is private, and the default `GITHUB_TOKEN` is scoped to this repository only.
The workflow authenticates with `secrets.CLEAN_HOST_CORE_DEPLOY_KEY` — an SSH
deploy key registered read-only on `clean-host-core`. A deploy key is bound to
exactly one repository, so a leak exposes read access to that repo and nothing
else; a PAT would carry the whole account's scopes into this repo's secrets.
