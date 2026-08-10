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
# Build the acceptance guest (needs wasm-tools).
./testing/fake-guest/build.sh

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

## Status — Phase 2 complete

Working: HTTP/1.1 and HTTP/2 listeners, TLS termination with ALPN protocol
negotiation, dynamic routing with path parameters and wildcards, WebSocket
upgrade with a bounded per-socket outbound queue, Server-Sent Events, body-size
limits, per-request timeouts via epoch interruption, queue-depth load shedding,
`--check`, SIGTERM graceful drain, structured per-request logs, capability
manifest emission, and the HCV-06 parity check.

`host.wit` declares five interfaces — `routing`, `request`, `response`,
`websocket`, `sse` — and each is really registered in the wasmtime `Linker`.
The rest of the `clean:host/server@0.1` world is added as each part is
implemented, never before; declaring an interface the Linker does not register
is exactly what HCV-06 exists to catch.

In particular `clean:realtime/sockets` is **not** declared yet. It exists so
the realtime bridge can call back into the host, and bridge composition is
Phase 3 — an interface with no reachable caller would be a promise the server
cannot yet keep. The socket machinery it will sit on (queue, backpressure,
force-close) is built and tested now.

Not yet implemented, by design: bridges, the session envelope and CSRF, reload
and hot-swap, the admin API, and metrics. A `[bridges]` entry is a startup
error rather than being silently ignored.

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
components and generates no `clean:http/*` imports, so it cannot yet build a
guest for the server world. The fixture imports the same interfaces `host.wit`
declares, so it functions as a contract test and will be replaced by a real
`cln build` output once the compiler emits components.

## Repository layout

```
host.wit                          the published contract (HCV-02)
crates/clean-server/              the binary
  src/config.rs                   the [server] block
  src/guest.rs                    clean:http/* Linker registration
  src/routing.rs                  route matching, params, wildcards
  src/startup.rs                  Host construction, compose, dispatch
  src/listener.rs                 hyper listener, TLS, protocol select
  src/tls.rs                      rustls acceptor and ALPN
  src/websocket.rs                upgrade handshake and socket task
  src/sockets.rs                  outbound queues, backpressure, SSE framing
  src/main.rs                     CLI, signals, parity subcommand
  tests/support/mod.rs            shared end-to-end harness
  tests/acceptance.rs             M0 acceptance
  tests/phase2.rs                 routing, TLS, h2, SSE, WebSocket, limits
testing/fake-guest/               the acceptance guest + build.sh
testing/fixtures/hello-world/     the acceptance host.toml
```

Crates the plan defers to later phases (`server-envelope`, `server-admin`,
`server-dev-socket`, `server-diagnostics`) are not scaffolded yet — empty
crates would be noise.

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
