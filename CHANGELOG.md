# Changelog

All notable changes to Clean Server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Post-1.0 / Unreleased] — current version `1.9.87`

This CHANGELOG was frozen at v1.0.0 (2024-12-07) and went unmaintained while
Clean Server's host bridge expanded substantially. Rather than invent a
retroactive per-version history (the bridge landed piecemeal across many
minor releases and the accurate mapping isn't in this file's git log), this
entry summarizes the aggregate delta between v1.0.0 and the current
`Cargo.toml` version.

### Host-bridge surface, then vs. now

- **v1.0.0 (2024-12-07):** 8 namespaces — `http`, `db`, `env`, `time`,
  `crypto`, `log`, `fs`, `sys`.
- **Current:** 476 unique bridge functions registered against the WASM
  `env` linker — 308 in **Layer 2** (`host-bridge/src/wasm_linker/`,
  portable across all hosts) and 168 in **Layer 3**
  (`src/bridge.rs`, server-only).

### Major bridge namespaces added since v1.0.0

Grouped by prefix, with rough function counts per group (source: grep of
`func_wrap("env", …)` and `register_bridge_fn!(linker, …)` in this repo):

**Layer 2 — portable (`host-bridge/src/wasm_linker/`):**

- `math.*` (~96) — full IEEE-754 math + Clean stdlib coverage.
- `string.*` (~49) — UTF-8-aware string ops, formatting, parsing.
- `http.*` (~27) — HTTP client (GET/POST/PUT/…/response inspection).
- `db.*` (~19) — SQL query, exec, transactions, connection state.
- `time.*` (~16) — epoch, ISO, timezone offsets, sleep.
- `crypto.*` (~16) — hashing, HMAC, UUID, random, JWT helpers.
- `file.*` / `fs.*` (~16) — file I/O, including opaque byte-handle bridges.
- `array.*` / `list.*` (~23) — heap collection operations.
- `print*` / `console.*` (~11) — stdout formatting for primitives.
- `env.*` (~7), `input.*` (~5), `arena.*` (~4), `state.*` (~4),
  plus scalar coercions (`int`, `boolean`, `float`, `number`, …).

**Layer 3 — server-only (`src/bridge.rs`):**

- `_http_*` (~26) — routing (`_http_route`, `_http_ws_route`,
  `_http_sse_route`), response helpers (`_http_json`, `_http_html`,
  `_http_redirect`, `_http_not_found`, …), CORS, cache, cookies.
- `_req_*` (~24) — request context (method, path, headers, cookies,
  form/body/JSON parsing, params/queries, IP, auth token).
- `_ui_*` (~19) — server-side UI helpers (component HTML registration,
  page rendering, layouts, patches, iframe messaging).
- `_auth_*` (~16) — authentication guards, session roles, user identity,
  password-reset tokens.
- `_session_*` (~15) — session lifecycle (create, extend, destroy, claim,
  CSRF tokens, key/value store).
- `_job_*` (~12) — background jobs (enqueue, cancel, register, retry,
  status, results).
- `_i18n_*` (~8) — locale, translation lookup, currency/date/number
  formatting.
- `_ws_*` (~8) — WebSocket lifecycle, send, broadcast, room membership.
- `_mcp_*` (~7) — Model Context Protocol transport (stdio + HTTP + SSE).
- `_res_*` (~6) — response mutation (status, body, JSON, redirect,
  download, headers).
- `_sse_*` (~5) — Server-Sent Events emit/close/retry.
- `_email_*` (~3), `_role_*` / `_roles_*` (~3), `_json_*` (~3),
  `_test_*` (~3), `_async_*` (~2), `_schedule_*` (~2),
  plus singletons: `_dev_snapshot`, `_island_register`,
  `_jwt_refresh_and_rotate`, `_cors_configure`, `_rate_limit_configure`,
  `_server_sleep`.

### Authoritative sources

The counts above are approximate and derived from source grep. The
authoritative registry is:

- **`foundation/spec/platform/function-registry.toml`** — machine-readable
  signature registry for every Layer 2 + Layer 3 bridge function.
- **`foundation/spec/platform/HOST_BRIDGE.md`** — Layer 2 contract.
- **`foundation/spec/platform/SERVER_EXTENSIONS.md`** — Layer 3 contract.

### CI parity

Drift between the shipped bridge implementation and
`function-registry.toml` is caught by
`foundation/management/scripts/check_host_parity.py --host server --strict`,
run as part of the `comita` pipeline. Two in-repo tests
(`host-bridge/src/wasm_linker/mod.rs::test_spec_compliance` for Layer 2 and
`src/bridge.rs::test_layer3_spec_compliance` for Layer 3) additionally
instantiate every registered signature against the WASM linker.

### Note

Future changes should be recorded per-release under a proper
`## [x.y.z] - YYYY-MM-DD` heading below.

## [1.0.0] - 2024-12-07

### Added

- Initial release of Clean Server
- WebAssembly runtime using Wasmtime 26.0
- HTTP server built on Axum with async/await support
- Complete Host Bridge implementation with 8 namespaces:
  - `bridge:http` - HTTP client operations
  - `bridge:db` - Database operations (PostgreSQL, MySQL, SQLite)
  - `bridge:env` - Environment variables
  - `bridge:time` - Time and date operations
  - `bridge:crypto` - Cryptographic functions (bcrypt, argon2, JWT, SHA-256)
  - `bridge:log` - Structured logging
  - `bridge:fs` - Filesystem operations
  - `bridge:sys` - System information
- Dynamic route registration from WASM modules
- Request/response handling with JSON support
- CORS support with configurable origins
- Configurable request body size limits
- Verbose logging mode for debugging
- Multi-platform support (Linux x64/ARM64, macOS x64/ARM64, Windows x64)
- GitHub Actions CI/CD for automated releases
- Comprehensive documentation (README, CONTRIBUTING, LICENSE)

### Technical Details

- Rust 2021 edition
- Zero-copy WASM memory access where possible
- Connection pooling for HTTP and database clients
- Async runtime using Tokio
- Secure defaults (HTTPS, HTTP-only cookies, etc.)
- Production-ready performance (< 100ms cold start, < 1ms p50 latency)

### Security

- WASM sandboxing for application code
- Host Bridge allowlisting for system access
- SQL injection prevention via parameterized queries
- XSS prevention via automatic HTML escaping
- CSRF protection built-in
- Secure cookie handling

### Performance

- Optimized release builds with LTO
- Minimal memory footprint (< 50MB baseline)
- High throughput (> 10,000 req/sec for simple endpoints)
- Fast WASM execution via Wasmtime JIT compilation

[Unreleased]: https://github.com/Ivan-Pasco/clean-server/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Ivan-Pasco/clean-server/releases/tag/v1.0.0
