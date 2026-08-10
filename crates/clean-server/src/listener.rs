//! The HTTP listener and per-request flow (§1.4).
//!
//! This is the part `clean-host-core` explicitly leaves to concrete hosts
//! (CH-01). M0 serves HTTP/1.1 in the clear; TLS and HTTP/2 land in Phase 2.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::guest::Exchange;
use crate::routing::Match;
use crate::startup::Runtime;

/// Header carrying a correlation id supplied by the client or an upstream proxy.
const CORRELATION_HEADER: &str = "x-correlation-id";

/// Serve until `shutdown` resolves.
pub async fn serve(
    runtime: Arc<Runtime>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
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
        routes = runtime.router.routes().len(),
        "listening"
    );

    let counter = Arc::new(AtomicU64::new(0));
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

                let runtime = Arc::clone(&runtime);
                let counter = Arc::clone(&counter);

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let runtime = Arc::clone(&runtime);
                        let counter = Arc::clone(&counter);
                        async move {
                            Ok::<_, Infallible>(
                                handle(runtime, counter, req, peer.to_string()).await,
                            )
                        }
                    });

                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder.keep_alive(true);

                    if let Err(e) = builder.serve_connection(io, service).await {
                        tracing::debug!(
                            target: "clean_server::listener",
                            error = %e,
                            "connection closed"
                        );
                    }
                });
            }
        }
    }
}

/// One request, end to end (§1.4.2).
async fn handle(
    runtime: Arc<Runtime>,
    counter: Arc<AtomicU64>,
    req: Request<Incoming>,
    peer: String,
) -> Response<Full<Bytes>> {
    let started = Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or_default().to_string();

    let correlation_id = req
        .headers()
        .get(CORRELATION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("req-{:016x}", counter.fetch_add(1, Ordering::Relaxed)));

    let response = route_and_dispatch(
        &runtime,
        req,
        &method,
        &path,
        &query,
        &peer,
        &correlation_id,
    )
    .await;

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
    runtime: &Arc<Runtime>,
    req: Request<Incoming>,
    method: &hyper::Method,
    path: &str,
    query: &str,
    peer: &str,
    correlation_id: &str,
) -> Response<Full<Bytes>> {
    let handler_id = match runtime.router.match_route(method.as_str(), path) {
        Match::Found { handler_id } => handler_id,
        Match::MethodNotAllowed { allowed } => {
            let allow = allowed.join(", ");
            return text_response(StatusCode::METHOD_NOT_ALLOWED, "405 method not allowed\n")
                .map_response(|mut r| {
                    if let Ok(v) = allow.parse() {
                        r.headers_mut().insert(ALLOW, v);
                    }
                    r
                });
        }
        Match::NotFound => {
            return text_response(StatusCode::NOT_FOUND, "404 not found\n");
        }
    };

    // Collect the body under the configured cap before touching the guest, so
    // an oversized upload never reaches guest memory (§1.7).
    let (parts, body) = req.into_parts();
    let limit = runtime.server.body_max_bytes;

    let collected = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            tracing::debug!(target: "clean_server::request", error = %e, "body read failed");
            return text_response(StatusCode::BAD_REQUEST, "400 bad request\n");
        }
    };
    if collected.len() as u64 > limit {
        return text_response(StatusCode::PAYLOAD_TOO_LARGE, "413 payload too large\n");
    }

    let mut exchange = Exchange::new();
    exchange.method = method.as_str().to_uppercase();
    exchange.path = path.to_string();
    exchange.query = query.to_string();
    exchange.peer = peer.to_string();
    exchange.correlation_id = correlation_id.to_string();
    exchange.request_body = collected.to_vec();
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

    let exchange = match result {
        Ok(Ok(ex)) => ex,
        Ok(Err(e)) => {
            let message = e.to_string();
            // Pool exhaustion is a load condition, not a bug: 503 + Retry-After
            // so a client can back off (§1.4.2).
            if message.contains("pool exhausted") {
                tracing::warn!(target: "clean_server::request", "pool exhausted");
                return text_response(StatusCode::SERVICE_UNAVAILABLE, "503 service unavailable\n")
                    .map_response(|mut r| {
                        r.headers_mut().insert(RETRY_AFTER, "1".parse().unwrap());
                        r
                    });
            }
            if message.contains("shutting down") {
                return text_response(StatusCode::SERVICE_UNAVAILABLE, "503 shutting down\n");
            }
            tracing::error!(target: "clean_server::request", error = %message, "guest call failed");
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            );
        }
        Err(e) => {
            tracing::error!(target: "clean_server::request", error = %e, "dispatch task panicked");
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 internal server error\n",
            );
        }
    };

    build_response(exchange, method == hyper::Method::HEAD)
}

fn build_response(exchange: Exchange, drop_body: bool) -> Response<Full<Bytes>> {
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
        Full::new(Bytes::new())
    } else {
        Full::new(Bytes::from(exchange.response_body))
    };

    builder.body(body).unwrap_or_else(|e| {
        tracing::error!(target: "clean_server::request", error = %e, "malformed guest response");
        plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "500 internal server error\n",
        )
    })
}

fn text_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    plain(status, body)
}

fn plain(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("static response is well-formed")
}

/// Small helper so header tweaks read inline at the call site.
trait MapResponse {
    fn map_response(self, f: impl FnOnce(Self) -> Self) -> Self
    where
        Self: Sized;
}

impl MapResponse for Response<Full<Bytes>> {
    fn map_response(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }
}
