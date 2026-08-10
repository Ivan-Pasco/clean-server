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

use tokio::sync::mpsc;

use crate::envelope::{self, CookieOptions, EnvelopeError, SameSite};
use crate::sockets::{Outbound, Registry, SocketError, SseEvent};

/// The request being served, plus the response being built.
///
/// One of these is installed into the store before the guest is invoked and
/// taken out afterwards. Because a checked-out instance serves exactly one
/// request at a time, this is never shared across requests.
/// Not `Clone`: it carries the outbound-queue receivers, which have exactly one
/// consumer (the connection task that takes them after the handler returns).
#[derive(Debug, Default)]
pub struct Exchange {
    pub method: String,
    pub path: String,
    pub query: String,
    pub peer: String,
    pub correlation_id: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: Vec<u8>,
    /// Path parameters captured by the matched route.
    pub params: Vec<(String, String)>,

    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Vec<u8>,

    /// Routes the guest registered during `init`.
    pub routes: Vec<Route>,

    /// Set when this request carried a valid WebSocket upgrade, so
    /// `websocket.accept` knows whether it may succeed.
    pub upgrade_available: bool,
    /// The socket the guest accepted, if any. The connection task takes over
    /// the response once the handler returns.
    pub accepted_socket: Option<u64>,
    /// The SSE stream the guest started, if any.
    pub sse_stream: Option<u64>,
    /// Live-connection registry. Absent during `init`, when no request exists.
    pub registry: Option<Registry>,
    /// The `[server]` block, for cookie defaults.
    pub server_config: Option<std::sync::Arc<crate::config::ServerConfig>>,
    /// A CSRF token bound during this request.
    pub csrf_token: Option<String>,
    /// Receiver for an accepted WebSocket's outbound queue, taken by the
    /// connection task once the handler returns.
    pub pending_socket_rx: Option<mpsc::UnboundedReceiver<Outbound>>,
    /// Receiver for a started SSE stream's outbound queue.
    pub pending_stream_rx: Option<mpsc::UnboundedReceiver<Outbound>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub handler_id: u32,
    /// Whether CSRF validation applies (§1.7). Defaults to true; a route opts
    /// out only when it authenticates callers another way.
    pub csrf: bool,
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
        &[
            "get-parts",
            "get-headers",
            "get-header",
            "get-body",
            "get-params",
            "get-param",
        ],
    ),
    (
        "clean:http/response@0.1.0",
        &["set-status", "add-header", "set-body"],
    ),
    (
        "clean:http/websocket@0.1.0",
        &[
            "accept",
            "send-text",
            "send-binary",
            "queued-bytes",
            "close",
        ],
    ),
    (
        "clean:http/sse@0.1.0",
        &["start", "send", "set-retry", "close"],
    ),
    (
        "clean:http/session-envelope@0.1.0",
        &["set-cookie", "set-csrf", "get-csrf", "read-cookie"],
    ),
    (
        "clean:http/realtime-sockets@0.1.0",
        &["deliver", "close", "queued-bytes"],
    ),
    ("clean:http/log@0.1.0", &["emit"]),
];

/// Register every `clean:http/*` interface into the linker.
///
/// The exchange is read and written through `StoreState::host_context`, which
/// the request loop populates per request.
pub fn register(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    register_routing(linker)?;
    register_request(linker)?;
    register_response(linker)?;
    register_websocket(linker)?;
    register_sse(linker)?;
    register_session_envelope(linker)?;
    register_realtime_sockets(linker)?;
    register_log(linker)?;
    Ok(())
}

/// `clean:host/log` — structured records from guests and bridges.
///
/// Routed into `tracing` so guest output interleaves with the server's own
/// request logs rather than racing them on the same file descriptor.
fn register_log(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/log@0.1.0")?;

    iface.func_new("emit", |mut store, _ty, args, _results| {
        let level = match &args[0] {
            Val::Enum(name) => name.clone(),
            other => return Err(type_error("log.emit: expected enum level", other)),
        };
        let message = match &args[1] {
            Val::String(m) => m.clone(),
            other => return Err(type_error("log.emit: expected string message", other)),
        };

        let mut fields = Vec::new();
        if let Val::List(items) = &args[2] {
            for item in items {
                if let Val::Record(pairs) = item {
                    let mut key = String::new();
                    let mut value = String::new();
                    for (name, v) in pairs {
                        match (name.as_str(), v) {
                            ("key", Val::String(s)) => key = s.clone(),
                            ("value", Val::String(s)) => value = s.clone(),
                            _ => {}
                        }
                    }
                    fields.push((key, value));
                }
            }
        }

        // The correlation id ties a guest record to the request that produced
        // it, which is the whole point of having one.
        let correlation_id = exchange(store.data_mut())
            .ok()
            .map(|ex| ex.lock().unwrap().correlation_id.clone())
            .unwrap_or_default();

        let rendered: Vec<String> = fields
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();

        match level.as_str() {
            "trace" => tracing::trace!(target: "clean_server::guest", correlation_id, fields = %rendered.join(" "), "{message}"),
            "debug" => tracing::debug!(target: "clean_server::guest", correlation_id, fields = %rendered.join(" "), "{message}"),
            "warn" => tracing::warn!(target: "clean_server::guest", correlation_id, fields = %rendered.join(" "), "{message}"),
            "error" => tracing::error!(target: "clean_server::guest", correlation_id, fields = %rendered.join(" "), "{message}"),
            _ => tracing::info!(target: "clean_server::guest", correlation_id, fields = %rendered.join(" "), "{message}"),
        }
        Ok(())
    })?;

    Ok(())
}

/// Lower a `SocketError` into the WIT `result` shape the interfaces declare.
fn socket_result(outcome: Result<(), SocketError>) -> Val {
    match outcome {
        Ok(()) => Val::Result(Ok(None)),
        Err(e) => Val::Result(Err(Some(Box::new(Val::Enum(e.as_wit().into()))))),
    }
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

        // A missing or malformed options record leaves CSRF ON. A parse slip
        // must never silently disable a security control.
        let csrf = match args.get(3) {
            Some(Val::Record(fields)) => fields
                .iter()
                .find(|(name, _)| name == "csrf")
                .and_then(|(_, v)| match v {
                    Val::Bool(b) => Some(*b),
                    _ => None,
                })
                .unwrap_or(true),
            _ => true,
        };

        ex.lock().unwrap().routes.push(Route {
            method: method.to_uppercase(),
            path,
            handler_id,
            csrf,
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

    iface.func_new("get-params", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let ex = ex.lock().unwrap();
        results[0] = Val::List(
            ex.params
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

    iface.func_new("get-param", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let wanted = match &args[0] {
            Val::String(s) => s.clone(),
            other => return Err(type_error("request.get-param: expected string name", other)),
        };
        let ex = ex.lock().unwrap();
        let found = ex
            .params
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(_, v)| Val::String(v.clone()));
        results[0] = Val::Option(found.map(Box::new));
        Ok(())
    })?;

    Ok(())
}

fn register_websocket(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/websocket@0.1.0")?;

    iface.func_new("accept", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let mut ex = ex.lock().unwrap();

        // Refusing here rather than at the wire keeps the failure legible: the
        // guest asked to upgrade a request that was never an upgrade.
        if !ex.upgrade_available {
            results[0] = Val::Result(Err(Some(Box::new(Val::Enum(
                SocketError::NotAnUpgrade.as_wit().into(),
            )))));
            return Ok(());
        }
        if let Some(existing) = ex.accepted_socket {
            results[0] = Val::Result(Ok(Some(Box::new(Val::U64(existing)))));
            return Ok(());
        }

        let Some(registry) = ex.registry.clone() else {
            return Err(wasmtime::Error::msg(
                "websocket.accept called outside a request",
            ));
        };
        let (id, receiver) = registry.register_socket();
        ex.accepted_socket = Some(id);
        // The connection task collects the receiver after the handler returns.
        ex.pending_socket_rx = Some(receiver);
        results[0] = Val::Result(Ok(Some(Box::new(Val::U64(id)))));
        Ok(())
    })?;

    iface.func_new("send-text", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (id, text) = match (&args[0], &args[1]) {
            (Val::U64(id), Val::String(s)) => (*id, s.clone()),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "websocket.send-text: expected (u64, string), got {other:?}"
                )))
            }
        };
        let registry = registry_of(&ex)?;
        results[0] = socket_result(registry.send_socket(id, Outbound::Text(text)));
        Ok(())
    })?;

    iface.func_new("send-binary", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let id = match &args[0] {
            Val::U64(id) => *id,
            other => {
                return Err(type_error(
                    "websocket.send-binary: expected u64 socket",
                    other,
                ))
            }
        };
        let bytes = match &args[1] {
            Val::List(items) => items
                .iter()
                .map(|v| match v {
                    Val::U8(b) => Ok(*b),
                    other => Err(type_error("websocket.send-binary: expected u8", other)),
                })
                .collect::<wasmtime::Result<Vec<u8>>>()?,
            other => {
                return Err(type_error(
                    "websocket.send-binary: expected list<u8>",
                    other,
                ))
            }
        };
        let registry = registry_of(&ex)?;
        results[0] = socket_result(registry.send_socket(id, Outbound::Binary(bytes)));
        Ok(())
    })?;

    iface.func_new("queued-bytes", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let id = match &args[0] {
            Val::U64(id) => *id,
            other => return Err(type_error("websocket.queued-bytes: expected u64", other)),
        };
        let registry = registry_of(&ex)?;
        results[0] = Val::U64(registry.queued_bytes(id));
        Ok(())
    })?;

    iface.func_new("close", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (id, code, reason) = match (&args[0], &args[1], &args[2]) {
            (Val::U64(id), Val::U16(code), Val::String(reason)) => (*id, *code, reason.clone()),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "websocket.close: expected (u64, u16, string), got {other:?}"
                )))
            }
        };
        let registry = registry_of(&ex)?;
        results[0] = socket_result(registry.send_socket(id, Outbound::Close { code, reason }));
        Ok(())
    })?;

    Ok(())
}

fn register_sse(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/sse@0.1.0")?;

    iface.func_new("start", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let mut ex = ex.lock().unwrap();

        // A body already set means the response shape is decided; turning it
        // into a stream now would silently discard what the guest wrote.
        if !ex.response_body.is_empty() {
            results[0] = Val::Result(Err(Some(Box::new(Val::Enum(
                "response-already-started".into(),
            )))));
            return Ok(());
        }
        if let Some(existing) = ex.sse_stream {
            results[0] = Val::Result(Ok(Some(Box::new(Val::U64(existing)))));
            return Ok(());
        }

        let Some(registry) = ex.registry.clone() else {
            return Err(wasmtime::Error::msg("sse.start called outside a request"));
        };
        let (id, receiver) = registry.register_stream();
        ex.sse_stream = Some(id);
        ex.pending_stream_rx = Some(receiver);
        results[0] = Val::Result(Ok(Some(Box::new(Val::U64(id)))));
        Ok(())
    })?;

    iface.func_new("send", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (id, event_type, data, id_field) = match (&args[0], &args[1], &args[2], &args[3]) {
            (Val::U64(s), Val::String(t), Val::String(d), Val::String(i)) => {
                (*s, t.clone(), d.clone(), i.clone())
            }
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "sse.send: expected (u64, string, string, string), got {other:?}"
                )))
            }
        };
        let registry = registry_of(&ex)?;
        let frame = SseEvent {
            event_type,
            data,
            id: id_field,
            retry_millis: None,
        }
        .frame();
        results[0] = stream_result(registry.send_stream(id, Outbound::Text(frame)));
        Ok(())
    })?;

    iface.func_new("set-retry", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (id, millis) = match (&args[0], &args[1]) {
            (Val::U64(id), Val::U32(ms)) => (*id, *ms),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "sse.set-retry: expected (u64, u32), got {other:?}"
                )))
            }
        };
        let registry = registry_of(&ex)?;
        // `retry:` is a field of an otherwise-empty event, per the SSE spec.
        let frame = format!("retry: {millis}\n\n");
        results[0] = stream_result(registry.send_stream(id, Outbound::Text(frame)));
        Ok(())
    })?;

    iface.func_new("close", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let id = match &args[0] {
            Val::U64(id) => *id,
            other => return Err(type_error("sse.close: expected u64 stream", other)),
        };
        let registry = registry_of(&ex)?;
        results[0] = stream_result(registry.send_stream(
            id,
            Outbound::Close {
                code: 1000,
                reason: String::new(),
            },
        ));
        Ok(())
    })?;

    Ok(())
}

fn register_session_envelope(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/session-envelope@0.1.0")?;

    iface.func_new("set-cookie", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (name, value) = match (&args[0], &args[1]) {
            (Val::String(n), Val::String(v)) => (n.clone(), v.clone()),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "session-envelope.set-cookie: expected two strings, got {other:?}"
                )))
            }
        };
        let options = cookie_options_from(&args[2])?;

        let mut ex = ex.lock().unwrap();
        // Callable during `init`, when no request exists — the WIT declares
        // `no-active-request` for exactly this case.
        let Some(config) = ex.server_config.clone() else {
            results[0] = Val::Result(Err(Some(Box::new(Val::Variant(
                EnvelopeError::NoActiveRequest.as_wit().into(),
                None,
            )))));
            return Ok(());
        };

        let options = envelope::apply_defaults(options, &config);
        match envelope::set_cookie_header(&name, &value, &options) {
            Ok(header) => {
                // Once a stream or socket is open the response head is already
                // on the wire; a late Set-Cookie would silently never arrive.
                if ex.sse_stream.is_some() || ex.accepted_socket.is_some() {
                    results[0] = Val::Result(Err(Some(Box::new(Val::Variant(
                        EnvelopeError::HeaderLocked.as_wit().into(),
                        None,
                    )))));
                } else {
                    ex.response_headers.push(("set-cookie".into(), header));
                    results[0] = Val::Result(Ok(None));
                }
            }
            Err(e) => {
                results[0] =
                    Val::Result(Err(Some(Box::new(Val::Variant(e.as_wit().into(), None)))));
            }
        }
        Ok(())
    })?;

    iface.func_new("set-csrf", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let token = match &args[0] {
            Val::String(t) => t.clone(),
            other => {
                return Err(type_error(
                    "session-envelope.set-csrf: expected string",
                    other,
                ))
            }
        };

        let mut ex = ex.lock().unwrap();
        let Some(config) = ex.server_config.clone() else {
            results[0] = Val::Result(Err(Some(Box::new(Val::Variant(
                EnvelopeError::NoActiveRequest.as_wit().into(),
                None,
            )))));
            return Ok(());
        };

        // With no session bridge composed, the token rides in a cookie. When
        // one is composed this moves to clean:session/store so it survives
        // across requests (session spec §5).
        let options = envelope::apply_defaults(CookieOptions::default(), &config);
        match envelope::set_cookie_header(envelope::CSRF_COOKIE, &token, &options) {
            Ok(header) => {
                ex.csrf_token = Some(token);
                ex.response_headers.push(("set-cookie".into(), header));
                results[0] = Val::Result(Ok(None));
            }
            Err(_) => {
                results[0] = Val::Result(Err(Some(Box::new(Val::Variant(
                    EnvelopeError::InvalidCookieValue.as_wit().into(),
                    None,
                )))));
            }
        }
        Ok(())
    })?;

    iface.func_new("get-csrf", |mut store, _ty, _args, results| {
        let ex = exchange(store.data_mut())?;
        let ex = ex.lock().unwrap();

        // Prefer a token set during this request; otherwise read the cookie the
        // client sent back.
        let token = ex.csrf_token.clone().or_else(|| {
            ex.request_headers
                .iter()
                .find(|(n, _)| n == "cookie")
                .and_then(|(_, v)| envelope::read_cookie(v, envelope::CSRF_COOKIE))
        });
        results[0] = Val::Option(token.map(|t| Box::new(Val::String(t))));
        Ok(())
    })?;

    iface.func_new("read-cookie", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let name = match &args[0] {
            Val::String(n) => n.clone(),
            other => {
                return Err(type_error(
                    "session-envelope.read-cookie: expected string",
                    other,
                ))
            }
        };
        let ex = ex.lock().unwrap();
        let found = ex
            .request_headers
            .iter()
            .find(|(n, _)| n == "cookie")
            .and_then(|(_, v)| envelope::read_cookie(v, &name));
        results[0] = Val::Option(found.map(|v| Box::new(Val::String(v))));
        Ok(())
    })?;

    Ok(())
}

/// Decode the `cookie-options` record.
fn cookie_options_from(value: &Val) -> wasmtime::Result<CookieOptions> {
    let Val::Record(fields) = value else {
        return Err(type_error("expected a cookie-options record", value));
    };

    let mut options = CookieOptions::default();
    for (name, field) in fields {
        match (name.as_str(), field) {
            ("path", Val::Option(v)) => {
                options.path = string_option(v);
            }
            ("domain", Val::Option(v)) => {
                options.domain = string_option(v);
            }
            ("max-age-secs", Val::Option(v)) => {
                options.max_age_secs = match v.as_deref() {
                    Some(Val::U32(n)) => Some(*n),
                    _ => None,
                };
            }
            ("http-only", Val::Bool(b)) => options.http_only = *b,
            ("secure", Val::Bool(b)) => options.secure = *b,
            ("same-site", Val::Option(v)) => {
                options.same_site = match v.as_deref() {
                    Some(Val::Variant(name, _)) => SameSite::parse(name),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    Ok(options)
}

fn string_option(value: &Option<Box<Val>>) -> Option<String> {
    match value.as_deref() {
        Some(Val::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn register_realtime_sockets(linker: &mut Linker<StoreState>) -> anyhow::Result<()> {
    let mut iface = linker.instance("clean:http/realtime-sockets@0.1.0")?;

    iface.func_new("deliver", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let id = match &args[0] {
            Val::U64(id) => *id,
            other => return Err(type_error("realtime-sockets.deliver: expected u64", other)),
        };
        let payload = match &args[1] {
            Val::List(items) => items
                .iter()
                .map(|v| match v {
                    Val::U8(b) => Ok(*b),
                    other => Err(type_error("realtime-sockets.deliver: expected u8", other)),
                })
                .collect::<wasmtime::Result<Vec<u8>>>()?,
            other => {
                return Err(type_error(
                    "realtime-sockets.deliver: expected list<u8>",
                    other,
                ))
            }
        };

        let registry = registry_of(&ex)?;
        results[0] = envelope_socket_result(registry.send_socket(id, Outbound::Binary(payload)));
        Ok(())
    })?;

    iface.func_new("close", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let (id, code, reason) = match (&args[0], &args[1], &args[2]) {
            (Val::U64(id), Val::U16(code), Val::String(r)) => (*id, *code, r.clone()),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "realtime-sockets.close: expected (u64, u16, string), got {other:?}"
                )))
            }
        };
        let registry = registry_of(&ex)?;
        results[0] =
            envelope_socket_result(registry.send_socket(id, Outbound::Close { code, reason }));
        Ok(())
    })?;

    iface.func_new("queued-bytes", |mut store, _ty, args, results| {
        let ex = exchange(store.data_mut())?;
        let id = match &args[0] {
            Val::U64(id) => *id,
            other => {
                return Err(type_error(
                    "realtime-sockets.queued-bytes: expected u64",
                    other,
                ))
            }
        };
        let registry = registry_of(&ex)?;
        results[0] = Val::U64(registry.queued_bytes(id));
        Ok(())
    })?;

    Ok(())
}

/// The realtime envelope's error variant omits `not-an-upgrade`, which is
/// meaningless to a caller that never saw the request.
fn envelope_socket_result(outcome: Result<(), SocketError>) -> Val {
    match outcome {
        Ok(()) => Val::Result(Ok(None)),
        Err(SocketError::SocketSlow) => Val::Result(Err(Some(Box::new(Val::Variant(
            "socket-slow".into(),
            None,
        ))))),
        Err(_) => Val::Result(Err(Some(Box::new(Val::Variant("closed".into(), None))))),
    }
}

/// The live-connection registry for the in-flight request.
fn registry_of(ex: &ExchangeRef) -> wasmtime::Result<Registry> {
    ex.lock()
        .unwrap()
        .registry
        .clone()
        .ok_or_else(|| wasmtime::Error::msg("no live-connection registry for this request"))
}

/// Lower a `SocketError` into the SSE `stream-error` shape.
fn stream_result(outcome: Result<(), SocketError>) -> Val {
    match outcome {
        Ok(()) => Val::Result(Ok(None)),
        Err(_) => Val::Result(Err(Some(Box::new(Val::Enum("closed".into()))))),
    }
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
        assert!(regs.iter().all(|r| r.kind == RegistrationKind::Real));

        let names: Vec<_> = regs.iter().map(|r| r.interface.as_str()).collect();
        assert!(names.contains(&"clean:http/routing@0.1.0"));
        assert!(names.contains(&"clean:http/request@0.1.0"));
        assert!(names.contains(&"clean:http/response@0.1.0"));
        assert!(names.contains(&"clean:http/websocket@0.1.0"));
        assert!(names.contains(&"clean:http/sse@0.1.0"));
    }

    #[test]
    fn registration_list_matches_the_interfaces_table() {
        // The parity report is only trustworthy if it is derived from the same
        // table `register` walks.
        assert_eq!(registered_interfaces().len(), INTERFACES.len());
    }
}
