//! HTTP Server Implementation
//!
//! Uses Axum to serve HTTP requests and route them to WASM handlers.

use crate::error::{HttpError, RuntimeError, RuntimeResult};
use crate::router::{HttpMethod, SharedRouter};
use crate::wasm::{RequestContext, SharedDbBridge, SharedWasmInstance};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use host_bridge::{DbBridge, DbConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::RwLock as TokioRwLock;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, warn};

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Host address to bind to
    pub host: String,
    /// Port to listen on
    pub port: u16,
    /// Enable CORS
    pub cors_enabled: bool,
    /// CORS allowed origins (if empty, allows any)
    pub cors_origins: Vec<String>,
    /// Request body size limit (bytes)
    pub body_limit: usize,
    /// Database URL (e.g., "sqlite://app.db", "postgres://user:pass@host/db")
    /// If None, database features are disabled
    pub database_url: Option<String>,
    /// Database pool max connections (default: 10)
    pub database_max_connections: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            cors_enabled: true,
            cors_origins: vec![],
            body_limit: 10 * 1024 * 1024, // 10MB
            database_url: std::env::var("DATABASE_URL").ok(),
            database_max_connections: 10,
        }
    }
}

impl ServerConfig {
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn with_database(mut self, url: impl Into<String>) -> Self {
        self.database_url = Some(url.into());
        self
    }

    pub fn with_database_pool_size(mut self, max_connections: u32) -> Self {
        self.database_max_connections = max_connections;
        self
    }

    pub fn socket_addr(&self) -> SocketAddr {
        format!("{}:{}", self.host, self.port)
            .parse()
            .expect("Invalid socket address")
    }
}

/// Application state shared across requests
#[derive(Clone)]
pub struct AppState {
    /// WASM instance for handling requests
    wasm: SharedWasmInstance,
    /// Router with registered routes
    router: SharedRouter,
}

impl AppState {
    pub fn new(wasm: SharedWasmInstance, router: SharedRouter) -> Self {
        Self { wasm, router }
    }
}

/// Start the HTTP server with the given WASM module
pub async fn start_server(wasm_path: PathBuf, config: ServerConfig) -> RuntimeResult<()> {
    info!("Starting Frame Runtime server");
    info!("Loading WASM module from {:?}", wasm_path);

    // Create shared router
    let router = crate::router::create_shared_router();

    // Create database bridge
    let db_bridge: SharedDbBridge = Arc::new(TokioRwLock::new(DbBridge::new()));

    // Configure database if URL is provided
    if let Some(ref db_url) = config.database_url {
        info!("Configuring database connection: {}", mask_db_url(db_url));

        let db_config = DbConfig {
            database_url: db_url.clone(),
            max_connections: config.database_max_connections,
            min_connections: 2,
            connection_timeout: 10000,
            query_timeout: 30000,
        };

        let mut bridge = db_bridge.write().await;
        match bridge.configure(db_config).await {
            Ok(()) => info!("Database connection pool initialized"),
            Err(e) => {
                warn!("Failed to initialize database: {}. Database features will be unavailable.", e);
            }
        }
    } else {
        info!("No DATABASE_URL configured. Database features disabled.");
    }

    // Load WASM module with database bridge
    let wasm = crate::wasm::create_shared_instance_with_db(&wasm_path, router.clone(), db_bridge)?;

    // Initialize WASM module (registers routes)
    wasm.initialize()?;

    // Check if any routes were registered
    if router.is_empty() {
        warn!("No routes were registered by the WASM module");
        warn!("The server will start but won't handle any requests");
    } else {
        info!("Registered {} routes:", router.len());
        for route in router.all_routes() {
            info!("  {} {} -> handler {}", route.method, route.path, route.handler_index);
        }
    }

    // Create app state
    let state = AppState::new(wasm, router);

    // Build Axum router
    let app = build_router(state, &config);

    // Start server
    let addr = config.socket_addr();
    info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| RuntimeError::server(format!("Failed to bind to {}: {}", addr, e)))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| RuntimeError::server(format!("Server error: {}", e)))?;

    info!("Server shut down gracefully");
    Ok(())
}

/// Build the Axum router with middleware
fn build_router(state: AppState, config: &ServerConfig) -> Router {
    let mut app = Router::new()
        // Catch-all handler that routes to WASM
        .fallback(handle_request)
        .with_state(state);

    // Add CORS if enabled
    if config.cors_enabled {
        let cors = if config.cors_origins.is_empty() {
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        } else {
            // Custom origins would go here
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        };
        app = app.layer(cors);
    }

    // Add tracing
    app = app.layer(TraceLayer::new_for_http());

    app
}

/// Handle all incoming requests
async fn handle_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let path = uri.path();
    let query_string = uri.query().unwrap_or("");

    debug!("Incoming request: {} {}", method, path);

    // Convert Axum method to our HttpMethod
    let http_method = match method {
        Method::GET => HttpMethod::GET,
        Method::POST => HttpMethod::POST,
        Method::PUT => HttpMethod::PUT,
        Method::PATCH => HttpMethod::PATCH,
        Method::DELETE => HttpMethod::DELETE,
        Method::HEAD => HttpMethod::HEAD,
        Method::OPTIONS => HttpMethod::OPTIONS,
        _ => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                "Method not allowed",
            )
                .into_response();
        }
    };

    // Find matching route
    let (route_handler, params) = match state.router.find(http_method, path) {
        Some(result) => result,
        None => {
            debug!("No route found for {} {}", method, path);
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }
    };

    debug!(
        "Matched route: {} {} -> handler {}",
        route_handler.method, route_handler.path, route_handler.handler_index
    );

    // Check authentication for protected routes
    if route_handler.protected {
        // TODO: Implement actual auth checking
        // For now, we'll check for an Authorization header
        if !headers.contains_key(header::AUTHORIZATION) {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }

        // Check role if required
        if let Some(required_role) = &route_handler.required_role {
            // TODO: Extract role from token and verify
            debug!("Route requires role: {}", required_role);
        }
    }

    // Parse query parameters
    let query_params: HashMap<String, String> = url::form_urlencoded::parse(query_string.as_bytes())
        .into_owned()
        .collect();

    // Convert headers
    let header_vec: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_string(), v.to_string()))
        })
        .collect();

    // Create request context
    let request_ctx = RequestContext {
        method: method.to_string(),
        path: path.to_string(),
        headers: header_vec,
        body,
        params,
        query: query_params,
    };

    // Call WASM handler
    match state.wasm.call_handler(route_handler.handler_index, request_ctx) {
        Ok(response_body) => {
            debug!("Handler returned: {} bytes", response_body.len());

            // Determine content type based on response
            let content_type = if response_body.starts_with('{') || response_body.starts_with('[') {
                "application/json"
            } else if response_body.starts_with("<!") || response_body.starts_with("<html") {
                "text/html; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(response_body))
                .unwrap()
        }
        Err(e) => {
            error!("Handler error: {}", e);
            let http_err = HttpError::from(e);

            Response::builder()
                .status(StatusCode::from_u16(http_err.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(http_err.to_json().to_string()))
                .unwrap()
        }
    }
}

/// Graceful shutdown signal handler
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C, shutting down...");
        }
        _ = terminate => {
            info!("Received termination signal, shutting down...");
        }
    }
}

/// Mask password in database URL for logging
fn mask_db_url(url: &str) -> String {
    // Pattern: protocol://user:password@host/db -> protocol://user:***@host/db
    if let Some(at_pos) = url.rfind('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            // Check if this colon is part of password (after ://)
            if colon_pos > 3 && &url[colon_pos - 1..colon_pos] != "/" {
                let protocol_end = url.find("://").map(|p| p + 3).unwrap_or(0);
                if colon_pos > protocol_end {
                    return format!(
                        "{}***{}",
                        &url[..colon_pos + 1],
                        &url[at_pos..]
                    );
                }
            }
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 3000);
        assert!(config.cors_enabled);
    }

    #[test]
    fn test_server_config_builder() {
        let config = ServerConfig::default()
            .with_port(8080)
            .with_host("127.0.0.1");

        assert_eq!(config.port, 8080);
        assert_eq!(config.host, "127.0.0.1");
    }

    #[test]
    fn test_socket_addr() {
        let config = ServerConfig::default().with_port(8080);
        let addr = config.socket_addr();
        assert_eq!(addr.port(), 8080);
    }
}
