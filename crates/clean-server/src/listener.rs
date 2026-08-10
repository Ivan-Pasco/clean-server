//! The HTTP listener and per-request flow (§1.4).
//!
//! This is the part `clean-host-core` explicitly leaves to concrete hosts
//! (CH-01). It owns TCP accept, TLS termination, protocol selection, request
//! marshaling, WebSocket upgrade, and SSE streaming.
//!
//! ## Protocol selection
//!
//! Under TLS, ALPN decides: the handshake reports `h2` or `http/1.1` and the
//! connection is served accordingly. Without TLS, HTTP/2 requires prior
//! knowledge, so h2c is detected from the connection preface rather than
//! assumed — a plaintext client that is not speaking h2 must not be handed an
//! h2 parser.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{ALLOW, CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use crate::guest::Exchange;
use crate::routing::Match;
use crate::sockets::Outbound;
use crate::startup::Runtime;
use crate::websocket;

/// Header carrying a correlation id supplied by the client or an upstream proxy.
const CORRELATION_HEADER: &str = "x-correlation-id";

/// The HTTP/2 connection preface, for detecting h2c on a plaintext socket.
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n";

/// The response body type. Boxed because a response is either a complete
/// buffer or an open SSE stream, and the connection cannot know which until
/// the guest has run.
pub type ResponseBody = BoxBody<Bytes, Infallible>;

/// Live per-connection counters shared across the whole listener.
struct Shared {
    runtime: Arc<Runtime>,
    correlation: AtomicU64,
    /// Requests currently waiting for or holding a guest instance. Bounds the
    /// queue at `[server] queue-depth` (§1.4.2).
    in_flight: AtomicUsize,
}

/// Serve until `shutdown` resolves.
pub async fn serve(
    runtime: Arc<Runtime>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let acceptor = crate::tls::acceptor(&runtime.server)?;

    let listener = TcpListener::bind(runtime.server.listen)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "cannot bind `[server] listen = \"{}\"`: {e}",
                runtime.server.listen
            )
        })?;

    tracing::info!(
        target: "clean_server::listener",
        listen = %runtime.server.listen,
        mount = %runtime.server.mount,
        routes = runtime.router.len(),
        tls = acceptor.is_some(),
        h2 = runtime.server.h2,
        "listening"
    );

    let shared = Arc::new(Shared {
        runtime,
        correlation: AtomicU64::new(0),
        in_flight: AtomicUsize::new(0),
    });

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!(target: "clean_server::listener", "no longer accepting connections");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        // A failed accept concerns one connection, not the
                        // process; log and keep serving.
                        tracing::warn!(target: "clean_server::listener", error = %e, "accept failed");
                        continue;
                    }
                };

                let shared = Arc::clone(&shared);
                let acceptor = acceptor.clone();

                tokio::spawn(async move {
                    match acceptor {
                        Some(acceptor) => match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                // ALPN already told us what the client speaks.
                                let alpn_h2 = tls_stream
                                    .get_ref()
                                    .1
                                    .alpn_protocol()
                                    .map(|p| p == b"h2")
                                    .unwrap_or(false);
                                serve_connection(shared, tls_stream, peer.to_string(), alpn_h2)
                                    .await;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    target: "clean_server::listener",
                                    error = %e,
                                    "TLS handshake failed"
                                );
                            }
                        },
                        None => {
                            serve_plaintext(shared, stream, peer.to_string()).await;
                        }
                    }
                });
            }
        }
    }
}

/// Serve a plaintext connection, detecting h2c from the preface.
async fn serve_plaintext(shared: Arc<Shared>, stream: tokio::net::TcpStream, peer: String) {
    let h2c = shared.runtime.server.h2 && peek_is_h2(&stream).await;
    serve_connection(shared, stream, peer, h2c).await;
}

/// Look at the buffered bytes without consuming them.
///
/// Without this, enabling `h2` would break every plaintext HTTP/1.1 client:
/// h2c has no negotiation, so the preface is the only honest signal.
async fn peek_is_h2(stream: &tokio::net::TcpStream) -> bool {
    let mut buf = [0u8; 16];
    match stream.peek(&mut buf).await {
        Ok(n) if n >= H2_PREFACE.len() => buf.starts_with(H2_PREFACE),
        _ => false,
    }
}

async fn serve_connection<S>(shared: Arc<Shared>, stream: S, peer: String, use_h2: bool)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let service = service_fn(move |req| {
        let shared = Arc::clone(&shared);
        let peer = peer.clone();
        async move { Ok::<_, Infallible>(handle(shared, req, peer).await) }
    });

    let result = if use_h2 {
        hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
    } else {
        // `with_upgrades` is what lets a WebSocket handshake take over the
        // connection after the 101 response.
        hyper::server::conn::http1::Builder::new()
            .keep_alive(true)
            .serve_connection(io, service)
            .with_upgrades()
            .await
    };

    if let Err(e) = result {
        tracing::debug!(target: "clean_server::listener", error = %e, "connection closed");
    }
}

/// One request, end to end (§1.4.2).
async fn handle(
    shared: Arc<Shared>,
    req: Request<Incoming>,
    peer: String,
) -> Response<ResponseBody> {
    let started = Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    let correlation_id = req
        .headers()
        .get(CORRELATION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "req-{:016x}",
                shared.correlation.fetch_add(1, Ordering::Relaxed)
            )
        });

    let response = route_and_dispatch(&shared, req, &peer, &correlation_id).await;

    // §1.9: one structured log line per request.
    tracing::info!(
        target: "clean_server::request",
        method = %method,
        path = %path,
        status = response.status().as_u16(),
        latency_ms = started.elapsed().as_millis() as u64,
        correlation_id = %correlation_id,
        "request"
    );

    response
}

async fn route_and_dispatch(
    shared: &Arc<Shared>,
    req: Request<Incoming>,
    peer: &str,
    correlation_id: &str,
) -> Response<ResponseBody> {
    let runtime = &shared.runtime;
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or_default().to_string();

    let (handler_id, params) = match runtime.router.match_route(method.as_str(), &path) {
        Match::Found { handler_id, params } => (handler_id, params),
        Match::MethodNotAllowed { allowed } => {
            let mut response = text(StatusCode::METHOD_NOT_ALLOWED, "405 method not allowed\n");
            if let Ok(v) = allowed.join(", ").parse() {
                response.headers_mut().insert(ALLOW, v);
            }
            return response;
        }
        Match::NotFound => return text(StatusCode::NOT_FOUND, "404 not found\n"),
    };

    // §1.4.2: beyond `instances-max`, requests queue up to `queue-depth`, then
    // 503. Checked before reading the body so an overloaded server sheds load
    // cheaply rather than buffering megabytes it will not use.
    let ceiling = runtime.server.queue_depth as usize + runtime.instances_max as usize;
    let in_flight = shared.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
    let _guard = InFlightGuard {
        counter: &shared.in_flight,
    };

    if in_flight > ceiling {
        tracing::warn!(
            target: "clean_server::request",
            in_flight,
            ceiling,
            "queue depth exceeded"
        );
        return retry_after(text(
            StatusCode::SERVICE_UNAVAILABLE,
            "503 service unavailable\n",
        ));
    }

    let is_upgrade = websocket::is_upgrade_request(&req);

    // §1.7: unsafe methods without a valid CSRF token are rejected before
    // reaching the guest, so a handler cannot forget to check.
    let cookie_header = req
        .headers()
        .get(hyper::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let submitted_token = req
        .headers()
        .get(crate::envelope::CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if let Some(reason) = crate::envelope::csrf_rejection(
        method.as_str(),
        cookie_header.as_deref(),
        submitted_token.as_deref(),
    ) {
        tracing::warn!(
            target: "clean_server::request",
            method = %method,
            path = %path,
            reason,
            "CSRF check failed"
        );
        return text(StatusCode::FORBIDDEN, "403 forbidden\n");
    }

    // Collect the body under the configured cap before touching the guest, so
    // an oversized upload never reaches guest memory (§1.7).
    let limit = runtime.server.body_max_bytes;
    if let Some(declared) = req
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        // Refuse on the declared length rather than reading it first.
        if declared > limit {
            return text(StatusCode::PAYLOAD_TOO_LARGE, "413 payload too large\n");
        }
    }

    let (parts, body) = req.into_parts();
    let collected = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            tracing::debug!(target: "clean_server::request", error = %e, "body read failed");
            return text(StatusCode::BAD_REQUEST, "400 bad request\n");
        }
    };
    if collected.len() as u64 > limit {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "413 payload too large\n");
    }

    let mut exchange = Exchange::new();
    exchange.method = method.as_str().to_uppercase();
    exchange.path = path.clone();
    exchange.query = query;
    exchange.peer = peer.to_string();
    exchange.correlation_id = correlation_id.to_string();
    exchange.request_body = collected.to_vec();
    exchange.params = params;
    exchange.upgrade_available = is_upgrade;
    exchange.registry = Some(runtime.sockets.clone());
    exchange.server_config = Some(Arc::clone(&runtime.server));
    exchange.request_headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();

    // The guest call is synchronous and CPU-bound from tokio's perspective;
    // running it on a blocking thread keeps it off the async worker pool.
    let runtime_for_task = Arc::clone(runtime);
    let result = tokio::task::spawn_blocking(move || {
        crate::startup::dispatch(&runtime_for_task.host, handler_id, exchange)
    })
    .await;

    let mut exchange = match result {
        Ok(Ok(ex)) => ex,
        Ok(Err(e)) => return dispatch_error(&e.to_string()),
        Err(e) => {
            tracing::error!(target: "clean_server::request", error = %e, "dispatch task panicked");
            return text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            );
        }
    };

    // The guest accepted a WebSocket upgrade: hand the connection to the
    // socket task and reply 101.
    if let Some(socket_id) = exchange.accepted_socket {
        let Some(receiver) = exchange.pending_socket_rx.take() else {
            return text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            );
        };
        return websocket::upgrade_response(
            Request::from_parts(parts, ()),
            socket_id,
            receiver,
            runtime.sockets.clone(),
        );
    }

    // The guest started an SSE stream: keep the response open and drain the
    // queue into it.
    if let Some(stream_id) = exchange.sse_stream {
        let Some(receiver) = exchange.pending_stream_rx.take() else {
            return text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            );
        };
        return sse_response(&exchange, stream_id, receiver, runtime.sockets.clone());
    }

    build_response(exchange, method == hyper::Method::HEAD)
}

/// Decrements the in-flight counter however the request ends.
struct InFlightGuard<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

fn dispatch_error(message: &str) -> Response<ResponseBody> {
    // Pool exhaustion is a load condition, not a bug: 503 + Retry-After so a
    // client can back off (§1.4.2).
    if message.contains("pool exhausted") {
        tracing::warn!(target: "clean_server::request", "pool exhausted");
        return retry_after(text(
            StatusCode::SERVICE_UNAVAILABLE,
            "503 service unavailable\n",
        ));
    }
    if message.contains("shutting down") {
        return text(StatusCode::SERVICE_UNAVAILABLE, "503 shutting down\n");
    }
    // An epoch trap means the handler outran `[server] request-timeout`.
    if message.contains("interrupt") || message.contains("epoch") {
        tracing::warn!(target: "clean_server::request", "handler exceeded request-timeout");
        return text(StatusCode::GATEWAY_TIMEOUT, "504 gateway timeout\n");
    }
    tracing::error!(target: "clean_server::request", error = %message, "guest call failed");
    text(
        StatusCode::INTERNAL_SERVER_ERROR,
        "500 internal server error\n",
    )
}

/// Turn the guest's SSE stream into an open response body.
fn sse_response(
    exchange: &Exchange,
    stream_id: u64,
    receiver: tokio::sync::mpsc::UnboundedReceiver<Outbound>,
    registry: crate::sockets::Registry,
) -> Response<ResponseBody> {
    let stream = futures_util::stream::unfold(
        (receiver, registry, stream_id),
        |(mut rx, registry, id)| async move {
            match rx.recv().await {
                Some(Outbound::Text(frame)) => {
                    let len = frame.len();
                    registry.on_written(id, len);
                    Some((
                        Ok::<_, Infallible>(Frame::data(Bytes::from(frame))),
                        (rx, registry, id),
                    ))
                }
                Some(Outbound::Binary(bytes)) => {
                    registry.on_written(id, bytes.len());
                    Some((Ok(Frame::data(Bytes::from(bytes))), (rx, registry, id)))
                }
                // Close ends the body, which ends the response.
                Some(Outbound::Close { .. }) | None => {
                    registry.remove(id);
                    None
                }
            }
        },
    );

    let mut builder = Response::builder()
        .status(StatusCode::from_u16(exchange.status).unwrap_or(StatusCode::OK))
        .header(CONTENT_TYPE, "text/event-stream")
        // Proxies that buffer would defeat the point of a live stream.
        .header(CACHE_CONTROL, "no-cache")
        .header(CONNECTION, "keep-alive");

    for (name, value) in &exchange.response_headers {
        if !name.eq_ignore_ascii_case("content-type") {
            builder = builder.header(name, value);
        }
    }

    builder
        .body(BoxBody::new(StreamBody::new(stream)))
        .unwrap_or_else(|_| {
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            )
        })
}

fn build_response(exchange: Exchange, drop_body: bool) -> Response<ResponseBody> {
    let status = StatusCode::from_u16(exchange.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status);
    let mut saw_content_type = false;

    for (name, value) in &exchange.response_headers {
        if name.eq_ignore_ascii_case("content-type") {
            saw_content_type = true;
        }
        builder = builder.header(name, value);
    }
    if !saw_content_type {
        builder = builder.header(CONTENT_TYPE, "text/plain; charset=utf-8");
    }

    let len = exchange.response_body.len();
    let body = if drop_body {
        // HEAD keeps the headers a GET would produce, including the length,
        // but sends no body.
        builder = builder.header(CONTENT_LENGTH, len);
        Bytes::new()
    } else {
        Bytes::from(exchange.response_body)
    };

    builder
        .body(BoxBody::new(Full::new(body).map_err(|e| match e {})))
        .unwrap_or_else(|e| {
            tracing::error!(target: "clean_server::request", error = %e, "malformed guest response");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            )
        })
}

fn retry_after(mut response: Response<ResponseBody>) -> Response<ResponseBody> {
    response
        .headers_mut()
        .insert(RETRY_AFTER, "1".parse().expect("static value"));
    response
}

/// A complete plain-text response.
pub fn text(status: StatusCode, body: &'static str) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(BoxBody::new(
            Full::new(Bytes::from_static(body.as_bytes())).map_err(|e| match e {}),
        ))
        .expect("static response is well-formed")
}

/// An empty body, for responses that carry none (the 101 handshake).
pub fn empty_body() -> ResponseBody {
    BoxBody::new(Full::new(Bytes::new()).map_err(|e| match e {}))
}
