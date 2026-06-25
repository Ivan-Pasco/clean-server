//! Runtime configuration populated by `server:` block bridge calls during WASM init.
//!
//! Values written here by `_http_listen_on`, `_cors_configure`,
//! `_rate_limit_configure`, and `_http_set_global_error_handler` outlive the
//! transient `WasmState` used for initialization and are read by `start_server`
//! when building the axum router. Per-request `WasmState`s receive an `Arc`
//! clone so the same handle is shared everywhere.

use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    pub listen_host: Option<String>,
    pub listen_port: Option<u16>,
    pub cors: Option<CorsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub global_error_handler: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub max_age_secs: u32,
    pub allow_credentials: bool,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub per_window: u32,
    pub window_secs: u32,
    pub strategy: RateLimitStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitStrategy {
    Ip,
    User,
}

impl RateLimitStrategy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Self::User,
            _ => Self::Ip,
        }
    }
}

pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;

pub fn create_shared_runtime_config() -> SharedRuntimeConfig {
    Arc::new(RwLock::new(RuntimeConfig::default()))
}

/// Split a comma-separated bridge argument into trimmed, non-empty entries.
pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}
