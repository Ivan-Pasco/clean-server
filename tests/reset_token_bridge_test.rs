//! Bridge registration tests for `_auth_create_reset_token` and
//! `_auth_consume_reset_token`.
//!
//! Verifies the server linker resolves both the canonical names and their
//! `auth.create_reset_token` / `auth.consume_reset_token` dot aliases with
//! the signatures declared in `foundation/spec/platform/function-registry.toml`.
//!
//! Security-critical semantics (single-use consume, expiry) are covered by
//! unit tests on `SessionStore::store_reset_token` / `consume_reset_token`
//! in `src/session.rs`.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

fn make_store(engine: &Engine) -> Store<WasmState> {
    let router = Arc::new(Router::new());
    Store::new(engine, WasmState::new(router))
}

fn probe(name: &str, params: &str, result: &str) {
    let engine = Engine::default();
    let linker = create_linker(&engine).expect("Failed to create server linker");
    let wat = format!(
        r#"(module (import "env" "{name}" (func {params} {result})))"#,
        name = name,
        params = params,
        result = result,
    );
    let module = Module::new(&engine, &wat)
        .unwrap_or_else(|e| panic!("failed to compile probe WAT for {}: {}", name, e));
    let mut store = make_store(&engine);
    linker.instantiate(&mut store, &module).unwrap_or_else(|e| {
        panic!(
            "linker could not resolve `env::{}` with signature {} -> {}: {}",
            name, params, result, e
        )
    });
}

#[test]
fn create_reset_token_canonical_and_alias() {
    // (user_id: i64, ttl_seconds: i64) -> i32 (length-prefixed string ptr)
    probe(
        "_auth_create_reset_token",
        "(param i64 i64)",
        "(result i32)",
    );
    probe("auth.create_reset_token", "(param i64 i64)", "(result i32)");
}

#[test]
fn consume_reset_token_canonical_and_alias() {
    // (token_ptr: i32, token_len: i32) -> i64 (user_id or 0)
    probe(
        "_auth_consume_reset_token",
        "(param i32 i32)",
        "(result i64)",
    );
    probe(
        "auth.consume_reset_token",
        "(param i32 i32)",
        "(result i64)",
    );
}
