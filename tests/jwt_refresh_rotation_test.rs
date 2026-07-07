//! Bridge registration test for `_jwt_refresh_and_rotate`.
//!
//! Verifies that the server linker resolves both the canonical name and the
//! `jwt.refresh_and_rotate` dot alias with the signature declared in
//! `foundation/platform-architecture/function-registry.toml`.
//!
//! Security-critical semantics (single-use rotation, replay rejection) are
//! covered by unit tests on `SessionStore::mark_jti_consumed` /
//! `is_jti_consumed` in `src/session.rs`.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

fn make_store(engine: &Engine) -> Store<WasmState> {
    let router = Arc::new(Router::new());
    Store::new(engine, WasmState::new(router))
}

#[test]
fn jwt_refresh_and_rotate_is_registered_with_canonical_and_alias() {
    let engine = Engine::default();
    let linker = create_linker(&engine).expect("Failed to create server linker");

    // (token_ptr, token_len, secret_ptr, secret_len, algo_ptr, algo_len,
    //  new_ttl_seconds: i64) -> i32
    let params = "(param i32 i32 i32 i32 i32 i32 i64)";
    let result = "(result i32)";

    for name in ["_jwt_refresh_and_rotate", "jwt.refresh_and_rotate"] {
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
                "linker could not resolve `env::{}` with signature \
                 (i32 i32 i32 i32 i32 i32 i64) -> i32: {}",
                name, e
            )
        });
    }
}
