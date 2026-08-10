//! Startup: build the `clean-host-core` Host, wire the server's own
//! interfaces, compose, and collect the guest's routes.
//!
//! §1.11 — composition, WASI, pooling, config parsing, and the capability
//! manifest all belong to `clean-host-core`. This module is the seam where the
//! server hands those responsibilities over and keeps only what it owns.

use std::sync::{Arc, Mutex};

use clean_host_core::{Host, HostConfig, HostProvided};
use clean_host_core_wasmtime::{EngineConfig, WasmtimeInstance, WasmtimeRuntime};
use wasmtime::component::Val;

use crate::config::ServerConfig;
use crate::guest::{self, Exchange, ExchangeRef};
use crate::routing::Router;

/// Everything the request loop needs, assembled once at startup.
pub struct Runtime {
    pub host: Host,
    pub server: ServerConfig,
    pub router: Router,
}

/// Build the runtime from a `host.toml` path.
pub fn boot(config_path: &std::path::Path) -> anyhow::Result<Runtime> {
    let host_config = HostConfig::load(config_path)?;

    for warning in &host_config.warnings {
        tracing::warn!(target: "clean_server::config", "{warning}");
    }

    let server = ServerConfig::from_host_config(&host_config)?;
    let host = build_host(host_config, server.request_timeout)?;

    // Composition must happen before the guest can be asked for its routes.
    host.compose()?;
    host.emit_manifest()?;

    let routes = collect_routes(&host)?;
    let router = Router::new(routes, &server.mount);

    if router.is_empty() {
        // Not fatal — a guest may legitimately register nothing — but it is
        // almost always a mistake, and silence here becomes a confusing 404.
        tracing::warn!(
            target: "clean_server::startup",
            "guest registered no routes; every request will return 404"
        );
    }

    Ok(Runtime {
        host,
        server,
        router,
    })
}

fn build_host(config: HostConfig, request_timeout: std::time::Duration) -> anyhow::Result<Host> {
    // §1.4.3: `[server] request-timeout` is enforced by epoch interruption, so
    // the deadline is that timeout expressed in `[runtime] epoch-interval`
    // ticks. At least one tick, or a sub-interval timeout would trap instantly.
    let interval = config.runtime.epoch_interval;
    let ticks = (request_timeout.as_nanos() / interval.as_nanos().max(1)).max(1) as u64;

    let mut runtime = WasmtimeRuntime::new(EngineConfig {
        pooling: config.runtime.pooling,
        memory_max: config.runtime.memory_max,
        epoch_interruption: true,
        epoch_interval: interval,
        epoch_deadline_ticks: ticks,
    })?;

    // The server's own interfaces (§1.3.1). These are what `host.wit` declares
    // and what the HCV-06 parity check verifies.
    guest::register(runtime.linker_mut())?;

    let mut host = Host::new(config, Box::new(runtime))?;
    host.set_host_provided(HostProvided {
        interfaces: guest::registered_interfaces()
            .into_iter()
            .map(|r| r.interface)
            .collect(),
    });

    Ok(host)
}

/// Invoke the guest's `init` export so it can register its routes.
///
/// The routes are read out of the exchange the guest wrote into, then the
/// instance is returned to the pool.
fn collect_routes(host: &Host) -> anyhow::Result<Vec<guest::Route>> {
    let exchange: ExchangeRef = Arc::new(Mutex::new(Exchange::new()));
    let mut guard = host.checkout()?;

    let instance = guard
        .downcast_mut::<WasmtimeInstance>()
        .ok_or_else(|| anyhow::anyhow!("runtime adapter is not the wasmtime adapter"))?;

    instance.store.data_mut().host_context = Some(Box::new(Arc::clone(&exchange)));

    let component = instance.component.clone();
    let linker = Arc::clone(&instance.linker);
    let wasm_instance = linker.instantiate(&mut instance.store, &component)?;
    instance.instance = Some(wasm_instance);

    // `init` is optional: a guest with no routes to register need not export it.
    if let Some(func) = wasm_instance.get_func(&mut instance.store, "init") {
        func.call(&mut instance.store, &[], &mut [])?;
    }

    instance.store.data_mut().host_context = None;
    drop(guard);

    let routes = exchange.lock().unwrap().routes.clone();
    for route in &routes {
        tracing::info!(
            target: "clean_server::startup",
            method = %route.method,
            path = %route.path,
            handler = route.handler_id,
            "route registered"
        );
    }
    Ok(routes)
}

/// Run one request through the guest.
///
/// Steps 3–7 of §1.4.2: check out an instance, marshal the request in, invoke
/// the handler, marshal the response out, return the instance.
pub fn dispatch(host: &Host, handler_id: u32, exchange: Exchange) -> anyhow::Result<Exchange> {
    let shared: ExchangeRef = Arc::new(Mutex::new(exchange));
    let mut guard = host.checkout()?;

    let instance = guard
        .downcast_mut::<WasmtimeInstance>()
        .ok_or_else(|| anyhow::anyhow!("runtime adapter is not the wasmtime adapter"))?;

    instance.store.data_mut().host_context = Some(Box::new(Arc::clone(&shared)));

    let component = instance.component.clone();
    let linker = Arc::clone(&instance.linker);
    let wasm_instance = linker.instantiate(&mut instance.store, &component)?;

    // A guest that registered a route must export `handle`; its absence is a
    // contract violation worth naming precisely.
    let func = wasm_instance
        .get_func(&mut instance.store, "handle")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "guest registered routes but does not export `handle`; \
                 the world requires it (see host.wit)"
            )
        })?;

    let result = func.call(&mut instance.store, &[Val::U32(handler_id)], &mut []);
    instance.store.data_mut().host_context = None;
    drop(guard);

    result?;

    let exchange = shared.lock().unwrap().clone();
    Ok(exchange)
}
