//! WASM Module Loading and Execution
//!
//! Handles loading compiled Clean Language WASM modules and executing route handlers.

use crate::bridge::create_linker;
use crate::error::{RuntimeError, RuntimeResult};
use crate::router::SharedRouter;
use host_bridge::{DbBridge, WasmMemory, WasmStateCore};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, info, warn};
use wasmtime::{Engine, Instance, Module, Store};

/// Shared database bridge type
pub type SharedDbBridge = Arc<TokioRwLock<DbBridge>>;

/// State held by each WASM store instance
pub struct WasmState {
    /// Memory allocator
    pub memory: WasmMemory,
    /// Router for registering routes
    pub router: SharedRouter,
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
}

/// Request context passed to handlers
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub params: std::collections::HashMap<String, String>,
    pub query: std::collections::HashMap<String, String>,
}

/// Authentication context
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: i64,
    pub role: String,
    pub session_id: Option<String>,
}

impl WasmState {
    pub fn new(router: SharedRouter) -> Self {
        Self {
            memory: WasmMemory::new(),
            router,
            port: 3000,
            request_context: None,
            auth_context: None,
            last_error: None,
            db_bridge: Arc::new(TokioRwLock::new(DbBridge::new())),
        }
    }

    pub fn with_db_bridge(router: SharedRouter, db_bridge: SharedDbBridge) -> Self {
        Self {
            memory: WasmMemory::new(),
            router,
            port: 3000,
            request_context: None,
            auth_context: None,
            last_error: None,
            db_bridge,
        }
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

// Implement WasmStateCore trait from host-bridge
// This allows clean-server to use all shared host functions from host-bridge
impl WasmStateCore for WasmState {
    fn memory(&self) -> &WasmMemory {
        &self.memory
    }

    fn memory_mut(&mut self) -> &mut WasmMemory {
        &mut self.memory
    }

    fn db_bridge(&self) -> Option<host_bridge::SharedDbBridge> {
        Some(self.db_bridge.clone())
    }

    fn set_error(&mut self, error: String) {
        self.last_error = Some(error);
    }

    fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

/// WASM module instance ready for execution
pub struct WasmInstance {
    /// Wasmtime engine
    engine: Engine,
    /// Compiled module (thread-safe, can create instances from this)
    module: Module,
    /// Shared router
    router: SharedRouter,
    /// Linker for creating instances
    linker: wasmtime::Linker<WasmState>,
    /// Database bridge (shared with state for external configuration)
    db_bridge: SharedDbBridge,
}

impl WasmInstance {
    /// Load a WASM module from a file
    pub fn load(wasm_path: &Path, router: SharedRouter) -> RuntimeResult<Self> {
        info!("Loading WASM module from {:?}", wasm_path);

        // Read WASM bytes
        let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
            RuntimeError::wasm(format!("Failed to read WASM file {:?}: {}", wasm_path, e))
        })?;

        Self::from_bytes(&wasm_bytes, router)
    }

    /// Load a WASM module from a file with a custom database bridge
    pub fn load_with_db(
        wasm_path: &Path,
        router: SharedRouter,
        db_bridge: SharedDbBridge,
    ) -> RuntimeResult<Self> {
        info!("Loading WASM module from {:?}", wasm_path);

        let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
            RuntimeError::wasm(format!("Failed to read WASM file {:?}: {}", wasm_path, e))
        })?;

        Self::from_bytes_with_db(&wasm_bytes, router, db_bridge)
    }

    /// Load a WASM module from bytes
    pub fn from_bytes(wasm_bytes: &[u8], router: SharedRouter) -> RuntimeResult<Self> {
        let db_bridge = Arc::new(TokioRwLock::new(DbBridge::new()));
        Self::from_bytes_with_db(wasm_bytes, router, db_bridge)
    }

    /// Load a WASM module from bytes with a custom database bridge
    pub fn from_bytes_with_db(
        wasm_bytes: &[u8],
        router: SharedRouter,
        db_bridge: SharedDbBridge,
    ) -> RuntimeResult<Self> {
        // Create engine
        let engine = Engine::default();

        // Compile module
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| RuntimeError::wasm(format!("Failed to compile WASM module: {}", e)))?;

        debug!("WASM module compiled successfully");

        // Create linker with host functions
        let linker = create_linker(&engine)?;

        debug!("WASM linker configured");

        Ok(Self {
            engine,
            module,
            router,
            linker,
            db_bridge,
        })
    }

    /// Create a fresh WASM instance for request handling
    fn create_instance(&self) -> RuntimeResult<(Store<WasmState>, Instance)> {
        let state = WasmState::with_db_bridge(self.router.clone(), self.db_bridge.clone());
        let mut store = Store::new(&self.engine, state);

        let instance = self.linker.instantiate(&mut store, &self.module).map_err(|e| {
            RuntimeError::wasm(format!("Failed to instantiate WASM module: {}", e))
        })?;

        Ok((store, instance))
    }

    /// Get the database bridge for configuration
    pub fn db_bridge(&self) -> &SharedDbBridge {
        &self.db_bridge
    }

    /// Initialize the module (calls main/start function to register routes)
    pub fn initialize(&self) -> RuntimeResult<()> {
        // Create an instance specifically for initialization
        let (mut store, instance) = self.create_instance()?;

        // Read the heap pointer from WASM Global[0], NOT from memory[0]
        // Clean Language native malloc stores heap_ptr in Global index 0, initialized to 65536
        if let Some(heap_global) = instance.get_global(&mut store, "__heap_ptr") {
            let heap_ptr = heap_global.get(&mut store).i32().unwrap_or(-1);
            info!("Initial heap pointer from __heap_ptr global: {}", heap_ptr);
        } else {
            // Fallback: try to read from exported global or log that it's not available
            info!("No __heap_ptr global exported - heap pointer tracking unavailable");
        }

        // Try different entry point names
        let entry_names = ["main", "_start", "start", "init"];

        for name in entry_names {
            if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, name) {
                info!("Calling WASM entry point: {}", name);

                func.call(&mut store, ()).map_err(|e| {
                    RuntimeError::wasm(format!("Failed to call {}: {}", name, e))
                })?;

                // Check if routes were registered
                let route_count = self.router.len();
                info!("Module initialized with {} routes", route_count);

                return Ok(());
            }
        }

        // No entry point found, but module might still work
        // (routes could be registered via exports or other means)
        warn!("No entry point found in WASM module");
        Ok(())
    }

    /// Call a route handler function
    /// Creates a fresh WASM instance for each request to ensure clean memory state
    pub fn call_handler(
        &self,
        handler_index: u32,
        request: RequestContext,
    ) -> RuntimeResult<String> {
        // Create a fresh instance for this request
        // This ensures each request starts with clean memory (no heap exhaustion)
        let (mut store, instance) = self.create_instance()?;

        // Set request context
        store.data_mut().set_request(request);

        // Get memory
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| RuntimeError::wasm("Module has no memory export"))?;

        // The handler function takes no arguments and returns a string pointer
        // In Clean Language, handlers are generated like:
        // func __handler_0() -> i32 { return string_ptr }

        let handler_name = format!("__route_handler_{}", handler_index);
        debug!("Calling handler: {}", handler_name);

        // Try the generated handler name first
        if let Ok(handler) = instance
            .get_typed_func::<(), i32>(&mut store, &handler_name)
        {
            let result_ptr = handler.call(&mut store, ()).map_err(|e| {
                RuntimeError::wasm(format!("Handler {} failed: {}", handler_name, e))
            })?;

            // Read result string from memory
            let result = crate::memory::read_string_from_memory(&store, &memory, result_ptr as u32)?;

            return Ok(result);
        }

        // Try direct function table call
        // WASM function tables allow calling functions by index
        if let Some(table) = instance.get_table(&mut store, "__indirect_function_table") {
            if let Some(func_ref) = table.get(&mut store, handler_index as u64) {
                if let Some(func) = func_ref.unwrap_func() {
                    // Try to call as a function returning i32
                    let result_ptr = func
                        .typed::<(), i32>(&store)
                        .map_err(|e| RuntimeError::wasm(format!("Invalid handler signature: {}", e)))?
                        .call(&mut store, ())
                        .map_err(|e| RuntimeError::wasm(format!("Handler call failed: {}", e)))?;

                    let result =
                        crate::memory::read_string_from_memory(&store, &memory, result_ptr as u32)?;

                    return Ok(result);
                }
            }
        }

        // Fallback: try calling a generic handler function with the index
        if let Ok(dispatch) = instance
            .get_typed_func::<i32, i32>(&mut store, "__dispatch_route")
        {
            let result_ptr = dispatch.call(&mut store, handler_index as i32).map_err(|e| {
                RuntimeError::wasm(format!("Dispatch failed: {}", e))
            })?;

            let result = crate::memory::read_string_from_memory(&store, &memory, result_ptr as u32)?;

            return Ok(result);
        }

        Err(RuntimeError::wasm(format!(
            "Could not find or call handler index {}",
            handler_index
        )))
    }

    /// Get the shared router
    pub fn router(&self) -> &SharedRouter {
        &self.router
    }

    /// Get export names (for debugging)
    pub fn export_names(&self) -> Vec<String> {
        self.module
            .exports()
            .map(|e| e.name().to_string())
            .collect()
    }

    /// Check if an export exists
    pub fn has_export(&self, name: &str) -> bool {
        self.module.exports().any(|e| e.name() == name)
    }
}

/// Shared WASM instance wrapped in Arc
pub type SharedWasmInstance = Arc<WasmInstance>;

/// Create a shared WASM instance
pub fn create_shared_instance(wasm_path: &Path, router: SharedRouter) -> RuntimeResult<SharedWasmInstance> {
    let instance = WasmInstance::load(wasm_path, router)?;
    Ok(Arc::new(instance))
}

/// Create a shared WASM instance with a database bridge
pub fn create_shared_instance_with_db(
    wasm_path: &Path,
    router: SharedRouter,
    db_bridge: SharedDbBridge,
) -> RuntimeResult<SharedWasmInstance> {
    let instance = WasmInstance::load_with_db(wasm_path, router, db_bridge)?;
    Ok(Arc::new(instance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::create_shared_router;

    #[test]
    fn test_wasm_state() {
        let router = create_shared_router();
        let mut state = WasmState::new(router);

        assert!(state.request_context.is_none());

        let request = RequestContext {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: vec![],
            body: String::new(),
            params: std::collections::HashMap::new(),
            query: std::collections::HashMap::new(),
        };

        state.set_request(request);
        assert!(state.request_context.is_some());

        state.clear_request();
        assert!(state.request_context.is_none());
    }

    #[test]
    fn test_request_context() {
        let mut params = std::collections::HashMap::new();
        params.insert("id".to_string(), "123".to_string());

        let mut query = std::collections::HashMap::new();
        query.insert("page".to_string(), "1".to_string());

        let request = RequestContext {
            method: "GET".to_string(),
            path: "/users/123".to_string(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: String::new(),
            params,
            query,
        };

        assert_eq!(request.method, "GET");
        assert_eq!(request.params.get("id"), Some(&"123".to_string()));
        assert_eq!(request.query.get("page"), Some(&"1".to_string()));
    }
}
