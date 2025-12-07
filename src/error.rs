//! Error types for frame-runtime
//!
//! Provides structured error handling across all runtime components.

use thiserror::Error;

/// Main error type for the Frame runtime
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error loading or instantiating WASM module
    #[error("WASM error: {message}")]
    Wasm {
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    /// Error in HTTP server operations
    #[error("Server error: {message}")]
    Server {
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    /// Error in route handling
    #[error("Route error: {message}")]
    Route { message: String },

    /// Error in memory operations
    #[error("Memory error: {message}")]
    Memory { message: String },

    /// Error in Host Bridge operations
    #[error("Bridge error: {namespace}::{function}: {message}")]
    Bridge {
        namespace: String,
        function: String,
        message: String,
    },

    /// Configuration error
    #[error("Configuration error: {message}")]
    Config { message: String },

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic error with context
    #[error("{context}: {message}")]
    WithContext { context: String, message: String },
}

impl RuntimeError {
    /// Create a WASM error
    pub fn wasm(message: impl Into<String>) -> Self {
        Self::Wasm {
            message: message.into(),
            source: None,
        }
    }

    /// Create a WASM error with source
    pub fn wasm_with_source(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self::Wasm {
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a server error
    pub fn server(message: impl Into<String>) -> Self {
        Self::Server {
            message: message.into(),
            source: None,
        }
    }

    /// Create a server error with source
    pub fn server_with_source(message: impl Into<String>, source: anyhow::Error) -> Self {
        Self::Server {
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a route error
    pub fn route(message: impl Into<String>) -> Self {
        Self::Route {
            message: message.into(),
        }
    }

    /// Create a memory error
    pub fn memory(message: impl Into<String>) -> Self {
        Self::Memory {
            message: message.into(),
        }
    }

    /// Create a bridge error
    pub fn bridge(
        namespace: impl Into<String>,
        function: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Bridge {
            namespace: namespace.into(),
            function: function.into(),
            message: message.into(),
        }
    }

    /// Create a configuration error
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// Add context to an error
    pub fn with_context(self, context: impl Into<String>) -> Self {
        Self::WithContext {
            context: context.into(),
            message: self.to_string(),
        }
    }
}

/// Result type alias for runtime operations
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// HTTP response error that can be returned from handlers
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl HttpError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }

    /// Convert to JSON response body
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "ok": false,
            "error": {
                "code": self.status,
                "message": self.message
            }
        });

        if let Some(details) = &self.details {
            obj["error"]["details"] = details.clone();
        }

        obj
    }
}

impl From<RuntimeError> for HttpError {
    fn from(err: RuntimeError) -> Self {
        match err {
            RuntimeError::Route { message } => HttpError::not_found(message),
            RuntimeError::Memory { message } => HttpError::internal_error(message),
            RuntimeError::Wasm { message, .. } => HttpError::internal_error(message),
            RuntimeError::Server { message, .. } => HttpError::internal_error(message),
            RuntimeError::Bridge { message, .. } => HttpError::internal_error(message),
            RuntimeError::Config { message } => HttpError::internal_error(message),
            RuntimeError::Io(e) => HttpError::internal_error(e.to_string()),
            RuntimeError::WithContext { context, message } => {
                HttpError::internal_error(format!("{}: {}", context, message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_messages() {
        let err = RuntimeError::wasm("Failed to load module");
        assert!(err.to_string().contains("Failed to load module"));

        let err = RuntimeError::route("No handler for /api/users");
        assert!(err.to_string().contains("/api/users"));

        let err = RuntimeError::bridge("db", "query", "Connection refused");
        assert!(err.to_string().contains("db::query"));
    }

    #[test]
    fn test_http_error_json() {
        let err = HttpError::not_found("User not found");
        let json = err.to_json();

        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], 404);
        assert_eq!(json["error"]["message"], "User not found");
    }

    #[test]
    fn test_runtime_to_http_error() {
        let runtime_err = RuntimeError::route("Not found");
        let http_err: HttpError = runtime_err.into();
        assert_eq!(http_err.status, 404);
    }
}
