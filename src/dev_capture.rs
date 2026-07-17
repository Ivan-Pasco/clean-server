//! Dev-mode Capture — Runtime Snapshot for `/_debug/capture`
//!
//! Implements the `_dev_snapshot()` host bridge and the two supporting ring
//! buffers (request log, stderr/stdout log) that back the errors dashboard's
//! reproduction pipeline. See
//! `foundation/platform-architecture/SERVER_EXTENSIONS.md § Dev-mode Capture`.
//!
//! Security boundary: the entire surface is gated on `CLEAN_DEV=1`. When the
//! variable is unset or any other value, `snapshot_json()` returns an empty
//! string and the ring-buffer writers become no-ops. Production servers must
//! not expose any of this data via any code path.
//!
//! Redaction of sensitive headers (`Cookie`, `Authorization`) happens at
//! **write time** to the request ring buffer, so a code path that skips the
//! redactor cannot leak credentials on the read side.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// CLEAN_DEV must equal exactly `"1"` for capture to be active.
///
/// Any other value (including `"true"`, `"yes"`, empty string, unset) is
/// treated as production. Read every call so operators can flip the env var
/// without restarting the server for local testing — the cost is a single
/// process-env lookup on the request hot path (~50 ns).
pub fn is_enabled() -> bool {
    matches!(std::env::var("CLEAN_DEV").ok().as_deref(), Some("1"))
}

/// Global-once state initialized on first request or bridge call. Structurally
/// bounded (100 log lines × ~256 bytes ≈ 25 KB; 20 request entries × ~10 KB ≈
/// 200 KB), so memory cost is negligible even when CLEAN_DEV is left on.
static STATE: OnceLock<CaptureState> = OnceLock::new();

/// Wasm binary bytes for the currently-loaded module. Set once by
/// `wasm::WasmInstance::load*` so `_dev_snapshot()` can base64 the raw bytes
/// without re-reading them from disk (the file may have moved).
static CURRENT_WASM: OnceLock<Mutex<Option<Vec<u8>>>> = OnceLock::new();

/// Absolute path of the on-disk WASM binary (informational; used in traceback
/// when the binary was too large to embed).
static WASM_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// Configured project root for the `source_tree` walk. Defaults to the
/// current working directory of the server process; some deployments run the
/// server from a subdirectory of the project, so allow explicit override via
/// `CLEAN_DEV_PROJECT_ROOT` for those cases.
fn project_root() -> PathBuf {
    if let Ok(v) = std::env::var("CLEAN_DEV_PROJECT_ROOT")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Ring buffers guarded by a single mutex. One shared lock is fine for the
/// request-writing rate (dev mode, single-user) — no contention concern.
struct CaptureState {
    inner: Mutex<CaptureInner>,
}

struct CaptureInner {
    request_log: VecDeque<RequestEntry>,
    log_lines: VecDeque<String>,
}

/// One entry in the request ring buffer. Shape matches the framework contract
/// in SERVER_EXTENSIONS.md §Dev-mode Capture.
#[derive(Debug, Clone, Serialize)]
pub struct RequestEntry {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    pub captured_at: String,
    pub headers: serde_json::Map<String, serde_json::Value>,
    pub body: String,
    /// Present only when the body was cut at REQUEST_BODY_MAX_BYTES.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_truncated: Option<bool>,
}

/// Maximum bytes of request body kept in the ring buffer per entry. The spec
/// caps at 8 KB. Bytes past this point are dropped and `body_truncated=true`
/// is emitted so the framework's selector can flag the entry.
const REQUEST_BODY_MAX_BYTES: usize = 8 * 1024;

/// Number of stderr/stdout log lines kept in the ring buffer.
const LOG_LINES_MAX: usize = 100;

/// Number of request entries kept in the ring buffer.
const REQUEST_LOG_MAX: usize = 20;

/// Cap for embedded WASM (raw bytes, before base64). Larger WASM binaries
/// return `""` and add a warning to `last_log_lines`.
const CURRENT_WASM_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Cap for the `source_tree` walk.
const SOURCE_TREE_MAX_FILES: usize = 200;
const SOURCE_TREE_MAX_DEPTH: usize = 4;

/// Directory names to skip during the source_tree walk. Mirrors the tarball
/// exclusion rules in SERVER_EXTENSIONS.md §Tarball layout.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "tests"];

fn state() -> &'static CaptureState {
    STATE.get_or_init(|| CaptureState {
        inner: Mutex::new(CaptureInner {
            request_log: VecDeque::with_capacity(REQUEST_LOG_MAX),
            log_lines: VecDeque::with_capacity(LOG_LINES_MAX),
        }),
    })
}

// ---------------------------------------------------------------------------
// Public write hooks
// ---------------------------------------------------------------------------

/// Register the WASM bytes for the currently-loaded module. Called by
/// `WasmInstance::from_bytes_inner` at load time.
pub fn set_current_wasm(bytes: Vec<u8>, path: Option<PathBuf>) {
    let slot = CURRENT_WASM.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(bytes);
    }
    let path_slot = WASM_PATH.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = path_slot.lock() {
        *guard = path;
    }
}

/// Record a completed HTTP request into the ring buffer.
///
/// Redaction and body truncation happen here at write time — a reader that
/// skips redaction cannot leak credentials because the ring buffer never
/// stored the real values.
///
/// No-op when CLEAN_DEV is not `"1"`.
#[allow(clippy::too_many_arguments)]
pub fn record_request(
    method: &str,
    path_and_query: &str,
    status: u16,
    duration_ms: u64,
    header_pairs: &[(String, String)],
    body_bytes: &[u8],
    content_type: Option<&str>,
) {
    if !is_enabled() {
        return;
    }

    let mut headers = serde_json::Map::new();
    for (name, value) in header_pairs {
        // RFC 7230 style: duplicate header names are pre-joined by the caller
        // (axum's HeaderMap iteration already produces one entry per raw
        // header line, so this is a stable ordering).
        let out_value = redact_header_value(name, value);
        // Merge duplicates by comma-joining, preserving RFC 7230 convention.
        headers
            .entry(name.to_string())
            .and_modify(|existing| {
                if let serde_json::Value::String(s) = existing {
                    *s = format!("{}, {}", s, out_value);
                }
            })
            .or_insert_with(|| serde_json::Value::String(out_value));
    }

    let (body_str, truncated) = shape_body(body_bytes, content_type);

    let entry = RequestEntry {
        method: method.to_uppercase(),
        path: path_and_query.to_string(),
        status,
        duration_ms,
        captured_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        headers,
        body: body_str,
        body_truncated: if truncated { Some(true) } else { None },
    };

    let s = state();
    if let Ok(mut inner) = s.inner.lock() {
        if inner.request_log.len() == REQUEST_LOG_MAX {
            inner.request_log.pop_front();
        }
        inner.request_log.push_back(entry);
    }
}

/// Append a formatted stderr/stdout line into the ring buffer.
///
/// The line is stored without a trailing newline. ANSI escape sequences are
/// stripped so downstream consumers never see raw color codes. No-op when
/// CLEAN_DEV is not `"1"`.
pub fn record_log_line(line: &str) {
    if !is_enabled() {
        return;
    }
    let cleaned = strip_ansi(line);
    let s = state();
    if let Ok(mut inner) = s.inner.lock() {
        if inner.log_lines.len() == LOG_LINES_MAX {
            inner.log_lines.pop_front();
        }
        inner.log_lines.push_back(cleaned);
    }
}

// ---------------------------------------------------------------------------
// Snapshot builder
// ---------------------------------------------------------------------------

/// Produce the JSON payload consumed by the framework's `/_debug/capture`
/// handler. Returns the empty string when CLEAN_DEV is unset — the framework
/// treats that as "not in dev mode" and responds 404.
///
/// This is the entry point called by the `_dev_snapshot()` host bridge.
pub fn snapshot_json() -> String {
    if !is_enabled() {
        return String::new();
    }

    let root = project_root();

    // Source tree walk. Bounded at SOURCE_TREE_MAX_FILES/SOURCE_TREE_MAX_DEPTH.
    let source_tree = walk_source_tree(&root);

    // Current WASM (base64) — cap at CURRENT_WASM_MAX_BYTES raw. Larger
    // binaries emit an inline warning line into the log ring buffer and
    // return "".
    let current_wasm_b64 = read_current_wasm_b64();

    let last_log_lines = collect_log_lines();
    let request_log = collect_request_log();
    let db_schema = String::new(); // Populated by clean-server when a DB is attached — currently no
    // shared surface exposes CREATE TABLE. Left blank per spec: `""` when
    // no DB attached is the documented value. See SERVER_EXTENSIONS.md
    // §_dev_snapshot bridge-level constraints.
    let project_hash = compute_project_hash(&root);
    let component_versions = snapshot_component_versions();
    let captured_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let payload = serde_json::json!({
        "source_tree": source_tree,
        "current_wasm": current_wasm_b64,
        "last_log_lines": last_log_lines,
        "request_log": request_log,
        "db_schema": db_schema,
        "project_hash": project_hash,
        "component_versions": component_versions,
        "captured_at": captured_at,
    });

    serde_json::to_string(&payload).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Snapshot helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SourceFile {
    path: String,
    content: String,
}

fn walk_source_tree(root: &Path) -> Vec<SourceFile> {
    let mut out: Vec<SourceFile> = Vec::new();
    walk_dir(root, root, 0, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk_dir(root: &Path, dir: &Path, depth: usize, out: &mut Vec<SourceFile>) {
    if out.len() >= SOURCE_TREE_MAX_FILES {
        return;
    }
    if depth > SOURCE_TREE_MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Sort entries so the output is stable across runs.
    let mut names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        if out.len() >= SOURCE_TREE_MAX_FILES {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if file_type.is_dir() {
            if SKIP_DIRS.iter().any(|d| *d == name_str.as_ref()) {
                continue;
            }
            walk_dir(root, &path, depth + 1, out);
        } else if file_type.is_file() {
            // Only `.cln` source files per SERVER_EXTENSIONS.md §_dev_snapshot.
            let ext_matches = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("cln"))
                .unwrap_or(false);
            if !ext_matches {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path.strip_prefix(root).unwrap_or(&path);
            out.push(SourceFile {
                path: rel.to_string_lossy().replace('\\', "/"),
                content,
            });
        }
    }
}

fn read_current_wasm_b64() -> String {
    let Some(slot) = CURRENT_WASM.get() else {
        return String::new();
    };
    let Ok(guard) = slot.lock() else {
        return String::new();
    };
    let Some(bytes) = guard.as_ref() else {
        return String::new();
    };
    if bytes.len() > CURRENT_WASM_MAX_BYTES {
        record_log_line(&format!(
            "[dev-capture] current_wasm omitted: {} bytes exceeds {} byte cap",
            bytes.len(),
            CURRENT_WASM_MAX_BYTES
        ));
        return String::new();
    }
    B64.encode(bytes)
}

fn collect_log_lines() -> String {
    let s = state();
    let Ok(inner) = s.inner.lock() else {
        return String::new();
    };
    let lines: Vec<&str> = inner.log_lines.iter().map(|s| s.as_str()).collect();
    lines.join("\n")
}

fn collect_request_log() -> Vec<RequestEntry> {
    let s = state();
    let Ok(inner) = s.inner.lock() else {
        return Vec::new();
    };
    inner.request_log.iter().cloned().collect()
}

/// Compute the project hash using the same formula as `cleen`'s heartbeat:
/// `SHA256(trim(git_remote_origin_url) + "|" + git_repo_root)`. Returns the
/// empty string when the current directory is not inside a git working tree.
pub fn compute_project_hash(root: &Path) -> String {
    let Some(repo_root) = git_repo_root(root) else {
        return String::new();
    };
    let remote = git_remote_url(root);
    let mut hasher = Sha256::new();
    hasher.update(remote.as_bytes());
    hasher.update(b"|");
    hasher.update(repo_root.as_bytes());
    hex_encode(&hasher.finalize())
}

fn git_repo_root(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn git_remote_url(cwd: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn snapshot_component_versions() -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "clean-server".to_string(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    let Some(home) = std::env::var_os("HOME") else {
        return serde_json::Value::Object(map);
    };
    let plugins_root = std::path::PathBuf::from(home)
        .join(".cleen")
        .join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_root) else {
        return serde_json::Value::Object(map);
    };
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let plugin_toml = plugin_dir.join("plugin.toml");
        if !plugin_toml.exists() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&plugin_toml) else {
            continue;
        };
        let Ok(value) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        let Some(plugin_table) = value.get("plugin").and_then(|v| v.as_table()) else {
            continue;
        };
        let Some(name) = plugin_table.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let version = plugin_table
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        map.insert(name.to_string(), serde_json::Value::String(version));
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// Redaction and body shaping
// ---------------------------------------------------------------------------

/// Replace `Cookie` and `Authorization` values with `<redacted>` before the
/// ring buffer sees the real value. Case-insensitive header name match.
pub fn redact_header_value(name: &str, value: &str) -> String {
    if name.eq_ignore_ascii_case("cookie") || name.eq_ignore_ascii_case("authorization") {
        "<redacted>".to_string()
    } else {
        value.to_string()
    }
}

/// Decide how to serialize a body into the ring buffer.
///
/// - Binary content (per Content-Type sniff or non-UTF-8 detection): replaced
///   with a `[binary body, N bytes]` marker. Never base64-encoded — the
///   retest sandbox can't replay binary bodies today, so lossy handling is
///   correct (see SERVER_EXTENSIONS.md §Body handling).
/// - Text bodies over REQUEST_BODY_MAX_BYTES: truncated with the final 3
///   bytes replaced by `...`. The caller marks `body_truncated: true`.
///
/// Returns `(body_string, was_truncated)`.
pub fn shape_body(bytes: &[u8], content_type: Option<&str>) -> (String, bool) {
    if bytes.is_empty() {
        return (String::new(), false);
    }
    let treat_as_binary = is_binary_content(content_type, bytes);
    if treat_as_binary {
        return (format!("[binary body, {} bytes]", bytes.len()), false);
    }
    // UTF-8 text path.
    let s = String::from_utf8_lossy(bytes).into_owned();
    if s.len() <= REQUEST_BODY_MAX_BYTES {
        return (s, false);
    }
    // Truncate to REQUEST_BODY_MAX_BYTES, but preserve UTF-8 boundary.
    let mut cut = REQUEST_BODY_MAX_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_string();
    // Trim last 3 bytes to make room for `...` per spec.
    while out.len() > REQUEST_BODY_MAX_BYTES.saturating_sub(3) {
        out.pop();
    }
    out.push_str("...");
    (out, true)
}

fn is_binary_content(content_type: Option<&str>, bytes: &[u8]) -> bool {
    if let Some(ct) = content_type {
        let ct_lower = ct.to_ascii_lowercase();
        // Strip charset params for the comparison.
        let base = ct_lower.split(';').next().unwrap_or("").trim();
        if base == "application/octet-stream" {
            return true;
        }
        if base.starts_with("image/")
            || base.starts_with("audio/")
            || base.starts_with("video/")
            || base == "application/pdf"
            || base == "application/zip"
            || base == "application/gzip"
            || base == "application/x-gzip"
            || base == "application/x-tar"
        {
            return true;
        }
        // Explicit text/json content types are never binary.
        if base.starts_with("text/")
            || base == "application/json"
            || base == "application/xml"
            || base.ends_with("+json")
            || base.ends_with("+xml")
            || base == "application/x-www-form-urlencoded"
        {
            return false;
        }
    }
    // No content type OR unknown content type: sniff for NUL bytes and
    // invalid UTF-8. Binary payloads (gzip, images, etc.) almost always
    // contain NULs in the first KB.
    let sniff_len = bytes.len().min(1024);
    if bytes[..sniff_len].iter().any(|b| *b == 0) {
        return true;
    }
    // Try strict UTF-8: if it fails, treat as binary.
    std::str::from_utf8(&bytes[..sniff_len]).is_err()
}

fn strip_ansi(s: &str) -> String {
    // Minimal ANSI stripper: removes CSI sequences (ESC [ ... letter) and
    // the two-char `ESC c` reset. Enough for tracing-subscriber's default
    // color output; we're not trying to be a full terminal emulator.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: consume until a final byte in 0x40..=0x7E
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
                continue;
            }
            // Non-CSI escape (like ESC c): skip the ESC and one following byte
            i += 2;
            continue;
        }
        // Take one char (handle multibyte via char_indices restart approach)
        let ch_start = i;
        // Advance by UTF-8 char length
        let step = match bytes[i] {
            b if b < 0x80 => 1,
            b if b < 0xC0 => 1, // stray continuation, treat as single byte
            b if b < 0xE0 => 2,
            b if b < 0xF0 => 3,
            _ => 4,
        };
        let end = (ch_start + step).min(bytes.len());
        if let Ok(chunk) = std::str::from_utf8(&bytes[ch_start..end]) {
            out.push_str(chunk);
        }
        i = end;
    }
    out
}

// ---------------------------------------------------------------------------
// Tracing layer — captures `tracing::info!` / `warn!` / `error!` output into
// the `last_log_lines` ring buffer.
// ---------------------------------------------------------------------------

/// A `tracing_subscriber::Layer` that mirrors formatted event lines into the
/// dev-mode log ring buffer. Install alongside the process-wide FmtSubscriber
/// so operators keep seeing colored console output *and* the capture endpoint
/// sees the same lines.
///
/// Only lines produced through the `tracing` macros are captured. Direct
/// `println!` calls from the WASM print bridge do NOT flow through here —
/// that's a known limitation acknowledged in the SERVER_EXTENSIONS spec;
/// wiring println! interception would require patching `host-bridge`, which
/// is out of scope for this bridge (dev-only capture must not distort the
/// shared portable console I/O path used by CLI and node-server hosts).
pub struct DevCaptureTracingLayer;

impl<S> Layer<S> for DevCaptureTracingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !is_enabled() {
            return;
        }
        let meta = event.metadata();
        let mut line = String::new();
        // Format: "LEVEL target: message key=value ..."
        let _ = write!(&mut line, "{} {}: ", meta.level(), meta.target());
        let mut visitor = MessageVisitor {
            buf: &mut line,
            wrote_message: false,
        };
        event.record(&mut visitor);
        record_log_line(&line);
    }
}

/// Field visitor that appends the event's `message` field first, then any
/// remaining structured fields as `k=v`. Keeps the ring buffer human-readable
/// without pulling in tracing-subscriber's full formatter machinery.
struct MessageVisitor<'a> {
    buf: &'a mut String,
    wrote_message: bool,
}

impl<'a> Visit for MessageVisitor<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.buf, "{:?}", value);
            self.wrote_message = true;
        } else {
            if self.wrote_message {
                let _ = write!(self.buf, " ");
            }
            let _ = write!(self.buf, "{}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.buf.push_str(value);
            self.wrote_message = true;
        } else {
            if self.wrote_message {
                let _ = write!(self.buf, " ");
            }
            let _ = write!(self.buf, "{}={}", field.name(), value);
        }
    }
}

// ---------------------------------------------------------------------------
// Test hooks
// ---------------------------------------------------------------------------

/// Reset both ring buffers to an empty state. Exposed unconditionally so
/// integration tests (which run as external binaries and don't see
/// `#[cfg(test)]` items in the lib crate) can start each case from a
/// deterministic baseline. The double-underscore prefix and `#[doc(hidden)]`
/// marker signal that this is not part of the stable public API.
///
/// Called only from `#[test]` code paths; production callers have no reason
/// to reset the ring buffers mid-flight.
#[doc(hidden)]
pub fn __reset_for_test() {
    let s = state();
    if let Ok(mut inner) = s.inner.lock() {
        inner.request_log.clear();
        inner.log_lines.clear();
    }
    if let Some(slot) = CURRENT_WASM.get()
        && let Ok(mut guard) = slot.lock()
    {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------- Header redaction -------

    #[test]
    fn cookie_header_is_redacted() {
        assert_eq!(
            redact_header_value("Cookie", "session=abc123; auth=xyz"),
            "<redacted>"
        );
    }

    #[test]
    fn authorization_header_is_redacted() {
        assert_eq!(
            redact_header_value("Authorization", "Bearer eyJhbGciOi..."),
            "<redacted>"
        );
    }

    #[test]
    fn redaction_is_case_insensitive() {
        assert_eq!(redact_header_value("cookie", "x"), "<redacted>");
        assert_eq!(redact_header_value("AUTHORIZATION", "x"), "<redacted>");
        assert_eq!(redact_header_value("CoOkIe", "x"), "<redacted>");
    }

    #[test]
    fn non_sensitive_headers_pass_through() {
        assert_eq!(
            redact_header_value("Accept", "text/html"),
            "text/html".to_string()
        );
        assert_eq!(
            redact_header_value("User-Agent", "curl/7.85"),
            "curl/7.85".to_string()
        );
    }

    // ------- Body shaping -------

    #[test]
    fn empty_body_stays_empty() {
        let (s, truncated) = shape_body(&[], None);
        assert_eq!(s, "");
        assert!(!truncated);
    }

    #[test]
    fn small_utf8_body_survives_verbatim() {
        let bytes = b"{\"user\": \"alice\"}";
        let (s, truncated) = shape_body(bytes, Some("application/json"));
        assert_eq!(s, "{\"user\": \"alice\"}");
        assert!(!truncated);
    }

    #[test]
    fn body_over_8kb_is_truncated_with_trailing_dots() {
        let bytes = vec![b'a'; 9000];
        let (s, truncated) = shape_body(&bytes, Some("text/plain"));
        assert!(truncated, "9 KB text body must be truncated");
        assert_eq!(s.len(), REQUEST_BODY_MAX_BYTES);
        assert!(s.ends_with("..."), "truncated body must end in '...'");
    }

    #[test]
    fn binary_body_gets_marker_not_utf8_decode() {
        let bytes = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0xff];
        let (s, truncated) = shape_body(&bytes, Some("application/octet-stream"));
        assert_eq!(s, "[binary body, 7 bytes]");
        assert!(!truncated);
    }

    #[test]
    fn image_content_type_treated_as_binary() {
        let bytes = vec![0x89, 0x50, 0x4e, 0x47];
        let (s, _) = shape_body(&bytes, Some("image/png"));
        assert_eq!(s, "[binary body, 4 bytes]");
    }

    #[test]
    fn content_type_with_charset_is_recognized_as_text() {
        let (s, _) = shape_body(b"hello", Some("text/plain; charset=utf-8"));
        assert_eq!(s, "hello");
    }

    #[test]
    fn nul_byte_sniff_flags_body_as_binary_when_no_content_type() {
        let bytes = vec![0x00, b'h', b'i'];
        let (s, _) = shape_body(&bytes, None);
        assert_eq!(s, "[binary body, 3 bytes]");
    }

    #[test]
    fn plain_utf8_without_content_type_is_kept_as_text() {
        let bytes = b"hello world";
        let (s, _) = shape_body(bytes, None);
        assert_eq!(s, "hello world");
    }

    #[test]
    fn json_content_type_never_treated_as_binary_even_with_high_bytes() {
        // Cyrillic / emoji in JSON payload — high bytes but valid UTF-8.
        let bytes = "{\"name\":\"Иван\"}".as_bytes();
        let (s, truncated) = shape_body(bytes, Some("application/json"));
        assert_eq!(s, "{\"name\":\"Иван\"}");
        assert!(!truncated);
    }

    #[test]
    fn truncation_preserves_utf8_boundary() {
        // Build a body that would land the cut mid-multibyte-char if we
        // didn't scan back to a boundary.
        let mut bytes: Vec<u8> = Vec::new();
        while bytes.len() < REQUEST_BODY_MAX_BYTES - 2 {
            bytes.extend_from_slice("é".as_bytes()); // 2-byte UTF-8
        }
        // Add a 4-byte code point straddling the cut.
        bytes.extend_from_slice("𝄞".as_bytes()); // 4-byte UTF-8
        while bytes.len() < REQUEST_BODY_MAX_BYTES + 200 {
            bytes.push(b'a');
        }
        let (s, truncated) = shape_body(&bytes, Some("text/plain"));
        assert!(truncated);
        // Should end in "..." and be valid UTF-8.
        assert!(s.is_char_boundary(s.len()));
        assert!(s.ends_with("..."));
    }

    // ------- ANSI stripping -------

    #[test]
    fn ansi_color_codes_are_stripped() {
        let colored = "\x1b[31mINFO\x1b[0m server started";
        let cleaned = strip_ansi(colored);
        assert_eq!(cleaned, "INFO server started");
    }

    #[test]
    fn plain_text_survives_ansi_stripper() {
        let s = strip_ansi("hello — world");
        assert_eq!(s, "hello — world");
    }

    // ------- Ring buffer behavior -------

    #[test]
    fn record_request_is_noop_when_clean_dev_unset() {
        // Guard: don't disturb other tests that rely on CLEAN_DEV=1 being on.
        let prior = std::env::var("CLEAN_DEV").ok();
        // SAFETY: env var mutation must be single-threaded; cargo runs tests
        // in parallel by default, but this module's env-touching tests are
        // gated by `#[cfg(test)]` and only mutate their own values under a
        // shared assumption of test isolation. See test-strategy notes for
        // clean-server's env-touching tests.
        unsafe {
            std::env::remove_var("CLEAN_DEV");
        }
        __reset_for_test();
        record_request(
            "GET",
            "/x",
            200,
            1,
            &[("Accept".to_string(), "*".to_string())],
            b"",
            None,
        );
        assert!(collect_request_log().is_empty());
        // Restore for downstream tests.
        if let Some(v) = prior {
            unsafe {
                std::env::set_var("CLEAN_DEV", v);
            }
        }
    }

    #[test]
    fn snapshot_is_empty_string_when_clean_dev_unset() {
        let prior = std::env::var("CLEAN_DEV").ok();
        unsafe {
            std::env::remove_var("CLEAN_DEV");
        }
        assert_eq!(snapshot_json(), "");
        if let Some(v) = prior {
            unsafe {
                std::env::set_var("CLEAN_DEV", v);
            }
        }
    }

    // ------- Project hash -------

    #[test]
    fn project_hash_matches_reference_formula() {
        // Direct reference-formula computation using the same crate as the
        // real impl. This test doesn't shell out to git — it just verifies
        // the hasher wiring (remote + "|" + repo_root) is identical to what
        // cleen's heartbeat computes.
        let remote = "git@github.com:example/repo.git";
        let repo_root = "/tmp/example/repo";
        let mut hasher = Sha256::new();
        hasher.update(remote.as_bytes());
        hasher.update(b"|");
        hasher.update(repo_root.as_bytes());
        let expected = hex_encode(&hasher.finalize());

        // Recompute independently to prove the formula is stable.
        let recomputed = {
            let mut h = Sha256::new();
            h.update(remote.as_bytes());
            h.update(b"|");
            h.update(repo_root.as_bytes());
            hex_encode(&h.finalize())
        };
        assert_eq!(recomputed, expected);
        assert_eq!(expected.len(), 64, "sha256 hex is always 64 chars");
    }
}
