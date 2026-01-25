//! WASM State Management
//!
//! Defines the state held by each WASM store instance and the core trait
//! that allows host functions to work with any compatible state type.

use crate::DbBridge;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;

/// Shared database bridge type
pub type SharedDbBridge = Arc<TokioRwLock<DbBridge>>;

// ============================================================================
// CORE TRAIT - Implement this to use host-bridge functions with any state
// ============================================================================

/// Core trait that any WASM state must implement to use host-bridge functions.
///
/// This allows different runtimes (CLI, server, embedded) to use the same
/// host function implementations while having their own extended state.
///
/// # Example
///
/// ```ignore
/// struct MyServerState {
///     memory: WasmMemory,
///     router: MyRouter,  // Server-specific
/// }
///
/// impl WasmStateCore for MyServerState {
///     fn memory(&self) -> &WasmMemory { &self.memory }
///     fn memory_mut(&mut self) -> &mut WasmMemory { &mut self.memory }
/// }
/// ```
pub trait WasmStateCore: Send + 'static {
    /// Get immutable reference to memory allocator
    fn memory(&self) -> &WasmMemory;

    /// Get mutable reference to memory allocator
    fn memory_mut(&mut self) -> &mut WasmMemory;

    /// Get database bridge (optional, returns None by default)
    fn db_bridge(&self) -> Option<SharedDbBridge> {
        None
    }

    /// Set last error message
    fn set_error(&mut self, _error: String) {
        // Default implementation does nothing
    }

    /// Get last error message
    fn last_error(&self) -> Option<&str> {
        None
    }

    // =========================================
    // HTTP SERVER METHODS (optional, for server runtimes)
    // =========================================

    /// Get the current request context (for HTTP server mode)
    fn request_context(&self) -> Option<&RequestContext> {
        None
    }

    /// Get the authentication context (for HTTP server mode)
    fn auth_context(&self) -> Option<&AuthContext> {
        None
    }

    /// Get the router interface (for HTTP server mode)
    fn router(&self) -> Option<Arc<dyn RouterInterface + Send + Sync>> {
        None
    }

    /// Get the server port
    fn port(&self) -> u16 {
        3000
    }

    /// Set the server port
    fn set_port(&mut self, _port: u16) {
        // Default implementation does nothing
    }

    /// Get immutable access to HTTP response being built
    fn http_response(&self) -> Option<&HttpResponseBuilder> {
        None
    }

    /// Get mutable access to HTTP response being built
    fn http_response_mut(&mut self) -> Option<&mut HttpResponseBuilder> {
        None
    }
}

/// Memory manager for WASM instance (bump allocator)
pub struct WasmMemory {
    /// Current allocation offset
    offset: usize,
}

impl WasmMemory {
    /// Create a new memory manager
    pub fn new() -> Self {
        Self {
            // Start allocation after initial memory region (64KB)
            // to avoid overwriting WASM's own data structures
            offset: 65536,
        }
    }

    /// Allocate memory and return the pointer
    pub fn allocate(&mut self, size: usize) -> usize {
        let ptr = self.offset;
        // Align to 8 bytes for safety
        self.offset = (self.offset + size + 7) & !7;
        ptr
    }

    /// Reset allocator (for between requests)
    pub fn reset(&mut self) {
        self.offset = 65536;
    }

    /// Get current allocation offset
    pub fn current_offset(&self) -> usize {
        self.offset
    }
}

impl Default for WasmMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// Request context passed to handlers
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub params: HashMap<String, String>,
    pub query: HashMap<String, String>,
}

impl Default for RequestContext {
    fn default() -> Self {
        Self {
            method: String::new(),
            path: String::new(),
            headers: Vec::new(),
            body: String::new(),
            params: HashMap::new(),
            query: HashMap::new(),
        }
    }
}

/// Authentication context
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: i32,
    pub role: String,
    pub session_id: Option<String>,
}

/// HTTP Response builder for server handlers
#[derive(Debug, Clone, Default)]
pub struct HttpResponseBuilder {
    /// Response status code
    pub status: u16,
    /// Response headers
    pub headers: HashMap<String, String>,
    /// Response body
    pub body: String,
    /// Redirect URL (if set, body is ignored)
    pub redirect_url: Option<String>,
    /// Whether the response has been finalized
    pub finalized: bool,
}

impl HttpResponseBuilder {
    /// Create a new response builder with default 200 status
    pub fn new() -> Self {
        Self {
            status: 200,
            headers: HashMap::new(),
            body: String::new(),
            redirect_url: None,
            finalized: false,
        }
    }

    /// Set the status code
    pub fn set_status(&mut self, status: u16) {
        self.status = status;
    }

    /// Set a header
    pub fn set_header(&mut self, name: String, value: String) {
        self.headers.insert(name, value);
    }

    /// Set the body
    pub fn set_body(&mut self, body: String) {
        self.body = body;
    }

    /// Set redirect
    pub fn set_redirect(&mut self, url: String, status: u16) {
        self.redirect_url = Some(url);
        self.status = status;
    }

    /// Check if this is a redirect response
    pub fn is_redirect(&self) -> bool {
        self.redirect_url.is_some()
    }

    /// Reset the builder for a new request
    pub fn reset(&mut self) {
        self.status = 200;
        self.headers.clear();
        self.body.clear();
        self.redirect_url = None;
        self.finalized = false;
    }
}

/// State held by each WASM store instance
pub struct WasmState {
    /// Memory allocator
    pub memory: WasmMemory,
    /// Server port (for _http_listen)
    pub port: u16,
    /// Current request context (if handling a request)
    pub request_context: Option<RequestContext>,
    /// Auth context (if authenticated)
    pub auth_context: Option<AuthContext>,
    /// HTTP response being built
    pub http_response: HttpResponseBuilder,
    /// Last error (for error reporting)
    pub last_error: Option<String>,
    /// Database bridge for database operations
    pub db_bridge: SharedDbBridge,
    /// Router reference (set by server)
    pub router: Option<Arc<dyn RouterInterface + Send + Sync>>,
}

/// Router interface for HTTP server integration
pub trait RouterInterface {
    fn register(
        &self,
        method: &str,
        path: String,
        handler_idx: u32,
        protected: bool,
        required_role: Option<String>,
    ) -> Result<(), String>;

    fn len(&self) -> usize;
}

impl WasmState {
    /// Create a new WasmState with default values
    pub fn new() -> Self {
        Self {
            memory: WasmMemory::new(),
            port: 3000,
            request_context: None,
            auth_context: None,
            http_response: HttpResponseBuilder::new(),
            last_error: None,
            db_bridge: Arc::new(TokioRwLock::new(DbBridge::new())),
            router: None,
        }
    }

    /// Create a new WasmState with a database bridge
    pub fn with_db_bridge(db_bridge: SharedDbBridge) -> Self {
        Self {
            memory: WasmMemory::new(),
            port: 3000,
            request_context: None,
            auth_context: None,
            http_response: HttpResponseBuilder::new(),
            last_error: None,
            db_bridge,
            router: None,
        }
    }

    /// Set the router for HTTP server operations
    pub fn with_router(mut self, router: Arc<dyn RouterInterface + Send + Sync>) -> Self {
        self.router = Some(router);
        self
    }

    /// Set request context for the current request
    pub fn set_request(&mut self, ctx: RequestContext) {
        self.request_context = Some(ctx);
        // Reset memory allocator and response builder for new request
        self.memory.reset();
        self.http_response.reset();
    }

    /// Clear request context
    pub fn clear_request(&mut self) {
        self.request_context = None;
        self.auth_context = None;
        self.http_response.reset();
    }
}

impl Default for WasmState {
    fn default() -> Self {
        Self::new()
    }
}

// Implement WasmStateCore for WasmState
impl WasmStateCore for WasmState {
    fn memory(&self) -> &WasmMemory {
        &self.memory
    }

    fn memory_mut(&mut self) -> &mut WasmMemory {
        &mut self.memory
    }

    fn db_bridge(&self) -> Option<SharedDbBridge> {
        Some(self.db_bridge.clone())
    }

    fn set_error(&mut self, error: String) {
        self.last_error = Some(error);
    }

    fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    // HTTP Server methods
    fn request_context(&self) -> Option<&RequestContext> {
        self.request_context.as_ref()
    }

    fn auth_context(&self) -> Option<&AuthContext> {
        self.auth_context.as_ref()
    }

    fn router(&self) -> Option<Arc<dyn RouterInterface + Send + Sync>> {
        self.router.clone()
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn set_port(&mut self, port: u16) {
        self.port = port;
    }

    fn http_response(&self) -> Option<&HttpResponseBuilder> {
        Some(&self.http_response)
    }

    fn http_response_mut(&mut self) -> Option<&mut HttpResponseBuilder> {
        Some(&mut self.http_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_memory_allocator() {
        let mut mem = WasmMemory::new();

        let ptr1 = mem.allocate(100);
        assert_eq!(ptr1, 65536);

        let ptr2 = mem.allocate(200);
        // 65536 + 100 = 65636, aligned to 8 = 65640
        assert_eq!(ptr2, 65640);

        mem.reset();
        let ptr3 = mem.allocate(50);
        assert_eq!(ptr3, 65536);
    }

    #[test]
    fn test_wasm_state() {
        let state = WasmState::new();
        assert!(state.request_context.is_none());
        assert_eq!(state.port, 3000);
    }
}
