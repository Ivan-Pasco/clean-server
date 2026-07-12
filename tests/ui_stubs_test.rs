//! UI Bridge Stub Coverage Test
//!
//! Verifies that the server linker registers stubs for every frame.ui bridge
//! function declared with `server_impl = "stub"` in
//! `clean-framework/plugins/frame.ui/plugin.toml`, plus browser-only functions
//! that the `events:` DSL may emit into server-mode WASM.
//!
//! Without these stubs, any WASM module compiled with a `component: client="on"`
//! block fails `Linker::instantiate` with
//! `unknown import: env::_ui_on_event`. This test catches missing stubs before
//! they cause production `LinkError`s.
//!
//! # How It Works
//!
//! 1. Parses `clean-framework/plugins/frame.ui/plugin.toml` `[bridge]` section.
//! 2. Collects every entry that has `server_impl = "stub"` (browser-side
//!    functions that need no-op stubs on the server).
//! 3. Also includes browser-only functions that the `events:` DSL may emit.
//! 4. Generates a synthetic WAT module importing every function.
//! 5. Instantiates it against the full server linker.
//! 6. Fails with the precise missing import if any stub is absent.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::path::PathBuf;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

fn locate_ui_plugin_toml() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur = manifest_dir.as_path();
    loop {
        let candidate = cur.join("clean-framework/plugins/frame.ui/plugin.toml");
        if candidate.exists() {
            return candidate;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => panic!(
                "Could not locate clean-framework/plugins/frame.ui/plugin.toml \
                 starting from {}",
                manifest_dir.display()
            ),
        }
    }
}

struct BridgeEntry {
    name: String,
    params: Vec<String>,
    returns: String,
}

fn extract_quoted(line: &str, key: &str) -> Option<String> {
    let needle = format!("{} = \"", key);
    let start = line.find(&needle)? + needle.len();
    let end_rel = line[start..].find('"')?;
    Some(line[start..start + end_rel].to_string())
}

fn extract_array(line: &str, key: &str) -> Vec<String> {
    let needle = format!("{} = [", key);
    let Some(start) = line.find(&needle) else {
        return Vec::new();
    };
    let start = start + needle.len();
    let Some(end_rel) = line[start..].find(']') else {
        return Vec::new();
    };
    let inner = &line[start..start + end_rel];
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse bridge entries with `server_impl = "stub"` from plugin.toml.
fn parse_stub_entries(text: &str) -> Vec<BridgeEntry> {
    let bridge_start = text
        .find("[bridge]")
        .expect("frame.ui plugin.toml missing [bridge] section");
    let rest = &text[bridge_start + "[bridge]".len()..];
    let bridge_end = rest
        .find("\n[")
        .map(|i| bridge_start + "[bridge]".len() + i)
        .unwrap_or(text.len());
    let block = &text[bridge_start..bridge_end];

    let mut entries = Vec::new();
    for line in block.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('{') {
            continue;
        }
        // Only include entries that have server_impl = "stub"
        if !trimmed.contains("server_impl = \"stub\"") {
            continue;
        }
        let Some(name) = extract_quoted(trimmed, "name") else {
            continue;
        };
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

fn expand_param_type(t: &str) -> Vec<&'static str> {
    match t {
        "string" => vec!["i32", "i32"],
        "integer" => vec!["i32"],
        "number" => vec!["f64"],
        "boolean" => vec!["i32"],
        "ptr" => vec!["i32"],
        other => panic!("Unknown param type in frame.ui/plugin.toml: '{}'", other),
    }
}

fn expand_return_type(t: &str) -> Option<&'static str> {
    match t {
        "" | "void" => None,
        "string" => Some("i32"),
        "ptr" => Some("i32"),
        "integer" => Some("i32"),
        "number" => Some("f64"),
        "boolean" => Some("i32"),
        other => panic!("Unknown return type in frame.ui/plugin.toml: '{}'", other),
    }
}

fn generate_wat_import(entry: &BridgeEntry) -> String {
    let mut import = format!("  (import \"env\" \"{}\" (func", entry.name);
    let params: Vec<&str> = entry
        .params
        .iter()
        .flat_map(|t| expand_param_type(t))
        .collect();
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

/// Extra browser-only functions the `events:` DSL may emit into server WASM.
/// These are not declared with `server_impl = "stub"` but still need stubs.
fn extra_browser_only_entries() -> Vec<BridgeEntry> {
    vec![
        BridgeEntry {
            name: "_ui_shortcut_register".into(),
            params: vec!["string".into(), "string".into(), "string".into()],
            returns: "integer".into(),
        },
        BridgeEntry {
            name: "_ui_shortcut_remove".into(),
            params: vec!["integer".into()],
            returns: "void".into(),
        },
        BridgeEntry {
            name: "_ui_shortcut_clear".into(),
            params: vec![],
            returns: "void".into(),
        },
        BridgeEntry {
            name: "_ui_navigate".into(),
            params: vec!["string".into()],
            returns: "void".into(),
        },
    ]
}

#[test]
fn ui_stubs_cover_browser_side_bridge() {
    let plugin_toml = locate_ui_plugin_toml();
    let text = std::fs::read_to_string(&plugin_toml)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", plugin_toml.display(), e));

    let mut entries = parse_stub_entries(&text);
    assert!(
        entries.len() >= 50,
        "Expected at least 50 stub entries in frame.ui/plugin.toml — found {}. \
         If plugin.toml changed, review parse_stub_entries().",
        entries.len()
    );

    entries.extend(extra_browser_only_entries());

    let mut wat = String::from("(module\n");
    for entry in &entries {
        wat.push_str(&generate_wat_import(entry));
        wat.push('\n');
    }
    wat.push(')');

    let engine = Engine::default();
    let module = Module::new(&engine, &wat).unwrap_or_else(|e| {
        panic!(
            "Failed to parse synthetic UI WAT module ({} imports): {}\nWAT:\n{}",
            entries.len(),
            e,
            &wat[..wat.len().min(800)]
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
            "Failed to instantiate UI WAT module with {} imports: {}\n\
             A browser-side `_ui_*` or `_storage_*` host stub is missing or \
             has the wrong signature. Add it to src/bridge_ui_stubs.rs.",
            entries.len(),
            e
        ),
    }
}
