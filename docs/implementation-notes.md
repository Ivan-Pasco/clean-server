> Moved from `clean-language-foundation` — `02 components/hosts/clean-server/implementation-notes.md`, at foundation commit c72d093 (2026-09-11). Informative: the law it refers to lives in foundation, cited by id; nothing here binds another implementation (SDD-02).

# Clean-Server — Reference Implementation Notes

An informative appendix recording what the reference implementation of `clean-server` happens to be built with today — which WASM engine, which async runtime, which HTTP layer, which database and mail bridges ship in the distribution. This file is not part of the specification and is not normative for other host implementors; the contracts live in 01 — Server. What is here helps a contributor to the reference codebase know which crates the tree uses, and helps an author of an alternative host know what is a *contract* vs what is merely *our choice*.

---

## Why this file exists

The clean-server specification defines *contracts*: the interfaces of the `server` world of `clean:host`, the async host-function contract, instance pooling, execution interruption, composed bridge components, and statelessness. Under SDD-02, a specification states what is observable from outside, never the mechanism — a rule that only one implementation could satisfy is a design memo, not a spec.

But the reference implementation does make mechanism choices, and they are worth recording. A contributor to the reference codebase needs to know which crates the tree uses; an author of an alternative host needs to know what is a *contract* (in 01 — Server) and what is merely *our choice* (here).

This file records the second kind. Nothing here binds another host implementation — an implementation using a different WASM engine, a thread-per-core runtime, or a different HTTP layer is fully conformant as long as it satisfies the contracts in the server spec. The governance rule that keeps mechanism out of the spec and here is SDD-02.

---

## 1. What the reference host is built with

These are the crates and libraries the reference `clean-server` source tree depends on. They are visible in `Cargo.toml`. Changing any of them is a code change in the clean-server repo; it does not require a spec revision, but it does deserve an update to this file so the record stays honest.

### 1.1 WASM engine

- **[Wasmtime](https://wasmtime.dev/)** — the WebAssembly engine the host embeds.
  - Configured with `async_support(true)` so host functions can suspend without blocking a runtime thread — required by the async host-function contract in 01 — Server §1.5.
  - Configured with the **pooling allocator** for instance memory, so instance creation is cheap enough for per-request pooling in §1.6.
  - Configured with **epoch-based interruption** so long-running guest work can be cancelled from the outside — required for the timeout and shutdown paths in §1.7.

An alternative engine (wasmer, WasmEdge, or a future one) satisfies the contracts iff it provides equivalents to those three features.

### 1.2 Async runtime

- **[Tokio](https://tokio.rs/)** — the async runtime the host and its bridges are written against. The `async_support(true)` mode above ties Wasmtime's suspension points to Tokio tasks.

An alternative host could use a thread-per-core runtime (e.g. `glommio`) if it wired equivalent suspension semantics.

### 1.3 HTTP surface

- **[Axum](https://github.com/tokio-rs/axum)** over **[Hyper](https://hyper.rs/)** — the HTTP router and low-level HTTP implementation the reference host uses to accept requests, route them to the right composed component, and stream responses.

An alternative host could implement the HTTP surface with any RFC 7230/9110-conformant server library.

### 1.4 TLS

- **[rustls](https://github.com/rustls/rustls)** — the TLS implementation terminating HTTPS in the reference host. An alternative host may terminate TLS with any conformant stack, or delegate termination to a fronting proxy.

### 1.5 Engine adapter

- **`clean-host-wasmtime`** — the shared engine-adapter crate from clean-host-core through which the reference host embeds Wasmtime. The wasmtime memory/limits configuration it applies is recorded in clean-host-core's implementation notes.

---

## 2. What the reference host talks to over standard protocols

Clean does not bundle databases or mail servers. It ships bridge components that speak *standard protocols* to whatever the deployer runs. The following are the bridge components shipped with the reference clean-server distribution — they define which protocols are supported out of the box, not which vendor products the deployer must run.

### 2.1 Database bridges

Bridge components that fulfil the `clean:host/database` interface, one per SQL dialect:

- **PostgreSQL** — via the `postgres-bridge` component, speaking the PostgreSQL wire protocol. Compatible with any PostgreSQL-wire-compatible database (self-hosted PostgreSQL, managed services, CockroachDB, etc.).
- **MySQL** — via the `mysql-bridge` component, speaking the MySQL wire protocol. Compatible with MySQL, MariaDB, and MySQL-wire-compatible databases.
- **SQLite** — via the `sqlite-bridge` component, embedded. Used in dev and for single-node deployments where an external database is overkill.

A new database backend joins the ecosystem by shipping a new bridge component targeting the interface in bridges/data-bridge (pure-Wasm or Wasm-shim-over-sidecar); no spec change needed.

### 2.2 Mail bridge

- **SMTP** — via the `smtp-bridge` component, speaking [RFC 5321](https://datatracker.ietf.org/doc/html/rfc5321). Sends mail to any SMTP-conformant relay (self-hosted, or any transactional-mail provider that accepts SMTP).

The bridge speaks the protocol; the deployer chooses the relay.

---

## 3. What this file does NOT cover

Explicitly outside the scope of this document:

- **Session storage, job queues, real-time pub/sub, rate limiting.** These are *deployment infrastructure* the operator chooses and runs — they are not part of Clean, they are not shipped with Clean, and the reference host does not recommend specific products for them. The clean-server spec (§1.4c, §1.5.4) defines the *interfaces* these backplanes must satisfy (`clean:host/session`, `clean:host/jobs`, `clean:host/ws`, `clean:host/sse`); any backplane that implements the interface works. Choosing between vendors is a tutorial-level or deployment-guide topic, not implementation-notes material for Clean itself.
- **Container images, Kubernetes manifests, deployment recipes.** Not shipped with Clean. Community deployment guides may cover these.
- **Alternative-host implementations.** Non-reference hosts (a browser runtime, a CLI runner, a serverless-function runner, a third-party server) are governed by the contracts in the platform and host specs, not by this file.

---

## Metadata

- **Status:** Accepted (2026-08-02)
- **Audience:** Contributors to the reference `clean-server` codebase; readers wanting to know what the contracts in 01 — Server were validated against
- **Part of:** Clean Language Specification — Hosts / clean-server
- **References:** clean-server §1 — Server specification, data-bridge specification
