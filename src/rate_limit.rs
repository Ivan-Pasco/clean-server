//! Fixed-window rate limiter and axum middleware.
//!
//! Configured by `_rate_limit_configure` during WASM init. Strategy is either
//! `ip` (key by `X-Forwarded-For`/`X-Real-IP`, falling back to a constant) or
//! `user` (key by session cookie, falling back to ip).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::Response,
};
use parking_lot::Mutex;

use crate::runtime_config::{RateLimitConfig, RateLimitStrategy};

#[derive(Debug)]
struct BucketState {
    remaining: u32,
    window_start: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<String, BucketState>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    /// Consume one token for `key`. Returns `true` when the request is allowed.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let window = self.config.window_secs as u64;
        let mut buckets = self.buckets.lock();
        let entry = buckets.entry(key.to_string()).or_insert(BucketState {
            remaining: self.config.per_window,
            window_start: now,
        });
        if now.duration_since(entry.window_start).as_secs() >= window {
            entry.remaining = self.config.per_window;
            entry.window_start = now;
        }
        if entry.remaining == 0 {
            return false;
        }
        entry.remaining -= 1;
        true
    }
}

pub type SharedRateLimiter = Arc<RateLimiter>;

/// Derive the rate-limit key from request headers using the configured strategy.
fn extract_key(strategy: RateLimitStrategy, headers: &HeaderMap) -> String {
    fn header(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn session_from_cookie(headers: &HeaderMap) -> Option<String> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        for part in cookie_header.split(';') {
            let part = part.trim();
            for name in ["session", "sid"] {
                let prefix = format!("{}=", name);
                if let Some(value) = part.strip_prefix(&prefix)
                    && !value.is_empty()
                {
                    return Some(format!("user:{}", value));
                }
            }
        }
        None
    }

    let ip_key = || {
        let ip = header(headers, "x-forwarded-for")
            .or_else(|| header(headers, "x-real-ip"))
            .unwrap_or_else(|| "unknown".to_string());
        format!("ip:{}", ip)
    };

    match strategy {
        RateLimitStrategy::Ip => ip_key(),
        RateLimitStrategy::User => session_from_cookie(headers).unwrap_or_else(ip_key),
    }
}

/// axum middleware that enforces the rate limit configured in `SharedRateLimiter`.
pub async fn rate_limit_middleware(
    State(limiter): State<SharedRateLimiter>,
    req: Request,
    next: Next,
) -> Response {
    let key = extract_key(limiter.config().strategy, req.headers());
    if limiter.allow(&key) {
        return next.run(req).await;
    }
    let retry_after = limiter.config().window_secs.to_string();
    let body = format!(
        r#"{{"ok":false,"error":"rate_limit_exceeded","retry_after":{}}}"#,
        limiter.config().window_secs
    );
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(header::CONTENT_TYPE, "application/json")
        .header("Retry-After", retry_after)
        .body(Body::from(body))
        .expect("rate-limit response builder")
}
