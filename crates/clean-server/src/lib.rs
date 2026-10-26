//! `clean-server` internals, exposed as a library.
//!
//! The binary in `main.rs` is the product; this target exists so benchmarks
//! and integration tests can reach the per-request hot path without going
//! through a process boundary. Nothing here is a stable API — the published
//! contract is `host.wit` and the CLI.

pub mod admin;
pub mod config;
pub mod conformance;
pub mod diagnostics;
pub mod entrypoint;
pub mod envelope;
pub mod guest;
pub mod listener;
pub mod reload;
pub mod routing;
pub mod sockets;
pub mod startup;
pub mod tls;
pub mod websocket;
