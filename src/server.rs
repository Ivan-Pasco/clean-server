//! HTTP Server Implementation
//!
//! Uses Axum to serve HTTP requests and route them to WASM handlers.

use crate::build_manifest::{BuildManifest, ResolvedArtifact, purpose as artifact_purpose};
use crate::error::{HttpError, RuntimeError, RuntimeResult};
use crate::router::{HttpMethod, SharedRouter};
use crate::session::{SharedSessionStore, parse_cookies};
use crate::wasm::{AuthContext, RequestContext, SharedDbBridge, SharedIslandsStore, SharedWasmInstance};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use host_bridge::{DbBridge, DbConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::RwLock as TokioRwLock;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, warn};

/// Adapter that turns a tokio unbounded-channel receiver into a `futures::Stream`
/// of `Bytes` chunks — used to stream SSE frames as the WASM handler produces them.
struct SseStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl futures::Stream for SseStream {
    type Item = Result<bytes::Bytes, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(s)) => {
                std::task::Poll::Ready(Some(Ok(bytes::Bytes::from(s))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Memory budget tier for WASM instances
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    /// 8 MB — lightweight utilities, CLI tools
    Minimal,
    /// 32 MB — standard web applications (default)
    Standard,
    /// 128 MB — data-heavy workloads
    Large,
    /// 512 MB — maximum for special cases
    XLarge,
}

impl MemoryTier {
    /// Maximum bytes allowed for this tier
    pub fn max_bytes(self) -> usize {
        match self {
            MemoryTier::Minimal => 8 * 1024 * 1024,
            MemoryTier::Standard => 32 * 1024 * 1024,
            MemoryTier::Large => 128 * 1024 * 1024,
            MemoryTier::XLarge => 512 * 1024 * 1024,
        }
    }
}

impl std::str::FromStr for MemoryTier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "minimal" => Ok(MemoryTier::Minimal),
            "standard" => Ok(MemoryTier::Standard),
            "large" => Ok(MemoryTier::Large),
            "xlarge" => Ok(MemoryTier::XLarge),
            _ => Err(format!(
                "Unknown memory tier '{}'. Valid tiers: minimal, standard, large, xlarge",
                s
            )),
        }
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryTier::Minimal => write!(f, "minimal"),
            MemoryTier::Standard => write!(f, "standard"),
            MemoryTier::Large => write!(f, "large"),
            MemoryTier::XLarge => write!(f, "xlarge"),
        }
    }
}

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
    /// Memory budget tier for WASM instances
    pub memory_tier: MemoryTier,
    /// Explicit memory limit in bytes (overrides tier if set)
    pub memory_limit: Option<usize>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let memory_tier = std::env::var("CLEAN_MEMORY_TIER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(MemoryTier::Standard);

        let memory_limit = std::env::var("CLEAN_MEMORY_LIMIT_MB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|mb| mb * 1024 * 1024);

        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            cors_enabled: true,
            cors_origins: vec![],
            body_limit: 10 * 1024 * 1024, // 10MB
            database_url: std::env::var("DATABASE_URL").ok(),
            database_max_connections: 10,
            memory_tier,
            memory_limit,
        }
    }
}

impl ServerConfig {
    /// Effective memory limit in bytes: explicit limit if set, otherwise tier default
    pub fn effective_memory_limit(&self) -> usize {
        self.memory_limit.unwrap_or_else(|| self.memory_tier.max_bytes())
    }

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

    pub fn with_memory_tier(mut self, tier: MemoryTier) -> Self {
        self.memory_tier = tier;
        self
    }

    pub fn with_memory_limit_mb(mut self, mb: usize) -> Self {
        self.memory_limit = Some(mb * 1024 * 1024);
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
    /// Island components registered for client-side hydration
    islands_store: SharedIslandsStore,
    /// Content of the frame.ui runtime loader.js served at /loader.js
    loader_js: Arc<String>,
    /// Resolved path to the client-hydration artifact (`frontend.wasm`) when
    /// the build manifest declared one. `None` triggers the legacy CWD +
    /// `public/` probe for Phase B compatibility (manifest absent).
    frontend_wasm_path: Option<Arc<PathBuf>>,
}

impl AppState {
    pub fn new(
        wasm: SharedWasmInstance,
        router: SharedRouter,
        islands_store: SharedIslandsStore,
        loader_js: Arc<String>,
        frontend_wasm_path: Option<Arc<PathBuf>>,
    ) -> Self {
        Self {
            wasm,
            router,
            islands_store,
            loader_js,
            frontend_wasm_path,
        }
    }
}

/// Load the frame.ui runtime loader.js from the installed plugin.
/// Falls back to the embedded stub when the plugin is not installed.
fn load_runtime_loader_js() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let plugin_path = format!("{}/.cleen/plugins/frame.ui/runtime/loader.js", home);
    match std::fs::read_to_string(&plugin_path) {
        Ok(content) => {
            info!("Serving frame.ui loader.js from plugin: {}", plugin_path);
            content
        }
        Err(_) => {
            info!("frame.ui plugin loader not found at {}; using embedded stub", plugin_path);
            LOADER_JS.to_string()
        }
    }
}

/// Complete JavaScript hydration runtime, ES5-compatible for maximum browser support.
///
/// The loader fetches `/islands-manifest.json` on page load, then schedules
/// hydration of every `[data-island][data-client]` element according to its
/// `data-client` strategy ("on", "visible", "idle", or "only").
const LOADER_JS: &str = r#"(function() {
  'use strict';

  var manifest = null;

  function fetchManifest() {
    return fetch('/islands-manifest.json')
      .then(function(r) { return r.json(); })
      .then(function(data) { manifest = data; return data; });
  }

  function loadIsland(el) {
    var componentName = el.getAttribute('data-island');
    if (!componentName || !manifest) return;

    var entry = null;
    for (var i = 0; i < manifest.islands.length; i++) {
      if (manifest.islands[i].component === componentName) {
        entry = manifest.islands[i];
        break;
      }
    }
    if (!entry) return;

    var propsAttr = el.getAttribute('data-props');
    var props = propsAttr ? JSON.parse(propsAttr) : {};

    fetch(entry.module)
      .then(function(r) { return r.arrayBuffer(); })
      .then(function(bytes) { return WebAssembly.instantiate(bytes, buildImports(el, props)); })
      .then(function(result) {
        var instance = result.instance;
        if (instance.exports.hydrate) {
          instance.exports.hydrate();
        } else if (instance.exports.render) {
          instance.exports.render();
        }
        el.setAttribute('data-hydrated', 'true');
      })
      .catch(function(err) {
        console.error('[islands] Failed to load island "' + componentName + '":', err);
      });
  }

  function buildImports(el, props) {
    return {
      env: {
        _dom_set_text: function(ptr, len) {},
        _dom_set_attr: function(namePtr, nameLen, valPtr, valLen) {},
        _dom_add_event: function(evtPtr, evtLen, handlerIdx) {},
        _dom_get_prop: function(keyPtr, keyLen) { return 0; }
      }
    };
  }

  function hydrateAll() {
    var elements = document.querySelectorAll('[data-island][data-client]');
    if (elements.length === 0) return;

    fetchManifest().then(function() {
      for (var i = 0; i < elements.length; i++) {
        var el = elements[i];
        var strategy = el.getAttribute('data-client');
        scheduleHydration(el, strategy);
      }
    });
  }

  function scheduleHydration(el, strategy) {
    switch (strategy) {
      case 'on':
        loadIsland(el);
        break;
      case 'visible':
        if ('IntersectionObserver' in window) {
          var observer = new IntersectionObserver(function(entries) {
            for (var i = 0; i < entries.length; i++) {
              if (entries[i].isIntersecting) {
                loadIsland(entries[i].target);
                observer.unobserve(entries[i].target);
              }
            }
          });
          observer.observe(el);
        } else {
          loadIsland(el);
        }
        break;
      case 'idle':
        if ('requestIdleCallback' in window) {
          requestIdleCallback(function() { loadIsland(el); });
        } else {
          setTimeout(function() { loadIsland(el); }, 200);
        }
        break;
      case 'only':
        loadIsland(el);
        break;
      default:
        loadIsland(el);
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', hydrateAll);
  } else {
    hydrateAll();
  }
})();
"#;

async fn configure_db_bridge(config: &ServerConfig) -> SharedDbBridge {
    let db_bridge: SharedDbBridge = Arc::new(TokioRwLock::new(DbBridge::new()));

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
            Err(e) => warn!(
                "Failed to initialize database: {}. Database features will be unavailable.",
                e
            ),
        }
    } else {
        info!("No DATABASE_URL configured. Database features disabled.");
    }

    db_bridge
}

/// Start the HTTP server with the given WASM module
pub async fn start_server(wasm_path: PathBuf, config: ServerConfig) -> RuntimeResult<()> {
    info!("Starting Frame Runtime server");
    info!("Loading WASM module from {:?}", wasm_path);

    // Create shared router
    let router = crate::router::create_shared_router();

    // Configure database bridge
    let db_bridge = configure_db_bridge(&config).await;

    // Load WASM module with database bridge and memory limit
    let wasm = crate::wasm::create_shared_instance_with_config(
        &wasm_path,
        router.clone(),
        db_bridge,
        config.effective_memory_limit(),
    )?;

    // Initialize WASM module (registers routes and static dirs)
    wasm.initialize()?;

    // Check if any routes were registered
    if router.is_empty() {
        warn!("No routes were registered by the WASM module");
        warn!("The server will start but won't handle any requests");
    } else {
        info!("Registered {} routes:", router.len());
        for route in router.all_routes() {
            info!(
                "  {} {} -> handler {}",
                route.method, route.path, route.handler_name
            );
        }
    }

    // Collect static dirs registered during initialization
    let mut static_dirs = wasm.static_dirs().read().expect("store lock poisoned").clone();

    // Auto-mount ./public/ at /public if the directory exists and not already registered
    if std::path::Path::new("public").is_dir()
        && !static_dirs.iter().any(|(prefix, _)| prefix == "/public")
    {
        info!("Auto-mounting ./public/ at /public — link assets as /public/<filename>");
        static_dirs.push(("/public".to_string(), "public".to_string()));
    }

    for (prefix, dir) in &static_dirs {
        info!("Serving static files: {} -> {}", prefix, dir);
    }

    // Collect islands registered during initialization
    let islands_store = wasm.islands_store().clone();
    {
        let store = islands_store.read().expect("store lock poisoned");
        if store.islands.is_empty() {
            info!("No island components registered");
        } else {
            info!("Registered {} island component(s):", store.islands.len());
            for island in &store.islands {
                info!(
                    "  {} -> {} (hydration: {})",
                    island.component, island.module, island.hydration
                );
            }
        }
    }

    // Load the frame.ui runtime loader.js (plugin installation preferred over embedded stub)
    let loader_js = Arc::new(load_runtime_loader_js());

    // Read the build manifest (Plugin Contracts v2 — see contracts/artifacts.md §5).
    // The manifest sits next to the WASM and tells us where each artifact lives
    // (`frontend.wasm`, future `theme.css`, etc.). When the manifest is absent
    // we fall back to legacy CWD/public probing for Phase B compatibility.
    let main_wasm_dir = wasm_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let resolved_artifacts: Vec<ResolvedArtifact> = match BuildManifest::load_alongside(&wasm_path) {
        Ok(Some(manifest)) => {
            info!(
                "Loaded build manifest (compiler {}, {} artifact(s))",
                manifest.compiler_version,
                manifest.artifacts.len()
            );
            manifest.resolve_artifacts(&main_wasm_dir)
        }
        Ok(None) => {
            debug!(
                "No build-manifest.json next to {:?}; using legacy artifact discovery",
                wasm_path
            );
            Vec::new()
        }
        Err(e) => {
            warn!(
                "Build manifest present but unreadable; falling back to legacy lookup: {}",
                e
            );
            Vec::new()
        }
    };
    let frontend_wasm_path: Option<Arc<PathBuf>> = resolved_artifacts
        .iter()
        .find(|a| a.purpose == artifact_purpose::CLIENT_HYDRATION)
        .map(|a| Arc::new(a.absolute_path.clone()));
    if let Some(p) = &frontend_wasm_path {
        info!("Manifest-resolved frontend.wasm at {:?}", p.as_ref());
    }

    // Create app state
    let state = AppState::new(wasm, router, islands_store, loader_js, frontend_wasm_path);

    // Build Axum router
    let app = build_router(state, &config, static_dirs, &resolved_artifacts);

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
fn build_router(
    state: AppState,
    config: &ServerConfig,
    static_dirs: Vec<(String, String)>,
    resolved_artifacts: &[ResolvedArtifact],
) -> Router {
    // Reserved routes that already have explicit handlers below. Manifest
    // artifacts targeting these paths fall back to the dedicated handler
    // rather than registering a duplicate route.
    const RESERVED_ARTIFACT_NAMES: &[&str] =
        &["frontend.wasm", "loader.js", "islands-manifest.json"];

    let mut app = Router::new()
        // Built-in islands routes — registered before the fallback so they always take priority
        .route("/islands-manifest.json", axum::routing::get(serve_islands_manifest))
        .route("/loader.js", axum::routing::get(serve_loader_js))
        .route("/frontend.wasm", axum::routing::get(serve_frontend_wasm));

    // Register routes for every public artifact the manifest declared (other
    // than the ones with dedicated handlers above). Today this covers
    // future plugin-declared assets like `theme.css` (frame.ui) without
    // requiring per-plugin code in the server.
    // See contracts/artifacts.md §8.2.
    for artifact in resolved_artifacts {
        if !artifact.public {
            continue;
        }
        if RESERVED_ARTIFACT_NAMES.contains(&artifact.name.as_str()) {
            continue;
        }
        let route_path = format!("/{}", artifact.name);
        let absolute_path = artifact.absolute_path.clone();
        let content_type = artifact.content_type.clone();
        info!(
            "Manifest artifact route: {} -> {:?} ({})",
            route_path, absolute_path, content_type
        );
        app = app.route(
            &route_path,
            axum::routing::get(move || {
                let absolute_path = absolute_path.clone();
                let content_type = content_type.clone();
                async move { serve_manifest_artifact(absolute_path, content_type).await }
            }),
        );
    }

    let mut app = app
        // Catch-all handler that routes to WASM
        .fallback(handle_request)
        .with_state(state);

    // Mount static file directories (take priority over fallback)
    for (prefix, dir) in &static_dirs {
        app = app.nest_service(
            prefix.as_str(),
            ServeDir::new(dir).append_index_html_on_directories(true),
        );
    }

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

/// Serve the islands manifest as JSON.
///
/// Returns the list of registered island components so the client-side loader
/// knows which WASM module to fetch for each `[data-island]` element.
/// Cache-Control is set to `no-cache` because the manifest is regenerated on
/// every server start and may change between deployments.
async fn serve_islands_manifest(State(state): State<AppState>) -> Response {
    #[derive(serde::Serialize)]
    struct Manifest<'a> {
        islands: &'a Vec<crate::wasm::IslandEntry>,
    }

    let store = state.islands_store.read().expect("store lock poisoned");
    let manifest = Manifest {
        islands: &store.islands,
    };

    match serde_json::to_string(&manifest) {
        Ok(json) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(json))
            .expect("response builder"),
        Err(e) => {
            error!("Failed to serialize islands manifest: {}", e);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"error":"Failed to generate islands manifest"}"#))
                .expect("response builder")
        }
    }
}

/// Serve the client-side hydration loader JavaScript.
///
/// Prefers the installed frame.ui runtime loader over the embedded stub.
/// The script sets up the client bridge between the browser and frontend.wasm.
async fn serve_loader_js(State(state): State<AppState>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/javascript")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(state.loader_js.as_ref().clone()))
        .expect("response builder")
}

/// Serve the compiled client-side WASM runtime at /frontend.wasm.
///
/// Manifest-first per contracts/artifacts.md §5: when the build manifest
/// declared a `client_hydration` artifact, serve the file at the resolved
/// path. When the manifest is absent (Phase B compatibility), fall back to
/// the legacy CWD + `public/` probe. When the manifest IS present but the
/// resolved file is missing, return 404 immediately rather than searching
/// ambient directories — the manifest is the source of truth.
async fn serve_frontend_wasm(State(state): State<AppState>) -> Response {
    if let Some(path) = &state.frontend_wasm_path {
        match std::fs::read(path.as_ref()) {
            Ok(bytes) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/wasm")
                    .header(header::CACHE_CONTROL, "public, max-age=60")
                    .body(Body::from(bytes))
                    .expect("response builder");
            }
            Err(e) => {
                error!(
                    "Manifest-declared frontend.wasm missing at {:?}: {}",
                    path.as_ref(),
                    e
                );
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(format!(
                        "frontend.wasm declared in build-manifest.json but not found at {:?}",
                        path.as_ref()
                    )))
                    .expect("response builder");
            }
        }
    }

    // Phase B fallback: no manifest entry — probe legacy locations.
    let candidates = ["frontend.wasm", "public/frontend.wasm"];
    for path in &candidates {
        match std::fs::read(path) {
            Ok(bytes) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/wasm")
                    .header(header::CACHE_CONTROL, "public, max-age=60")
                    .body(Body::from(bytes))
                    .expect("response builder");
            }
            Err(_) => continue,
        }
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(
            "frontend.wasm not found — compile client components to generate it",
        ))
        .expect("response builder")
}

/// Serve a manifest-declared public artifact (e.g. `theme.css`).
///
/// Reads the file from the manifest-resolved absolute path and returns it
/// with the declared `Content-Type`. Returns 404 when the file is missing —
/// the manifest is authoritative, no ambient-directory search.
async fn serve_manifest_artifact(absolute_path: PathBuf, content_type: String) -> Response {
    match std::fs::read(&absolute_path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .body(Body::from(bytes))
            .expect("response builder"),
        Err(e) => {
            error!(
                "Manifest-declared artifact missing at {:?}: {}",
                absolute_path, e
            );
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(format!(
                    "artifact declared in build-manifest.json but not found at {:?}",
                    absolute_path
                )))
                .expect("response builder")
        }
    }
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
            return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response();
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
        "Matched route: {} {} -> handler {} (extracted {} params)",
        route_handler.method, route_handler.path, route_handler.handler_name, params.len()
    );
    debug!("Extracted route params: {:?}", params);

    // Try to extract auth context from session cookie
    let auth_context = extract_auth_from_headers(&headers, state.wasm.session_store());

    // Check authentication for protected routes
    if route_handler.protected {
        // Check if user is authenticated
        if auth_context.is_none() {
            debug!("Protected route requires authentication");
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"ok":false,"error":"Unauthorized"}"#))
                .expect("response builder");
        }

        // Check role if required
        if let Some(required_role) = &route_handler.required_role {
            let has_role = auth_context
                .as_ref()
                .map(|ctx| ctx.role == *required_role || ctx.role == "admin")
                .unwrap_or(false);

            if !has_role {
                debug!(
                    "Route requires role '{}' but user has '{}'",
                    required_role,
                    auth_context.as_ref().map(|c| c.role.as_str()).unwrap_or("none")
                );
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"ok":false,"error":"Forbidden"}"#))
                    .expect("response builder");
            }
        }
    }

    // Parse query parameters
    let query_params: HashMap<String, String> =
        url::form_urlencoded::parse(query_string.as_bytes())
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
    debug!("handle_request: Creating RequestContext with params: {:?}", params);
    let request_ctx = RequestContext {
        method: method.to_string(),
        path: path.to_string(),
        headers: header_vec,
        body,
        params,
        query: query_params,
    };
    debug!("handle_request: RequestContext params: {:?}", request_ctx.params);

    // Static redirect routes: registered via _http_redirect_route, no WASM handler needed.
    if let Some((to_path, status_code)) = &route_handler.redirect_destination {
        debug!("Static redirect: {} -> {} ({})", request_ctx.path, to_path, status_code);
        return Response::builder()
            .status(StatusCode::from_u16(*status_code).unwrap_or(StatusCode::FOUND))
            .header(header::LOCATION, to_path.as_str())
            .body(Body::empty())
            .expect("redirect response builder");
    }

    // SSE (STREAM) routes keep the connection open and stream frames as the handler emits them.
    if route_handler.is_sse {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let wasm = state.wasm.clone();
        let handler_name = route_handler.handler_name.clone();

        // Run the WASM handler on a blocking thread so it can loop calling _sse_emit*.
        // When the handler returns (or calls _sse_close), the sender is dropped and
        // the stream EOF is delivered to the client.
        tokio::task::spawn_blocking(move || {
            if let Err(e) = wasm.call_handler_sse(&handler_name, request_ctx, auth_context, tx) {
                error!("SSE handler {} error: {}", handler_name, e);
            }
        });

        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("X-Accel-Buffering", "no")
            .body(Body::from_stream(SseStream { rx }))
            .expect("SSE response builder");
    }

    // Call WASM handler with auth context
    match state
        .wasm
        .call_handler_with_auth(&route_handler.handler_name, request_ctx, auth_context)
    {
        Ok(handler_response) => {
            debug!("Handler returned: {} bytes", handler_response.body.len());

            // Check for redirect first
            if let Some((status_code, redirect_url)) = handler_response.redirect {
                debug!("Redirecting {} -> {}", status_code, redirect_url);
                let mut builder = Response::builder()
                    .status(StatusCode::from_u16(status_code).unwrap_or(StatusCode::FOUND))
                    .header(header::LOCATION, &redirect_url);

                // Add Set-Cookie header if pending (for post-login redirects)
                if let Some(cookie) = handler_response.set_cookie {
                    builder = builder.header(header::SET_COOKIE, cookie);
                }

                // Add custom headers
                for (name, value) in handler_response.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }

                return builder.body(Body::empty()).expect("response builder");
            }

            // Use caller-supplied Content-Type if present (e.g. from _http_respond),
            // otherwise infer from the response body.
            let explicit_content_type = handler_response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone());

            let content_type = explicit_content_type.as_deref().unwrap_or_else(|| {
                if handler_response.body.starts_with('{') || handler_response.body.starts_with('[') {
                    "application/json"
                } else if handler_response.body.starts_with("<!") || handler_response.body.starts_with("<html") {
                    "text/html; charset=utf-8"
                } else {
                    "text/plain; charset=utf-8"
                }
            });

            let status = handler_response
                .status
                .and_then(|s| StatusCode::from_u16(s).ok())
                .unwrap_or(StatusCode::OK);

            let mut builder = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type);

            // Add Set-Cookie header if pending
            if let Some(cookie) = handler_response.set_cookie {
                debug!("Setting cookie: {}", cookie);
                builder = builder.header(header::SET_COOKIE, cookie);
            }

            // Add custom headers, skipping Content-Type (already applied above)
            for (name, value) in handler_response.headers {
                if !name.eq_ignore_ascii_case("content-type") {
                    debug!("Setting header: {}={}", name, value);
                    builder = builder.header(name.as_str(), value.as_str());
                }
            }

            // Inject accumulated <style> and <link> tags into HTML responses
            let body = inject_head_tags(
                handler_response.body,
                handler_response.head_css,
                handler_response.head_links,
            );

            builder.body(Body::from(body)).expect("response builder")
        }
        Err(e) => {
            error!("Handler error: {}", e);
            let http_err = HttpError::from(e);

            Response::builder()
                .status(
                    StatusCode::from_u16(http_err.status)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(http_err.to_json().to_string()))
                .expect("response builder")
        }
    }
}

/// Extract auth context from request headers (session cookie or JWT)
fn extract_auth_from_headers(
    headers: &HeaderMap,
    session_store: &SharedSessionStore,
) -> Option<AuthContext> {
    // Try to get session from cookie first
    if let Some(cookie_header) = headers.get(header::COOKIE)
        && let Ok(cookie_str) = cookie_header.to_str()
    {
        let cookies = parse_cookies(cookie_str);

        // Try common session cookie names
        let session_id = cookies.get("session")
            .or_else(|| cookies.get("todo.sid"))
            .or_else(|| cookies.get("sid"));

        if let Some(session_id) = session_id {
            // Look up session
            let mut store = session_store.write().expect("store lock poisoned");
            if let Some(session) = store.get(session_id) {
                debug!("Found valid session {} for user {}", session.session_id, session.user_id);
                return Some(AuthContext {
                    user_id: session.user_id,
                    role: session.role,
                    session_id: Some(session.session_id),
                });
            }
        }
    }

    // Try Bearer token from Authorization header
    if let Some(auth_header) = headers.get(header::AUTHORIZATION)
        && let Ok(auth_str) = auth_header.to_str()
        && let Some(token) = auth_str.strip_prefix("Bearer ")
    {
        // For now, just log that we received a token
        // Full JWT validation would go here
        debug!("Received Bearer token: {}...", &token[..token.len().min(10)]);
        // JWT validation would return AuthContext here
    }

    None
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
    if let Some(at_pos) = url.rfind('@')
        && let Some(colon_pos) = url[..at_pos].rfind(':')
        && colon_pos > 3
        && &url[colon_pos - 1..colon_pos] != "/"
    {
        let protocol_end = url.find("://").map(|p| p + 3).unwrap_or(0);
        if colon_pos > protocol_end {
            return format!("{}***{}", &url[..colon_pos + 1], &url[at_pos..]);
        }
    }
    url.to_string()
}

/// Inject accumulated `<style>` and `<link rel="stylesheet">` tags before `</head>`.
/// If the response is not HTML or has no `</head>`, returns the body unchanged.
fn inject_head_tags(body: String, css: Vec<String>, links: Vec<String>) -> String {
    if css.is_empty() && links.is_empty() {
        return body;
    }
    let close_head = body.find("</head>");
    let close_head = match close_head {
        Some(pos) => pos,
        None => return body,
    };

    let mut injection = String::new();
    for href in &links {
        injection.push_str(&format!("<link rel=\"stylesheet\" href=\"{}\">\n", href));
    }
    for style in &css {
        injection.push_str(&format!("<style>{}</style>\n", style));
    }

    let mut result = String::with_capacity(body.len() + injection.len());
    result.push_str(&body[..close_head]);
    result.push_str(&injection);
    result.push_str(&body[close_head..]);
    result
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

    #[tokio::test]
    async fn serve_manifest_artifact_returns_file_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("theme.css");
        std::fs::write(&css_path, b":root{--c:0}").unwrap();

        let response =
            serve_manifest_artifact(css_path.clone(), "text/css".to_string()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/css")
        );
    }

    #[tokio::test]
    async fn serve_manifest_artifact_returns_404_when_missing() {
        // Manifest-declared artifact is missing on disk. Spec §5: NO ambient
        // search — must return 404 instead of probing CWD or public/.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.css");

        let response =
            serve_manifest_artifact(missing.clone(), "text/css".to_string()).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn build_manifest_resolves_frontend_wasm_to_main_wasm_dir() {
        // End-to-end check that the loader resolves frontend.wasm against the
        // WASM directory (not CWD) — the SRV004 contract guarantee.
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        let main_wasm = dist.join("app.wasm");
        std::fs::write(&main_wasm, b"WASM").unwrap();
        std::fs::write(dist.join("frontend.wasm"), b"FRONTEND").unwrap();
        std::fs::write(
            dist.join(crate::build_manifest::BUILD_MANIFEST_FILENAME),
            r#"{
                "schema_version": "1.0.0",
                "compiler_version": "0.30.257",
                "artifacts": [
                    {
                        "name": "frontend.wasm",
                        "path_relative": "frontend.wasm",
                        "purpose": "client_hydration",
                        "public": true,
                        "content_type": "application/wasm"
                    }
                ]
            }"#,
        )
        .unwrap();

        let manifest = BuildManifest::load_alongside(&main_wasm).unwrap().unwrap();
        let resolved = manifest.resolve_artifacts(&dist);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "frontend.wasm");
        assert_eq!(resolved[0].absolute_path, dist.join("frontend.wasm"));
        assert!(resolved[0].public);
    }
}
