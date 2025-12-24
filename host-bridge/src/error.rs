//! Error types for host-bridge
//!
//! Provides error types used throughout the host-bridge crate.

use thiserror::Error;

/// Bridge error type
#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("WASM error: {0}")]
    Wasm(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Memory error: {0}")]
    Memory(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Wasmtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),
}

/// Result type for bridge operations
pub type BridgeResult<T> = Result<T, BridgeError>;
