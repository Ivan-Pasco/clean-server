//! Session Storage for Clean Server
//!
//! Provides in-memory session storage with support for:
//! - Session creation with claims
//! - Session retrieval and validation
//! - Session deletion (logout)
//! - Automatic expiration
//!
//! Future: Redis/database-backed sessions for horizontal scaling

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info};
use uuid::Uuid;

/// Session data stored in the session store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    /// Session ID
    pub session_id: String,
    /// User ID (Clean Language integer = i32)
    pub user_id: i32,
    /// User role
    pub role: String,
    /// Additional claims (JSON string)
    pub claims: String,
    /// Last accessed timestamp
    #[serde(skip)]
    last_accessed: Option<Instant>,
}

impl SessionData {
    pub fn new(user_id: i32, role: String, claims: String) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            user_id,
            role,
            claims,
            last_accessed: Some(Instant::now()),
        }
    }

    /// Check if session is expired
    pub fn is_expired(&self, timeout: Duration) -> bool {
        if let Some(last_accessed) = self.last_accessed {
            last_accessed.elapsed() > timeout
        } else {
            true
        }
    }

    /// Update last accessed time
    pub fn touch(&mut self) {
        self.last_accessed = Some(Instant::now());
    }
}

/// Session store configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Session timeout in seconds (default: 3600 = 1 hour)
    pub timeout_seconds: u64,
    /// Cookie name for session ID
    pub cookie_name: String,
    /// Cookie path
    pub cookie_path: String,
    /// Cookie SameSite attribute
    pub same_site: String,
    /// Cookie secure flag
    pub secure: bool,
    /// Cookie httpOnly flag
    pub http_only: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 3600,
            cookie_name: "session".to_string(),
            cookie_path: "/".to_string(),
            same_site: "Lax".to_string(),
            secure: true,
            http_only: true,
        }
    }
}

/// In-memory session store
pub struct SessionStore {
    /// Sessions indexed by session ID (typed sessions for auth flow)
    sessions: HashMap<String, SessionData>,
    /// Raw key-value session storage (for plugin API: _session_store/_session_get)
    raw_data: HashMap<String, (String, Instant)>,
    /// CSRF tokens indexed by session ID
    csrf_tokens: HashMap<String, String>,
    /// Consumed JWT ids (jti) with the instant at which they expire from the
    /// revocation list. Used by `_jwt_refresh_and_rotate` to enforce single-use
    /// refresh tokens (spec AUTH-J007, AUTH-J009).
    consumed_jti: HashMap<String, Instant>,
    /// Active password reset tokens, keyed by the *hex sha256* of the plaintext
    /// token so a store dump does not leak usable tokens. Value is
    /// `(user_id, expires_at)`. Consumed atomically via `consume_reset_token()`
    /// under the SessionStore write lock.
    password_resets: HashMap<String, (i32, Instant)>,
    /// Configuration
    config: SessionConfig,
}

impl SessionStore {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            sessions: HashMap::new(),
            raw_data: HashMap::new(),
            csrf_tokens: HashMap::new(),
            consumed_jti: HashMap::new(),
            password_resets: HashMap::new(),
            config,
        }
    }

    /// Create a new session
    pub fn create(&mut self, user_id: i32, role: &str, claims: &str) -> SessionData {
        let session = SessionData::new(user_id, role.to_string(), claims.to_string());
        let session_id = session.session_id.clone();

        info!("Creating session {} for user {}", session_id, user_id);
        self.sessions.insert(session_id, session.clone());

        session
    }

    /// Get a session by ID
    pub fn get(&mut self, session_id: &str) -> Option<SessionData> {
        let timeout = Duration::from_secs(self.config.timeout_seconds);

        if let Some(session) = self.sessions.get_mut(session_id) {
            if session.is_expired(timeout) {
                debug!("Session {} has expired, removing", session_id);
                self.sessions.remove(session_id);
                return None;
            }
            session.touch();
            return Some(session.clone());
        }
        None
    }

    /// Delete a session
    pub fn delete(&mut self, session_id: &str) -> bool {
        info!("Deleting session {}", session_id);
        self.sessions.remove(session_id).is_some()
    }

    /// Get session configuration
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Format a Set-Cookie header for the session
    pub fn format_cookie(&self, session_id: &str) -> String {
        let mut cookie = format!(
            "{}={}; Path={}",
            self.config.cookie_name, session_id, self.config.cookie_path
        );

        if self.config.http_only {
            cookie.push_str("; HttpOnly");
        }
        if self.config.secure {
            cookie.push_str("; Secure");
        }
        cookie.push_str(&format!("; SameSite={}", self.config.same_site));

        // Add max-age
        cookie.push_str(&format!("; Max-Age={}", self.config.timeout_seconds));

        cookie
    }

    /// Format a cookie header that clears the session
    pub fn format_clear_cookie(&self) -> String {
        format!(
            "{}=; Path={}; Max-Age=0; HttpOnly",
            self.config.cookie_name, self.config.cookie_path
        )
    }

    // =========================================
    // RAW KEY-VALUE SESSION STORAGE
    // =========================================

    /// Store raw data by session ID (for plugin API)
    pub fn store_raw(&mut self, session_id: &str, data: &str) -> bool {
        debug!("Storing raw session data for {}", session_id);
        self.raw_data
            .insert(session_id.to_string(), (data.to_string(), Instant::now()));
        true
    }

    /// Get raw session data by ID (with expiration check)
    pub fn get_raw(&mut self, session_id: &str) -> Option<String> {
        let timeout = Duration::from_secs(self.config.timeout_seconds);

        if let Some((data, last_accessed)) = self.raw_data.get(session_id) {
            if last_accessed.elapsed() > timeout {
                debug!("Raw session {} expired, removing", session_id);
                self.raw_data.remove(session_id);
                return None;
            }
            let result = data.clone();
            // Touch - update timestamp
            if let Some(entry) = self.raw_data.get_mut(session_id) {
                entry.1 = Instant::now();
            }
            return Some(result);
        }

        // Also check typed sessions for backward compatibility
        if let Some(session) = self.sessions.get_mut(session_id) {
            if session.is_expired(timeout) {
                self.sessions.remove(session_id);
                return None;
            }
            session.touch();
            let json = serde_json::json!({
                "userId": session.user_id,
                "role": session.role,
                "sessionId": session.session_id,
                "claims": session.claims
            })
            .to_string();
            return Some(json);
        }

        None
    }

    /// Delete raw session data by ID
    pub fn delete_raw(&mut self, session_id: &str) -> bool {
        let raw_removed = self.raw_data.remove(session_id).is_some();
        let typed_removed = self.sessions.remove(session_id).is_some();
        // Also clean up CSRF token for this session
        self.csrf_tokens.remove(session_id);
        raw_removed || typed_removed
    }

    /// Check if a session exists (raw or typed)
    pub fn exists_raw(&self, session_id: &str) -> bool {
        self.raw_data.contains_key(session_id) || self.sessions.contains_key(session_id)
    }

    // =========================================
    // CSRF TOKEN MANAGEMENT
    // =========================================

    /// Set CSRF token for a session
    pub fn set_csrf(&mut self, session_id: &str, token: &str) {
        debug!("Setting CSRF token for session {}", session_id);
        self.csrf_tokens
            .insert(session_id.to_string(), token.to_string());
    }

    /// Get CSRF token for a session
    pub fn get_csrf(&self, session_id: &str) -> Option<String> {
        self.csrf_tokens.get(session_id).cloned()
    }

    /// Mark a JWT `jti` as consumed. Returns `false` if the `jti` was already
    /// present (i.e. a replay attempt); returns `true` on first consumption.
    ///
    /// `ttl` is how long the record should live in the revocation list — should
    /// match the original token's remaining lifetime so the entry is still
    /// blocking replays right up to natural expiry.
    pub fn mark_jti_consumed(&mut self, jti: &str, ttl: Duration) -> bool {
        // Opportunistically drop any expired entries so this map does not grow
        // unbounded when tokens are refreshed frequently.
        let now = Instant::now();
        self.consumed_jti.retain(|_, expires_at| *expires_at > now);

        if self.consumed_jti.contains_key(jti) {
            return false;
        }
        self.consumed_jti.insert(jti.to_string(), now + ttl);
        true
    }

    /// Returns true if `jti` has been consumed (and is still within its TTL).
    pub fn is_jti_consumed(&self, jti: &str) -> bool {
        match self.consumed_jti.get(jti) {
            Some(expires_at) => *expires_at > Instant::now(),
            None => false,
        }
    }

    /// Store a password reset token. Callers pass the SHA-256 hex digest of
    /// the plaintext token — the plaintext is never held in the store so a
    /// memory dump does not leak usable tokens. `ttl` is measured from now.
    ///
    /// Returns `true` if the row was inserted, `false` if a row with the same
    /// hash was already present (collision on cryptographic random is
    /// effectively impossible; this only happens if the caller supplies a
    /// pre-existing hash).
    pub fn store_reset_token(&mut self, token_hash: &str, user_id: i32, ttl: Duration) -> bool {
        let now = Instant::now();
        // Opportunistic cleanup keeps this map from growing unbounded when
        // users churn reset requests without following through.
        self.password_resets
            .retain(|_, (_, expires_at)| *expires_at > now);

        if self.password_resets.contains_key(token_hash) {
            return false;
        }
        self.password_resets
            .insert(token_hash.to_string(), (user_id, now + ttl));
        true
    }

    /// Atomically validate and consume a password reset token. If the hash is
    /// present AND not expired, the row is removed and the associated user_id
    /// is returned. Returns `None` on invalid, expired, or already-consumed.
    ///
    /// Atomicity comes from the fact that both the check and the remove
    /// happen while the caller holds the `SessionStore` write lock — a second
    /// caller attempting the same hash will observe the row already gone.
    pub fn consume_reset_token(&mut self, token_hash: &str) -> Option<i32> {
        let now = Instant::now();
        match self.password_resets.remove(token_hash) {
            Some((user_id, expires_at)) if expires_at > now => Some(user_id),
            _ => None,
        }
    }

    /// Cleanup expired sessions (call periodically)
    pub fn cleanup_expired(&mut self) -> usize {
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        let before = self.sessions.len() + self.raw_data.len();
        let now = Instant::now();

        self.sessions
            .retain(|_, session| !session.is_expired(timeout));
        self.raw_data
            .retain(|_, (_, last_accessed)| last_accessed.elapsed() <= timeout);
        self.consumed_jti.retain(|_, expires_at| *expires_at > now);
        self.password_resets
            .retain(|_, (_, expires_at)| *expires_at > now);

        let after = self.sessions.len() + self.raw_data.len();
        let removed = before - after;
        if removed > 0 {
            info!("Cleaned up {} expired sessions", removed);
        }
        removed
    }

    /// Get count of active sessions
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Check if store is empty
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Shared session store type
pub type SharedSessionStore = Arc<RwLock<SessionStore>>;

/// Create a shared session store
pub fn create_session_store(config: SessionConfig) -> SharedSessionStore {
    Arc::new(RwLock::new(SessionStore::new(config)))
}

/// Parse cookies from a Cookie header value
/// Returns a HashMap of cookie name -> value
pub fn parse_cookies(cookie_header: &str) -> HashMap<String, String> {
    let mut cookies = HashMap::new();

    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(eq_pos) = part.find('=') {
            let name = part[..eq_pos].trim();
            let value = part[eq_pos + 1..].trim();
            // Remove surrounding quotes if present
            let value = value.trim_matches('"');
            cookies.insert(name.to_string(), value.to_string());
        }
    }

    cookies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation() {
        let config = SessionConfig::default();
        let mut store = SessionStore::new(config);

        let session = store.create(1, "user", r#"{"email":"test@example.com"}"#);
        assert_eq!(session.user_id, 1);
        assert_eq!(session.role, "user");
        assert!(!session.session_id.is_empty());
    }

    #[test]
    fn test_session_get() {
        let config = SessionConfig::default();
        let mut store = SessionStore::new(config);

        let session = store.create(1, "admin", "{}");
        let session_id = session.session_id.clone();

        let retrieved = store.get(&session_id);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().user_id, 1);
    }

    #[test]
    fn test_session_delete() {
        let config = SessionConfig::default();
        let mut store = SessionStore::new(config);

        let session = store.create(1, "user", "{}");
        let session_id = session.session_id.clone();

        assert!(store.delete(&session_id));
        assert!(store.get(&session_id).is_none());
    }

    #[test]
    fn test_session_expiry() {
        let config = SessionConfig {
            timeout_seconds: 0, // Immediate expiry
            ..SessionConfig::default()
        };
        let mut store = SessionStore::new(config);

        let session = store.create(1, "user", "{}");
        let session_id = session.session_id.clone();

        // Session should be expired immediately
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(store.get(&session_id).is_none());
    }

    #[test]
    fn test_parse_cookies() {
        let cookies = parse_cookies("session=abc123; theme=dark; lang=en");

        assert_eq!(cookies.get("session"), Some(&"abc123".to_string()));
        assert_eq!(cookies.get("theme"), Some(&"dark".to_string()));
        assert_eq!(cookies.get("lang"), Some(&"en".to_string()));
    }

    #[test]
    fn test_jti_first_consumption_succeeds_replay_fails() {
        let mut store = SessionStore::new(SessionConfig::default());
        let ttl = Duration::from_secs(60);

        assert!(store.mark_jti_consumed("jti-abc", ttl));
        assert!(store.is_jti_consumed("jti-abc"));
        // Second call with the same jti must return false — this is the replay
        // guard the spec (AUTH-J007, AUTH-J009) requires.
        assert!(!store.mark_jti_consumed("jti-abc", ttl));
    }

    #[test]
    fn test_jti_expires_after_ttl() {
        let mut store = SessionStore::new(SessionConfig::default());
        // Zero-duration entry is considered expired on the next `Instant::now()`.
        assert!(store.mark_jti_consumed("jti-short", Duration::from_millis(0)));
        std::thread::sleep(Duration::from_millis(5));
        // Expired entries are treated as not-consumed so the caller can reissue.
        assert!(!store.is_jti_consumed("jti-short"));
    }

    #[test]
    fn test_reset_token_store_and_consume() {
        let mut store = SessionStore::new(SessionConfig::default());
        let ttl = Duration::from_secs(900);
        // Not-a-real-hash, but store treats it as an opaque key.
        let hash = "0123456789abcdef".repeat(4);

        assert!(store.store_reset_token(&hash, 42, ttl));
        // First consume returns the user id; second consume must return None
        // — this is the atomic single-use guarantee frame.auth relies on.
        assert_eq!(store.consume_reset_token(&hash), Some(42));
        assert_eq!(store.consume_reset_token(&hash), None);
    }

    #[test]
    fn test_reset_token_expired_returns_none() {
        let mut store = SessionStore::new(SessionConfig::default());
        let hash = "abababab".repeat(8);
        // TTL of 0 -> row is expired the moment the next Instant::now() fires.
        assert!(store.store_reset_token(&hash, 7, Duration::from_millis(0)));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(store.consume_reset_token(&hash), None);
    }

    #[test]
    fn test_format_cookie() {
        let config = SessionConfig {
            cookie_name: "mysession".to_string(),
            cookie_path: "/app".to_string(),
            same_site: "Strict".to_string(),
            secure: true,
            http_only: true,
            timeout_seconds: 3600,
        };
        let store = SessionStore::new(config);

        let cookie = store.format_cookie("test123");
        assert!(cookie.contains("mysession=test123"));
        assert!(cookie.contains("Path=/app"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Max-Age=3600"));
    }
}
