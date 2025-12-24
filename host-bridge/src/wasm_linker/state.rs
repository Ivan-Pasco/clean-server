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
    pub user_id: i64,
    pub role: String,
    pub session_id: Option<String>,
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
        // Reset memory allocator for new request
        self.memory.reset();
    }

    /// Clear request context
    pub fn clear_request(&mut self) {
        self.request_context = None;
        self.auth_context = None;
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
