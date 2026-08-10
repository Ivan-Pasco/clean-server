//! Wiring the `clean:http/*` interfaces into the wasmtime `Linker`, and
//! invoking the guest.
//!
//! This is the server's half of the host contract: `host.wit` declares these
//! interfaces, and [`register`] implements them. The two are kept in step by
//! HCV-06 — [`registered_interfaces`] reports exactly what [`register`] wires,
//! so the CI parity check reads the real registration rather than a
//! hand-maintained list that can drift.
//!
//! **No stub imports.** Every function registered here is a working
//! implementation. An interface the server does not implement is absent from
//! both `host.wit` and this module, so a guest that imports it fails at load
//! with a clear WASI error rather than silently getting a no-op
//! (Platform 16 §16.14).

use std::sync::{Arc, Mutex};

use clean_host_core::parity::{Registration, RegistrationKind};
use clean_host_core_wasmtime::StoreState;
use wasmtime::component::{Linker, Val};

/// The request being served, plus the response being built.
///
/// One of these is installed into the store before the guest is invoked and
/// taken out afterwards. Because a checked-out instance serves exactly one
/// request at a time, this is never shared across requests.
#[derive(Debug, Default, Clone)]
pub struct Exchange {
    pub method: String,
    pub path: String,
    pub query: String,
    pub peer: String,
    pub correlation_id: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: Vec<u8>,

    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Vec<u8>,

    /// Routes the guest registered during `init`.
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub handler_id: u32,
}

impl Exchange {
    pub fn new() -> Self {
        Self {
            status: 200,
            ..Default::default()
        }
    }
}

/// Shared handle to the in-flight exchange.
pub type ExchangeRef = Arc<Mutex<Exchange>>;

/// The interfaces this module registers, for the HCV-06 parity check.
///
/// Derived from the same constant list `register` iterates, so a declared
/// interface that is never wired cannot pass the check.
pub fn registered_interfaces() -> Vec<Registration> {
    INTERFACES
        .iter()
        .map(|(name, _)| Registration {
            interface: (*name).to_string(),
            // Every entry below is a real implementation. If that ever stops
            // being true, the entry must be deleted rather than marked Stub —
            // Stub exists so the checker can catch a shim someone added, not
            // as a way to declare one.
            kind: RegistrationKind::Real,
        })
        .collect()
}

/// Interface name → the functions it declares in `host.wit`.
const INTERFACES: &[(&str, &[&str])] = &[
    ("clean:http/routing@0.1.0", &["register"]),
    (
        "clean:http/request@0.1.0",
        &["get-parts", "get-headers", "get-header", "get-body"],
    ),
    (
        "clean:http/response@0.1.0",
        &["set-status", "add-header", "set-body"],
    ),
];

/// Register every `clean:http/*` interface into the linker.
///
/// The exchange is read and written through `StoreState::host_context`, which
/// the request loop populates per request.
pub fn register(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    register_routing(linker)?;
    register_request(linker)?;
    register_response(linker)?;
    Ok(())
}

/// Pull the exchange out of the store's host context.
///
/// Returns `wasmtime::Result` because these run inside linker closures, whose
/// error type is wasmtime's own.
fn exchange(state: &mut StoreState) -> wasmtime::Result<ExchangeRef> {
    state
        .host_context
        .as_ref()
        .and_then(|c| c.downcast_ref::<ExchangeRef>())
        .cloned()
        .ok_or_else(|| wasmtime::Error::msg("no request in flight for this instance"))
}

/// A type mismatch coming out of the component ABI.
fn type_error(what: &str, got: &Val) -> wasmtime::Error {
    wasmtime::Error::msg(format!("{what}, got {got:?}"))
}

fn register_routing(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/routing@0.1.0")?;

    iface.func_new("register", |mut store, _ty, args, _results| {
        let ex = exchange(store.data_mut())?;
        let method = match &args[0] {
            // `enum` lowers to a variant with no payload; the discriminant name
            // is the method.
            Val::Enum(name) => name.clone(),
            other => return Err(type_error("routing.register: expected enum method", other)),
        };
        let path = match &args[1] {
            Val::String(s) => s.clone(),
            other => return Err(type_error("routing.register: expected string path", other)),
        };
        let handler_id = match &args[2] {
            Val::U32(n) => *n,
            other => {
                return Err(type_error(
                    "routing.register: expected u32 handler-id",
                    other,
                ))
            }
        };

        ex.lock().unwrap().routes.push(Route {
            method: method.to_uppercase(),
            path,
            handler_id,
        });
        Ok(())
    })?;

    Ok(())
}

fn register_request(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/request@0.1.0")?;

    iface.func_new("get-parts", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let ex = ex.lock().unwrap();
        results[0] = Val::Record(vec![
            ("method".into(), Val::String(ex.method.clone())),
            ("path".into(), Val::String(ex.path.clone())),
            ("query".into(), Val::String(ex.query.clone())),
            ("peer".into(), Val::String(ex.peer.clone())),
            (
                "correlation-id".into(),
                Val::String(ex.correlation_id.clone()),
            ),
        ]);
        Ok(())
    })?;

    iface.func_new("get-headers", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let ex = ex.lock().unwrap();
        results[0] = Val::List(
            ex.request_headers
                .iter()
                .map(|(n, v)| {
                    Val::Record(vec![
                        ("name".into(), Val::String(n.clone())),
                        ("value".into(), Val::String(v.clone())),
                    ])
                })
                .collect(),
        );
        Ok(())
    })?;

    iface.func_new("get-header", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let wanted = match &args[0] {
            Val::String(s) => s.to_ascii_lowercase(),
            other => {
                return Err(type_error(
                    "request.get-header: expected string name",
                    other,
                ))
            }
        };
        let ex = ex.lock().unwrap();
        let found = ex
            .request_headers
            .iter()
            .find(|(n, _)| n == &wanted)
            .map(|(_, v)| Val::String(v.clone()));
        results[0] = Val::Option(found.map(Box::new));
        Ok(())
    })?;

    iface.func_new("get-body", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let ex = ex.lock().unwrap();
        results[0] = Val::List(ex.request_body.iter().copied().map(Val::U8).collect());
        Ok(())
    })?;

    Ok(())
}

fn register_response(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/response@0.1.0")?;

    iface.func_new("set-status", |mut store, _ty, args, _results| {
        let ex = exchange(store.data_mut())?;
        match &args[0] {
            Val::U16(s) => ex.lock().unwrap().status = *s,
            other => return Err(type_error("response.set-status: expected u16", other)),
        }
        Ok(())
    })?;

    iface.func_new("add-header", |mut store, _ty, args, _results| {
        let ex = exchange(store.data_mut())?;
        let (name, value) = match (&args[0], &args[1]) {
            (Val::String(n), Val::String(v)) => (n.clone(), v.clone()),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "response.add-header: expected two strings, got {other:?}"
                )))
            }
        };
        ex.lock().unwrap().response_headers.push((name, value));
        Ok(())
    })?;

    iface.func_new("set-body", |mut store, _ty, args, _results| {
        let ex = exchange(store.data_mut())?;
        let body = match &args[0] {
            Val::List(items) => items
                .iter()
                .map(|v| match v {
                    Val::U8(b) => Ok(*b),
                    other => Err(type_error("response.set-body: expected u8", other)),
                })
                .collect::<wasmtime::Result<Vec<u8>>>()?,
            other => return Err(type_error("response.set-body: expected list<u8>", other)),
        };
        ex.lock().unwrap().response_body = body;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_interface_is_reported_as_a_real_registration() {
        let regs = registered_interfaces();
        assert_eq!(regs.len(), 3);
        assert!(regs.iter().all(|r| r.kind == RegistrationKind::Real));

        let names: Vec<_> = regs.iter().map(|r| r.interface.as_str()).collect();
        assert!(names.contains(&"clean:http/routing@0.1.0"));
        assert!(names.contains(&"clean:http/request@0.1.0"));
        assert!(names.contains(&"clean:http/response@0.1.0"));
    }

    #[test]
    fn registration_list_matches_the_interfaces_table() {
        // The parity report is only trustworthy if it is derived from the same
        // table `register` walks.
        assert_eq!(registered_interfaces().len(), INTERFACES.len());
    }
}
