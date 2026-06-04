//! Canvas Bridge Stub Coverage Test
//!
//! Verifies that the server linker registers stubs for every `_canvas_*`,
//! `_input_*`, `_audio_*`, `_sprite_*`, `_anim_*`, `_tween_*`, `_timeline_*`,
//! `_particles_*`, `_collision_*`, `_asset_*`, `_camera_*`, `_ease_*`,
//! `_gradient_*`, `_layer_*`, `_scene_*`, `_page_*`, `_path_*`, `_font_*`,
//! `_custom_ease_*`, `_animsprite_*`, and `_animstate_*` bridge function
//! declared by the frame.canvas plugin.
//!
//! Without these stubs, any WASM module compiled with a `canvasScene:` block
//! fails `Linker::instantiate` with `unknown import: env::_canvas_*`. This
//! test catches missing stubs at build time before they cause production
//! `LinkError`s.
//!
//! # How It Works
//!
//! 1. Parses `clean-framework/plugins/frame.canvas/plugin.toml` `[bridge]`
//!    section directly (no TOML library — surgical regex extraction matching
//!    the generator in `tools/gen_canvas_stubs.py`).
//! 2. Generates a synthetic WAT module that imports every entry using the
//!    **compiler's** WASM signature convention: Clean Language `Integer` is
//!    emitted as `i32` for plugin bridges (verified via `wasm-tools dump` on
//!    real canvas fixtures — this differs from the registry's "integer→i64"
//!    convention for Layer 2 functions).
//! 3. Builds the full server linker and instantiates the synthetic module.
//! 4. Fails with the precise missing import if any stub is absent.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::path::PathBuf;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

/// Path to frame.canvas plugin.toml. Resolved at runtime so the test does not
/// require a build-time dependency on the framework component.
fn locate_canvas_plugin_toml() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR until we find a folder containing
    // "clean-framework/plugins/frame.canvas/plugin.toml".
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur = manifest_dir.as_path();
    loop {
        let candidate = cur.join("clean-framework/plugins/frame.canvas/plugin.toml");
        if candidate.exists() {
            return candidate;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => panic!(
                "Could not locate clean-framework/plugins/frame.canvas/plugin.toml \
                 starting from {}",
                manifest_dir.display()
            ),
        }
    }
}

/// Extracted bridge entry from plugin.toml.
struct BridgeEntry {
    name: String,
    params: Vec<String>,
    returns: String,
}

/// Parse the `[bridge].functions` array of frame.canvas/plugin.toml using a
/// line-level regex. Mirrors `tools/gen_canvas_stubs.py::parse_entries`.
fn parse_canvas_bridge(text: &str) -> Vec<BridgeEntry> {
    let bridge_start = text.find("[bridge]").expect("frame.canvas plugin.toml missing [bridge] section");
    // The [bridge] section ends at the next top-level [...] heading.
    let rest = &text[bridge_start + "[bridge]".len()..];
    let bridge_end = rest
        .find("\n[")
        .map(|i| bridge_start + "[bridge]".len() + i)
        .unwrap_or(text.len());
    let block = &text[bridge_start..bridge_end];

    let mut entries = Vec::new();
    for line in block.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("{") {
            continue;
        }
        let Some(name) = extract_quoted(trimmed, "name") else { continue };
        let params = extract_array(trimmed, "params");
        let returns = extract_quoted(trimmed, "returns").unwrap_or_default();
        entries.push(BridgeEntry {
            name,
            params,
            returns,
        });
    }
    entries
}

/// Extract `key = "value"` from a bridge entry line.
fn extract_quoted(line: &str, key: &str) -> Option<String> {
    let needle = format!("{} = \"", key);
    let start = line.find(&needle)? + needle.len();
    let end_rel = line[start..].find('"')?;
    Some(line[start..start + end_rel].to_string())
}

/// Extract `key = ["a", "b", ...]` from a bridge entry line.
fn extract_array(line: &str, key: &str) -> Vec<String> {
    let needle = format!("{} = [", key);
    let Some(start) = line.find(&needle) else { return Vec::new() };
    let start = start + needle.len();
    let Some(end_rel) = line[start..].find(']') else { return Vec::new() };
    let inner = &line[start..start + end_rel];
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Expand a high-level parameter type to one or more WASM value type tokens
/// **using the compiler's plugin-bridge convention** (Integer → i32).
fn expand_param_type(t: &str) -> Vec<&'static str> {
    match t {
        "string"  => vec!["i32", "i32"],
        "integer" => vec!["i32"],
        "number"  => vec!["f64"],
        "boolean" => vec!["i32"],
        "ptr"     => vec!["i32"],
        other => panic!(
            "Unknown parameter type in frame.canvas/plugin.toml: '{}'. \
             Update expand_param_type() in canvas_stubs_test.rs if a new type was added.",
            other
        ),
    }
}

/// Expand a return type to a WASM value type token, or `None` for void.
fn expand_return_type(t: &str) -> Option<&'static str> {
    match t {
        ""        => None,
        "string"  => Some("i32"),
        "ptr"     => Some("i32"),
        "integer" => Some("i32"),
        "number"  => Some("f64"),
        "boolean" => Some("i32"),
        other => panic!(
            "Unknown return type in frame.canvas/plugin.toml: '{}'. \
             Update expand_return_type() in canvas_stubs_test.rs if a new type was added.",
            other
        ),
    }
}

fn generate_wat_import(entry: &BridgeEntry) -> String {
    let mut import = format!("  (import \"env\" \"{}\" (func", entry.name);

    let params: Vec<&str> = entry.params.iter().flat_map(|t| expand_param_type(t)).collect();
    if !params.is_empty() {
        import.push_str(" (param");
        for p in &params {
            import.push_str(&format!(" {}", p));
        }
        import.push(')');
    }

    if let Some(r) = expand_return_type(&entry.returns) {
        import.push_str(&format!(" (result {})", r));
    }

    import.push_str("))");
    import
}

#[test]
fn canvas_stubs_cover_full_plugin_bridge() {
    let plugin_toml = locate_canvas_plugin_toml();
    let text = std::fs::read_to_string(&plugin_toml)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", plugin_toml.display(), e));

    let entries = parse_canvas_bridge(&text);
    assert!(
        entries.len() >= 200,
        "Expected at least 200 canvas bridge entries in plugin.toml — found {}. \
         If plugin.toml shape changed, update parse_canvas_bridge().",
        entries.len()
    );

    // Build a single WAT module with every canvas import as the only contents.
    let mut wat = String::from("(module\n");
    for entry in &entries {
        wat.push_str(&generate_wat_import(entry));
        wat.push('\n');
    }
    wat.push(')');

    let engine = Engine::default();
    let module = Module::new(&engine, &wat).unwrap_or_else(|e| {
        panic!(
            "Failed to parse synthetic canvas WAT module ({} imports): {}\n\
             First 500 chars of WAT:\n{}",
            entries.len(),
            e,
            &wat[..wat.len().min(500)]
        )
    });

    let linker = create_linker(&engine).expect("create_linker should succeed");

    let router = Arc::new(Router::new());
    let state = WasmState::new(router);
    let mut store = Store::new(&engine, state);

    let result = linker.instantiate(&mut store, &module);
    match result {
        Ok(_) => {}
        Err(e) => panic!(
            "Failed to instantiate canvas WAT module with {} imports: {}\n\
             This means a `_canvas_*` / `_input_*` / `_audio_*` / `_sprite_*` etc. \
             host stub is missing or has the wrong signature. Regenerate stubs with:\n\
             \n\
             \tpython3 tools/gen_canvas_stubs.py > src/bridge_canvas_stubs.rs\n",
            entries.len(),
            e
        ),
    }
}
