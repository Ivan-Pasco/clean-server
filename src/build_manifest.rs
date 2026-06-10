//! Build manifest loader — Plugin Contracts v2 (Accepted 2026-06-09).
//!
//! See `foundation/spec/plugins/contracts/artifacts.md` §5 and §8 for the
//! contract. The compiler (>= 0.30.257) writes a `build-manifest.json` next to
//! the main WASM listing every artifact produced (`<main>.wasm`,
//! `frontend.wasm`, future plugin-declared artifacts). Hosts read it at
//! startup to locate artifacts by declared `purpose` instead of probing
//! conventional paths.
//!
//! Closes SRV004 end-to-end. The current legacy probe in
//! `server::serve_frontend_wasm` ran against the working directory; under this
//! contract we read the artifact entry and resolve `path_relative` against the
//! WASM directory.
//!
//! ## Phase B compatibility
//!
//! When the manifest is absent (older compiler, or a build that did not write
//! one) the caller falls back to the legacy CWD + `public/` probe. When the
//! manifest IS present and the file it points to is missing, callers must
//! return a clear error rather than searching ambient paths — see §5 of the
//! contract.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Canonical filename emitted by the compiler. Hosts look for this name as a
/// sibling of the main WASM. Matches
/// `clean-language-compiler::build_manifest::BUILD_MANIFEST_FILENAME`.
pub const BUILD_MANIFEST_FILENAME: &str = "build-manifest.json";

/// Documented `purpose` values consumers handle.
pub mod purpose {
    pub const MAIN_MODULE: &str = "main_module";
    pub const CLIENT_HYDRATION: &str = "client_hydration";
    pub const STATIC_ASSET: &str = "static_asset";
    pub const MANIFEST: &str = "manifest";
    pub const DATA_MIGRATION: &str = "data_migration";
}

/// Subset of the compiler's `BuildManifest` shape we consume. Extra fields
/// (build_state, etc.) are tolerated and ignored. Callbacks are the v2
/// dispatch contract surface — see
/// `foundation/spec/plugins/contracts/bridge-host-classes.md` §4.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildManifest {
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub compiler_version: String,
    #[serde(default)]
    pub artifacts: Vec<BuildArtifact>,
    /// Resolved `[bridge.functions.callback]` declarations from every
    /// loaded plugin. The host scans this list at startup to learn which
    /// bridge functions need to dispatch back into the WASM module and how.
    /// See bridge-host-classes.md §4.
    #[serde(default)]
    pub callbacks: Vec<CallbackContract>,
}

/// One resolved callback contract — the host-facing view of a
/// `[bridge.functions.callback]` block. Closes SRV001 for clean-server when
/// `purpose == "component_tag_render"`.
#[derive(Debug, Clone, Deserialize)]
pub struct CallbackContract {
    /// Bridge function the callback is attached to (e.g. `_ui_render_page`).
    pub bridge: String,
    /// Documented purpose. Values from bridge-host-classes.md §4.1.
    pub purpose: String,
    /// Plugin whose exports the host should look up (e.g. `"frame.ui"`).
    pub plugin_target: String,
    /// How the host finds the right export.
    /// Values: `"exports_matching"`, `"manifest_lookup"`, `"explicit_argument"`.
    pub discovery: String,
    /// Symbol pattern with `{placeholder}` substitution when
    /// `discovery == "exports_matching"`.
    #[serde(default)]
    pub export_pattern: Option<String>,
    /// Fallback when no matching export is found.
    /// Values: `"passthrough"`, `"error"`, `"empty"`.
    pub fallback: String,
    /// Plugin that declared this callback (e.g. `"frame.ui"`).
    #[serde(default)]
    pub declared_by_plugin: String,
}

/// Documented `purpose` values for callbacks. Mirror of §4.1.
pub mod callback_purpose {
    pub const COMPONENT_TAG_RENDER: &str = "component_tag_render";
    pub const ROUTE_DISPATCH: &str = "route_dispatch";
    pub const MIGRATION_APPLY: &str = "migration_apply";
    pub const EVENT_DISPATCH: &str = "event_dispatch";
}

/// Documented `fallback` values for callbacks. Mirror of §4 fallback field.
pub mod callback_fallback {
    pub const PASSTHROUGH: &str = "passthrough";
    pub const ERROR: &str = "error";
    pub const EMPTY: &str = "empty";
}

/// Subset of `BuildArtifact` the host needs. Unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildArtifact {
    pub name: String,
    pub path_relative: String,
    pub purpose: String,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub source_plugin: Option<String>,
}

/// Resolved artifact ready for serving: combines the manifest entry with the
/// absolute filesystem path the host should read from.
#[derive(Debug, Clone)]
pub struct ResolvedArtifact {
    pub name: String,
    pub purpose: String,
    pub public: bool,
    pub content_type: String,
    pub absolute_path: PathBuf,
}

impl BuildManifest {
    /// Resolve the manifest path that sits alongside `main_wasm_path`.
    pub fn manifest_path_for(main_wasm_path: &Path) -> PathBuf {
        let dir = main_wasm_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty());
        match dir {
            Some(d) => d.join(BUILD_MANIFEST_FILENAME),
            None => PathBuf::from(BUILD_MANIFEST_FILENAME),
        }
    }

    /// Try to load the manifest that sits next to `main_wasm_path`. Returns
    /// `Ok(None)` when the manifest is absent (Phase B compatibility: caller
    /// falls back to legacy lookup). Returns `Err` only when the file exists
    /// but cannot be read or parsed — that's a hard failure the host surfaces.
    pub fn load_alongside(main_wasm_path: &Path) -> Result<Option<Self>, ManifestLoadError> {
        let manifest_path = Self::manifest_path_for(main_wasm_path);
        if !manifest_path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&manifest_path).map_err(|e| ManifestLoadError {
            manifest_path: manifest_path.clone(),
            kind: ManifestLoadErrorKind::Read(e),
        })?;
        let manifest: BuildManifest =
            serde_json::from_slice(&bytes).map_err(|e| ManifestLoadError {
                manifest_path: manifest_path.clone(),
                kind: ManifestLoadErrorKind::Parse(e),
            })?;
        Ok(Some(manifest))
    }

    /// Resolve every artifact against `main_wasm_dir` (the parent directory of
    /// the WASM the server was given). Returns artifacts in their declared
    /// order so the host can preserve plugin-specified routing precedence.
    pub fn resolve_artifacts(&self, main_wasm_dir: &Path) -> Vec<ResolvedArtifact> {
        self.artifacts
            .iter()
            .map(|a| {
                let absolute_path = resolve_artifact_path(main_wasm_dir, &a.path_relative);
                ResolvedArtifact {
                    name: a.name.clone(),
                    purpose: a.purpose.clone(),
                    public: a.public,
                    content_type: if a.content_type.is_empty() {
                        infer_content_type(&a.name).to_string()
                    } else {
                        a.content_type.clone()
                    },
                    absolute_path,
                }
            })
            .collect()
    }

    /// Find the artifact with `purpose = "client_hydration"` (canonical
    /// `frontend.wasm` slot). Returns the first match — plugin contract
    /// guarantees one client-hydration artifact per build.
    pub fn client_hydration_artifact(&self) -> Option<&BuildArtifact> {
        self.artifacts
            .iter()
            .find(|a| a.purpose == purpose::CLIENT_HYDRATION)
    }
}

/// Resolve `path_relative` (from the manifest) against `main_wasm_dir`. If
/// `path_relative` is absolute, return it as-is. If `main_wasm_dir` is empty
/// (e.g. WASM path had no parent), resolve against ".".
pub fn resolve_artifact_path(main_wasm_dir: &Path, path_relative: &str) -> PathBuf {
    let candidate = Path::new(path_relative);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if main_wasm_dir.as_os_str().is_empty() {
        PathBuf::from(".").join(candidate)
    } else {
        main_wasm_dir.join(candidate)
    }
}

/// Best-effort MIME inference from filename suffix. Only used when the
/// manifest entry omits `content_type` (plugins should always set it; this is
/// a safety net for v1.0.0 compatibility entries).
pub fn infer_content_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".wasm") {
        "application/wasm"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "application/javascript"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".html") {
        "text/html"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

/// Error returned when a manifest file exists but cannot be loaded. Absence of
/// the file is NOT an error — it returns `Ok(None)` from `load_alongside`.
#[derive(Debug)]
pub struct ManifestLoadError {
    pub manifest_path: PathBuf,
    pub kind: ManifestLoadErrorKind,
}

#[derive(Debug)]
pub enum ManifestLoadErrorKind {
    Read(std::io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for ManifestLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ManifestLoadErrorKind::Read(e) => {
                write!(
                    f,
                    "failed to read build manifest at {:?}: {}",
                    self.manifest_path, e
                )
            }
            ManifestLoadErrorKind::Parse(e) => {
                write!(
                    f,
                    "failed to parse build manifest at {:?}: {}",
                    self.manifest_path, e
                )
            }
        }
    }
}

impl std::error::Error for ManifestLoadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, json: &str) -> PathBuf {
        let path = dir.join(BUILD_MANIFEST_FILENAME);
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn manifest_path_sits_next_to_main_wasm() {
        let path = Path::new("dist/app.wasm");
        assert_eq!(
            BuildManifest::manifest_path_for(path),
            PathBuf::from("dist/build-manifest.json")
        );
    }

    #[test]
    fn manifest_path_handles_no_parent() {
        let path = Path::new("app.wasm");
        assert_eq!(
            BuildManifest::manifest_path_for(path),
            PathBuf::from("build-manifest.json")
        );
    }

    #[test]
    fn load_alongside_returns_none_when_missing() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();

        let result = BuildManifest::load_alongside(&main).expect("load");
        assert!(result.is_none(), "manifest absent ⇒ Ok(None) for fallback");
    }

    #[test]
    fn load_alongside_returns_err_on_invalid_json() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(dir.path(), "{not valid json");

        let err = BuildManifest::load_alongside(&main).expect_err("parse error");
        match err.kind {
            ManifestLoadErrorKind::Parse(_) => {}
            ManifestLoadErrorKind::Read(_) => panic!("expected Parse error"),
        }
    }

    #[test]
    fn load_alongside_parses_frontend_wasm_entry() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(
            dir.path(),
            r#"{
                "schema_version": "1.0.0",
                "compiler_version": "0.30.257",
                "contract_version": "2.0.0",
                "runtime_abi_version": "1",
                "generated_at_epoch_seconds": 1700000000,
                "artifacts": [
                    {
                        "name": "app.wasm",
                        "path_relative": "app.wasm",
                        "purpose": "main_module",
                        "public": false,
                        "content_type": "application/wasm",
                        "size_bytes": 4,
                        "sha256": "0000",
                        "source_plugin": null
                    },
                    {
                        "name": "frontend.wasm",
                        "path_relative": "frontend.wasm",
                        "purpose": "client_hydration",
                        "public": true,
                        "content_type": "application/wasm",
                        "size_bytes": 4,
                        "sha256": "1111",
                        "source_plugin": "frame.ui"
                    }
                ]
            }"#,
        );

        let manifest = BuildManifest::load_alongside(&main).unwrap().unwrap();
        assert_eq!(manifest.artifacts.len(), 2);
        let frontend = manifest.client_hydration_artifact().unwrap();
        assert_eq!(frontend.name, "frontend.wasm");
        assert_eq!(frontend.source_plugin.as_deref(), Some("frame.ui"));
        assert!(frontend.public);
    }

    #[test]
    fn resolve_artifacts_produces_absolute_paths() {
        let dir = tempdir().unwrap();
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        let main = dist.join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(
            &dist,
            r#"{
                "schema_version": "1.0.0",
                "compiler_version": "0.30.257",
                "contract_version": "2.0.0",
                "runtime_abi_version": "1",
                "generated_at_epoch_seconds": 1700000000,
                "artifacts": [
                    {
                        "name": "frontend.wasm",
                        "path_relative": "frontend.wasm",
                        "purpose": "client_hydration",
                        "public": true,
                        "content_type": "application/wasm",
                        "size_bytes": 4,
                        "sha256": "1111",
                        "source_plugin": "frame.ui"
                    }
                ]
            }"#,
        );

        let manifest = BuildManifest::load_alongside(&main).unwrap().unwrap();
        let resolved = manifest.resolve_artifacts(&dist);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "frontend.wasm");
        assert_eq!(resolved[0].absolute_path, dist.join("frontend.wasm"));
        assert!(resolved[0].public);
        assert_eq!(resolved[0].purpose, purpose::CLIENT_HYDRATION);
    }

    #[test]
    fn resolve_artifact_path_keeps_absolute() {
        let abs = if cfg!(windows) {
            r"C:\absolute\theme.css"
        } else {
            "/absolute/theme.css"
        };
        let resolved = resolve_artifact_path(Path::new("/whatever"), abs);
        assert_eq!(resolved, PathBuf::from(abs));
    }

    #[test]
    fn infer_content_type_covers_common_extensions() {
        assert_eq!(infer_content_type("frontend.wasm"), "application/wasm");
        assert_eq!(infer_content_type("theme.css"), "text/css");
        assert_eq!(infer_content_type("loader.js"), "application/javascript");
        assert_eq!(infer_content_type("components.json"), "application/json");
        assert_eq!(infer_content_type("UNKNOWN.bin"), "application/octet-stream");
    }

    #[test]
    fn callbacks_parse_when_present() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(
            dir.path(),
            r#"{
                "schema_version": "1.0.0",
                "compiler_version": "0.30.260",
                "artifacts": [],
                "callbacks": [
                    {
                        "bridge": "_ui_render_page",
                        "purpose": "component_tag_render",
                        "plugin_target": "frame.ui",
                        "discovery": "exports_matching",
                        "export_pattern": "{tagname}_render",
                        "fallback": "passthrough",
                        "declared_by_plugin": "frame.ui"
                    }
                ]
            }"#,
        );
        let manifest = BuildManifest::load_alongside(&main).unwrap().unwrap();
        assert_eq!(manifest.callbacks.len(), 1);
        let cb = &manifest.callbacks[0];
        assert_eq!(cb.bridge, "_ui_render_page");
        assert_eq!(cb.purpose, callback_purpose::COMPONENT_TAG_RENDER);
        assert_eq!(cb.discovery, "exports_matching");
        assert_eq!(cb.export_pattern.as_deref(), Some("{tagname}_render"));
        assert_eq!(cb.fallback, callback_fallback::PASSTHROUGH);
    }

    #[test]
    fn callbacks_default_to_empty_when_absent() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(
            dir.path(),
            r#"{ "schema_version": "1.0.0", "artifacts": [] }"#,
        );
        let manifest = BuildManifest::load_alongside(&main).unwrap().unwrap();
        assert!(manifest.callbacks.is_empty());
    }

    #[test]
    fn missing_content_type_in_manifest_is_inferred() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("app.wasm");
        std::fs::write(&main, b"WASM").unwrap();
        write_manifest(
            dir.path(),
            r#"{
                "schema_version": "1.0.0",
                "artifacts": [
                    {
                        "name": "theme.css",
                        "path_relative": "theme.css",
                        "purpose": "static_asset",
                        "public": true
                    }
                ]
            }"#,
        );
        let manifest = BuildManifest::load_alongside(&main).unwrap().unwrap();
        let resolved = manifest.resolve_artifacts(dir.path());
        assert_eq!(resolved[0].content_type, "text/css");
    }
}
