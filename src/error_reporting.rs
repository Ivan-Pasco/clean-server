//! Diagnostic reporting for `RUNTIME_WASM_PARSE` failures.
//!
//! When `wasmtime::Module::new` rejects the WASM bytes produced by the
//! compiler, the short error string alone is not enough to reproduce or
//! bisect the underlying bug. This module assembles a structured
//! diagnostic bundle at the crash site so the compiler team has the full
//! picture.
//!
//! ## What gets captured
//!
//! - Full untruncated wasmtime error (offset + reason)
//! - SHA-256 fingerprint of the WASM bytes (used for dedupe)
//! - Byte length and first 256 bytes as hex (module header)
//! - Second-opinion validation from `wasmparser` (separates encoder bugs
//!   from validator-vs-runtime mismatches)
//! - Compiler version from the optional `clean:build` custom section
//!   (emits `None` when the compiler hasn't stamped one yet)
//! - Snapshot of installed plugin bridge declarations
//! - Originating WASM file path (when available)
//!
//! ## Where it goes
//!
//! Per the user preference for local-only transport, reports are written
//! to `$CLEAN_DIAG_DIR/pending/<sha256>/report.json` (default
//! `./diagnostics/pending/<sha256>/report.json`) alongside the broken
//! `module.wasm` bytes. A `count.txt` tracks occurrences so repeat
//! failures do not explode the working directory.
//!
//! Developers surface these via the `clean-server errors` subcommand
//! (see `main.rs`).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{debug, error, warn};

/// Error code used by the dashboard and throughout the lifecycle.
pub const ERROR_CODE: &str = "RUNTIME_WASM_PARSE";

/// Maximum size of WASM bytes cached alongside a report. Beyond this we
/// skip the `module.wasm` write to avoid filling the working directory
/// with huge modules.
const MAX_CACHED_WASM_BYTES: usize = 64 * 1024 * 1024;

/// Report status in the on-disk lifecycle.
///
/// Maps loosely to the lifecycle stages defined in
/// `.claude/rules/tier1-foundations.md` Principle 1.1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Pending,
    Published,
    Resolved,
}

impl ReportStatus {
    fn as_dir(self) -> &'static str {
        match self {
            ReportStatus::Pending => "pending",
            ReportStatus::Published => "published",
            ReportStatus::Resolved => "resolved",
        }
    }
}

/// Snapshot of a single plugin's bridge declarations at report time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifestEntry {
    pub name: String,
    pub version: String,
    pub path: String,
    pub bridge_functions: Vec<String>,
}

/// Structured diagnostic payload for a `RUNTIME_WASM_PARSE` failure.
///
/// Produced at the wasmtime `Module::new` call site in `wasm.rs` and
/// serialized as `report.json` alongside the broken `module.wasm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmParseReport {
    pub error_code: String,
    pub reported_at: String,
    pub server_version: String,
    pub wasmtime_error: String,
    pub wasm_bytes_len: usize,
    pub wasm_sha256: String,
    pub wasm_header_hex: String,
    pub wasmparser_validates: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wasmparser_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<String>,
    pub compiler_version_source: String,
    pub plugin_manifest: Vec<PluginManifestEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_path: Option<String>,
    pub status: ReportStatus,
}

impl WasmParseReport {
    /// Build a new report from the raw crash-site inputs.
    ///
    /// Performs (in order): SHA-256, `wasmparser::validate`, compiler
    /// version extraction, plugin manifest snapshot. Does NOT write to
    /// disk — call [`WasmParseReport::emit`] for that.
    pub fn new(
        wasm_bytes: &[u8],
        wasmtime_error: &(impl std::fmt::Display + ?Sized),
        module_path: Option<&Path>,
    ) -> Self {
        let wasm_sha256 = sha256_hex(wasm_bytes);
        let wasm_header_hex = header_hex(wasm_bytes, 256);

        let (wasmparser_validates, wasmparser_error) = validate_with_wasmparser(wasm_bytes);

        let (compiler_version, compiler_version_source) = extract_compiler_version(wasm_bytes);

        let plugin_manifest = snapshot_plugin_manifest();

        Self {
            error_code: ERROR_CODE.to_string(),
            reported_at: Utc::now().to_rfc3339(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            wasmtime_error: wasmtime_error.to_string(),
            wasm_bytes_len: wasm_bytes.len(),
            wasm_sha256,
            wasm_header_hex,
            wasmparser_validates,
            wasmparser_error,
            compiler_version,
            compiler_version_source: compiler_version_source.to_string(),
            plugin_manifest,
            module_path: module_path.map(|p| p.display().to_string()),
            status: ReportStatus::Pending,
        }
    }

    /// First 12 hex chars of the sha — used in human-facing error
    /// messages and CLI listings.
    pub fn short_fingerprint(&self) -> &str {
        &self.wasm_sha256[..self.wasm_sha256.len().min(12)]
    }

    /// Write this report (plus the broken `.wasm` bytes) to the
    /// diagnostics directory, deduping on SHA.
    ///
    /// If a report with the same SHA already exists under `pending/`
    /// (the common case for repeat failures), the JSON is overwritten
    /// with refreshed `reported_at` / `wasmtime_error`, the `.wasm`
    /// bytes are left alone, and `count.txt` is incremented.
    ///
    /// Always emits a structured `tracing::error!` regardless of disk
    /// outcome, so logs still carry the payload even when disk writes
    /// fail.
    pub fn emit(&self, wasm_bytes: &[u8], diag_root: &Path) -> io::Result<PathBuf> {
        self.emit_tracing();

        let report_dir = diag_root
            .join(ReportStatus::Pending.as_dir())
            .join(&self.wasm_sha256);
        fs::create_dir_all(&report_dir)?;

        let report_path = report_dir.join("report.json");
        let wasm_path = report_dir.join("module.wasm");
        let count_path = report_dir.join("count.txt");

        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(&report_path, json)?;

        if !wasm_path.exists() && wasm_bytes.len() <= MAX_CACHED_WASM_BYTES {
            fs::write(&wasm_path, wasm_bytes)?;
        } else if wasm_bytes.len() > MAX_CACHED_WASM_BYTES {
            debug!(
                "Skipping module.wasm cache for {} (size {} > {} limit)",
                self.short_fingerprint(),
                wasm_bytes.len(),
                MAX_CACHED_WASM_BYTES
            );
        }

        let count = read_count(&count_path).unwrap_or(0) + 1;
        fs::write(&count_path, count.to_string())?;

        Ok(report_dir)
    }

    fn emit_tracing(&self) {
        match serde_json::to_string(self) {
            Ok(json) => error!(
                target: "wasm_parse_report",
                sha = %self.wasm_sha256,
                server_version = %self.server_version,
                wasmparser_validates = self.wasmparser_validates,
                "{}",
                json
            ),
            Err(e) => warn!(
                "Failed to serialize WasmParseReport for logging: {} (sha={})",
                e, self.wasm_sha256
            ),
        }
    }
}

/// Resolve the diagnostics root directory.
///
/// Honors the `CLEAN_DIAG_DIR` env var when set, otherwise defaults to
/// `./diagnostics/` relative to the server's working directory.
pub fn diag_dir() -> PathBuf {
    std::env::var("CLEAN_DIAG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./diagnostics"))
}

/// Summary row for `clean-server errors list`.
#[derive(Debug, Clone, Serialize)]
pub struct ReportSummary {
    pub sha: String,
    pub short: String,
    pub status: ReportStatus,
    pub reported_at: String,
    pub server_version: String,
    pub compiler_version: Option<String>,
    pub occurrences: u64,
    pub wasmparser_validates: bool,
    pub wasmtime_error_first_line: String,
    pub report_dir: PathBuf,
}

/// Enumerate all reports across all lifecycle stages.
///
/// Iterates `pending/`, `published/`, and `resolved/` subdirectories in
/// that order.
pub fn list_reports(diag_root: &Path) -> io::Result<Vec<ReportSummary>> {
    let mut out = Vec::new();
    for status in [
        ReportStatus::Pending,
        ReportStatus::Published,
        ReportStatus::Resolved,
    ] {
        let dir = diag_root.join(status.as_dir());
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            match load_summary(&entry.path(), status) {
                Ok(Some(summary)) => out.push(summary),
                Ok(None) => {}
                Err(e) => warn!(
                    "Skipping unreadable diagnostic at {:?}: {}",
                    entry.path(),
                    e
                ),
            }
        }
    }
    out.sort_by(|a, b| b.reported_at.cmp(&a.reported_at));
    Ok(out)
}

fn load_summary(report_dir: &Path, status: ReportStatus) -> io::Result<Option<ReportSummary>> {
    let report_path = report_dir.join("report.json");
    if !report_path.exists() {
        return Ok(None);
    }
    let json = fs::read_to_string(&report_path)?;
    let report: WasmParseReport = serde_json::from_str(&json).map_err(io::Error::other)?;
    let occurrences = read_count(&report_dir.join("count.txt")).unwrap_or(1);
    let first_line = report
        .wasmtime_error
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Ok(Some(ReportSummary {
        sha: report.wasm_sha256.clone(),
        short: report.short_fingerprint().to_string(),
        status,
        reported_at: report.reported_at.clone(),
        server_version: report.server_version.clone(),
        compiler_version: report.compiler_version.clone(),
        occurrences,
        wasmparser_validates: report.wasmparser_validates,
        wasmtime_error_first_line: first_line,
        report_dir: report_dir.to_path_buf(),
    }))
}

/// Find a report directory across lifecycle stages by SHA prefix.
///
/// Accepts any prefix ≥ 4 chars that uniquely identifies one report.
pub fn find_report_dir(
    diag_root: &Path,
    sha_prefix: &str,
) -> io::Result<Option<(PathBuf, ReportStatus)>> {
    if sha_prefix.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SHA prefix must be at least 4 characters",
        ));
    }
    let mut matches = Vec::new();
    for status in [
        ReportStatus::Pending,
        ReportStatus::Published,
        ReportStatus::Resolved,
    ] {
        let dir = diag_root.join(status.as_dir());
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(sha_prefix) {
                matches.push((entry.path(), status));
            }
        }
    }
    if matches.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "SHA prefix '{}' is ambiguous ({} matches)",
                sha_prefix,
                matches.len()
            ),
        ));
    }
    Ok(matches.into_iter().next())
}

/// Load a full report by SHA prefix.
pub fn load_report(diag_root: &Path, sha_prefix: &str) -> io::Result<Option<WasmParseReport>> {
    let Some((dir, _)) = find_report_dir(diag_root, sha_prefix)? else {
        return Ok(None);
    };
    let json = fs::read_to_string(dir.join("report.json"))?;
    let report: WasmParseReport = serde_json::from_str(&json).map_err(io::Error::other)?;
    Ok(Some(report))
}

/// Transition a report to a new lifecycle stage by moving its directory.
///
/// Returns the new directory path.
pub fn transition(
    diag_root: &Path,
    sha_prefix: &str,
    to: ReportStatus,
) -> io::Result<PathBuf> {
    let Some((from_dir, from_status)) = find_report_dir(diag_root, sha_prefix)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no diagnostic matches '{}'", sha_prefix),
        ));
    };
    if from_status == to {
        return Ok(from_dir);
    }

    let sha = from_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid diagnostic dir"))?;

    let target_parent = diag_root.join(to.as_dir());
    fs::create_dir_all(&target_parent)?;
    let target_dir = target_parent.join(&sha);

    // Update status inside the report.json before moving.
    let report_path = from_dir.join("report.json");
    if report_path.exists() {
        let json = fs::read_to_string(&report_path)?;
        let mut report: WasmParseReport =
            serde_json::from_str(&json).map_err(io::Error::other)?;
        report.status = to;
        // Stage 5 (resolved) strips the heavy fields per the retention policy.
        if matches!(to, ReportStatus::Resolved) {
            report.wasm_header_hex = String::new();
            report.plugin_manifest.clear();
        }
        let new_json = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;
        fs::write(&report_path, new_json)?;

        if matches!(to, ReportStatus::Resolved) {
            // Also delete the cached module.wasm — fix is released, we
            // no longer need the bytes.
            let wasm_path = from_dir.join("module.wasm");
            if wasm_path.exists() {
                let _ = fs::remove_file(&wasm_path);
            }
        }
    }

    fs::rename(&from_dir, &target_dir)?;
    Ok(target_dir)
}

// ---------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn header_hex(bytes: &[u8], max_len: usize) -> String {
    let slice = &bytes[..bytes.len().min(max_len)];
    hex::encode(slice)
}

fn read_count(path: &Path) -> io::Result<u64> {
    let s = fs::read_to_string(path)?;
    s.trim()
        .parse::<u64>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Run `wasmparser::validate` as a second opinion. Returns
/// `(validates, error_text)`.
fn validate_with_wasmparser(bytes: &[u8]) -> (bool, Option<String>) {
    let mut validator = wasmparser::Validator::new();
    match validator.validate_all(bytes) {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    }
}

/// Extract compiler version from the `clean:build` custom section.
///
/// Returns `(Some(version), "clean:build")` when present, or
/// `(None, "unknown")` when absent.
///
/// Uses two strategies: first tries the wasmparser streaming walk
/// (works for well-formed modules), then falls back to a raw byte
/// scan for the `clean:build` marker (works even when the module is
/// corrupted earlier in the file — which is exactly the case we care
/// about, since these reports are generated for broken WASM).
fn extract_compiler_version(wasm_bytes: &[u8]) -> (Option<String>, &'static str) {
    if let Some(result) = extract_compiler_version_wasmparser(wasm_bytes) {
        return result;
    }
    if let Some(result) = extract_compiler_version_raw_scan(wasm_bytes) {
        return result;
    }
    (None, "unknown")
}

fn extract_compiler_version_wasmparser(
    wasm_bytes: &[u8],
) -> Option<(Option<String>, &'static str)> {
    use wasmparser::{Parser, Payload};

    for payload in Parser::new(0).parse_all(wasm_bytes) {
        match payload {
            Ok(Payload::CustomSection(section)) => {
                if section.name() != "clean:build" {
                    continue;
                }
                return Some(parse_build_section_data(section.data()));
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    None
}

fn extract_compiler_version_raw_scan(
    wasm_bytes: &[u8],
) -> Option<(Option<String>, &'static str)> {
    const MARKER: &[u8] = b"clean:build";
    let pos = wasm_bytes
        .windows(MARKER.len())
        .position(|w| w == MARKER)?;
    let after = &wasm_bytes[pos + MARKER.len()..];
    // The JSON payload starts with `{` immediately after the marker.
    let json_start = after.iter().position(|&b| b == b'{')?;
    let json_region = &after[json_start..];
    let json_end = json_region.iter().position(|&b| b == b'}')?;
    let json_slice = &json_region[..=json_end];
    Some(parse_build_section_data(json_slice))
}

fn parse_build_section_data(data: &[u8]) -> (Option<String>, &'static str) {
    if let Ok(parsed) = serde_json::from_slice::<BTreeMap<String, String>>(data)
        && let Some(version) = parsed.get("compiler_version")
    {
        return (Some(version.clone()), "clean:build");
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return (Some(trimmed.to_string()), "clean:build");
        }
    }
    (None, "unknown")
}

/// Snapshot of `[bridge]` declarations from installed plugins.
///
/// Looks under `$HOME/.cleen/plugins/*/plugin.toml`. Silently skips
/// unreadable or malformed plugin directories — we never want a broken
/// manifest to mask the original WASM parse failure.
fn snapshot_plugin_manifest() -> Vec<PluginManifestEntry> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let plugins_root = PathBuf::from(home).join(".cleen").join("plugins");
    let Ok(entries) = fs::read_dir(&plugins_root) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let plugin_toml = plugin_dir.join("plugin.toml");
        if !plugin_toml.exists() {
            continue;
        }
        if let Some(parsed) = parse_plugin_toml(&plugin_toml) {
            out.push(parsed);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn parse_plugin_toml(path: &Path) -> Option<PluginManifestEntry> {
    let text = fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;

    let plugin_table = value.get("plugin")?.as_table()?;
    let name = plugin_table.get("name")?.as_str()?.to_string();
    let version = plugin_table
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let bridge_functions = value
        .get("bridge")
        .and_then(|b| b.get("functions"))
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_table())
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(PluginManifestEntry {
        name,
        version,
        path: path.display().to_string(),
        bridge_functions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MINIMAL_WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    #[test]
    fn sha256_is_deterministic() {
        let a = sha256_hex(MINIMAL_WASM);
        let b = sha256_hex(MINIMAL_WASM);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn header_hex_truncates_to_max_len() {
        let bytes = vec![0xABu8; 1000];
        let h = header_hex(&bytes, 256);
        assert_eq!(h.len(), 512); // 256 bytes → 512 hex chars
    }

    #[test]
    fn header_hex_handles_short_input() {
        let h = header_hex(MINIMAL_WASM, 256);
        assert_eq!(h.len(), MINIMAL_WASM.len() * 2);
    }

    #[test]
    fn wasmparser_accepts_minimal_module() {
        let (validates, err) = validate_with_wasmparser(MINIMAL_WASM);
        assert!(validates);
        assert!(err.is_none());
    }

    #[test]
    fn wasmparser_rejects_garbage() {
        let (validates, err) = validate_with_wasmparser(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(!validates);
        assert!(err.is_some());
    }

    #[test]
    fn compiler_version_reports_unknown_when_missing() {
        let (version, source) = extract_compiler_version(MINIMAL_WASM);
        assert!(version.is_none());
        assert_eq!(source, "unknown");
    }

    #[test]
    fn report_new_populates_all_core_fields() {
        let report = WasmParseReport::new(
            MINIMAL_WASM,
            &"garbage error at offset 42",
            Some(Path::new("/tmp/app.wasm")),
        );
        assert_eq!(report.error_code, ERROR_CODE);
        assert_eq!(report.wasm_bytes_len, MINIMAL_WASM.len());
        assert_eq!(report.wasm_sha256.len(), 64);
        assert!(report.wasmtime_error.contains("offset 42"));
        assert_eq!(report.module_path.as_deref(), Some("/tmp/app.wasm"));
        assert_eq!(report.status, ReportStatus::Pending);
        assert!(report.wasmparser_validates); // minimal module is valid
    }

    #[test]
    fn emit_writes_report_and_wasm_bytes() {
        let tmp = TempDir::new().unwrap();
        let report = WasmParseReport::new(MINIMAL_WASM, &"boom", None);
        let dir = report.emit(MINIMAL_WASM, tmp.path()).unwrap();
        assert!(dir.join("report.json").exists());
        assert!(dir.join("module.wasm").exists());
        assert!(dir.join("count.txt").exists());
        let count = fs::read_to_string(dir.join("count.txt")).unwrap();
        assert_eq!(count.trim(), "1");
    }

    #[test]
    fn emit_increments_count_on_repeat() {
        let tmp = TempDir::new().unwrap();
        let report = WasmParseReport::new(MINIMAL_WASM, &"boom", None);
        report.emit(MINIMAL_WASM, tmp.path()).unwrap();
        let dir = report.emit(MINIMAL_WASM, tmp.path()).unwrap();
        report.emit(MINIMAL_WASM, tmp.path()).unwrap();
        let count = fs::read_to_string(dir.join("count.txt")).unwrap();
        assert_eq!(count.trim(), "3");
    }

    #[test]
    fn find_report_dir_rejects_short_prefix() {
        let tmp = TempDir::new().unwrap();
        let err = find_report_dir(tmp.path(), "ab").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn transition_moves_directory_and_updates_status() {
        let tmp = TempDir::new().unwrap();
        let report = WasmParseReport::new(MINIMAL_WASM, &"boom", None);
        let sha = report.wasm_sha256.clone();
        report.emit(MINIMAL_WASM, tmp.path()).unwrap();

        let new_dir = transition(tmp.path(), &sha, ReportStatus::Published).unwrap();
        assert!(new_dir.starts_with(tmp.path().join("published")));
        assert!(!tmp.path().join("pending").join(&sha).exists());

        let reloaded: WasmParseReport =
            serde_json::from_str(&fs::read_to_string(new_dir.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(reloaded.status, ReportStatus::Published);
    }

    #[test]
    fn resolving_strips_heavy_fields() {
        let tmp = TempDir::new().unwrap();
        let report = WasmParseReport::new(MINIMAL_WASM, &"boom", None);
        let sha = report.wasm_sha256.clone();
        report.emit(MINIMAL_WASM, tmp.path()).unwrap();

        let new_dir = transition(tmp.path(), &sha, ReportStatus::Resolved).unwrap();
        let reloaded: WasmParseReport =
            serde_json::from_str(&fs::read_to_string(new_dir.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(reloaded.status, ReportStatus::Resolved);
        assert!(reloaded.wasm_header_hex.is_empty());
        assert!(reloaded.plugin_manifest.is_empty());
        assert!(!new_dir.join("module.wasm").exists());
    }
}
