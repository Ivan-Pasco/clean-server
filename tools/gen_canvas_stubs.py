#!/usr/bin/env python3
"""Generate Rust stub registrations for frame.canvas bridge functions.

Reads plugin.toml directly (not via TOML library since we need surgical extraction
of the [bridge].functions array, not its semantics).

Emits to stdout — pipe to clean-server/src/bridge/canvas_stubs.rs.
"""

import re
import sys
from pathlib import Path

PLUGIN_TOML = Path("/Users/earcandy/Documents/Dev/Clean Language/clean-framework/plugins/frame.canvas/plugin.toml")

# Type expansion for frame.canvas plugin bridge.
#
# The compiler emits Clean Language `Integer` as WASM i32 for plugin bridges
# (verified by `wasm-tools dump` on canvas test fixtures — every _canvas_*
# import uses [I32, ...] -> I32 for integer params and returns).
#
# This differs from foundation/platform-architecture/function-registry.toml,
# which documents "integer" -> i64 for Layer 2 portable functions. The drift
# is tracked under bug SYNC-PLUGIN-DRIFT. For these stubs, we MUST match the
# compiler's actual WASM output or instantiation will fail with a type mismatch
# rather than the missing-import error we are trying to resolve.
#
# Mapping for plugin-derived imports:
#   "string"  param  -> two i32s (ptr, len)
#   "integer" param  -> one i32  (per compiler, NOT i64)
#   "number"  param  -> one f64
#   "boolean" param  -> one i32
#   "ptr"     param  -> one i32
#   "string"  return -> i32 (length-prefixed string pointer; needs write_string_to_caller)
#   "ptr"     return -> i32
#   "integer" return -> i32  (per compiler, NOT i64)
#   "number"  return -> f64
#   "boolean" return -> i32

PARAM_EXPAND = {
    "string":  ["i32", "i32"],
    "integer": ["i32"],
    "number":  ["f64"],
    "boolean": ["i32"],
    "ptr":     ["i32"],
}

RET_TYPE = {
    "string":  "i32",
    "ptr":     "i32",
    "integer": "i32",
    "number":  "f64",
    "boolean": "i32",
    "":        None,  # void
}

RET_ZERO = {
    "i32": "0",
    "i64": "0",
    "f64": "0.0",
}


def parse_entries(text: str):
    """Extract every `{ name = "...", params = [...], returns = "..." }` line."""
    # Locate the [bridge] block boundaries.
    start = text.find("[bridge]")
    if start < 0:
        raise SystemExit("No [bridge] section found")
    end = text.find("\n[", start + 1)
    block = text[start:end if end > 0 else len(text)]

    entries = []
    pattern = re.compile(
        r'^\s*\{\s*name\s*=\s*"([^"]+)"\s*,\s*params\s*=\s*\[([^\]]*)\]\s*,\s*returns\s*=\s*"([^"]*)"',
        re.MULTILINE,
    )
    for m in pattern.finditer(block):
        name = m.group(1)
        params_raw = m.group(2).strip()
        params = []
        if params_raw:
            params = [p.strip().strip('"') for p in params_raw.split(",")]
            params = [p for p in params if p]
        ret = m.group(3)
        entries.append((name, params, ret))
    return entries


def emit_stub(name: str, params, ret: str) -> str:
    """Emit a single register_bridge_fn! invocation."""
    # Build the parameter list of the closure.
    arg_decls = []
    arg_idx = 0
    has_string_param = False
    for p in params:
        wasm_types = PARAM_EXPAND.get(p)
        if wasm_types is None:
            raise SystemExit(f"Unknown param type {p!r} in {name}")
        if p == "string":
            has_string_param = True
            arg_decls.append(f"_p{arg_idx}: i32")
            arg_decls.append(f"_l{arg_idx}: i32")
        else:
            arg_decls.append(f"_a{arg_idx}: {wasm_types[0]}")
        arg_idx += 1

    ret_ty = RET_TYPE.get(ret)
    if ret_ty is None and ret != "":
        raise SystemExit(f"Unknown return type {ret!r} in {name}")

    # String-returning stubs must write an empty length-prefixed string.
    if ret == "string":
        caller_arg = "mut caller: Caller<'_, WasmState>"
        body = "write_string_to_caller(&mut caller, \"\")"
    else:
        caller_arg = "_caller: Caller<'_, WasmState>"
        body = RET_ZERO[ret_ty] if ret_ty else ""

    arg_str = caller_arg + (", " + ", ".join(arg_decls) if arg_decls else "")

    if ret_ty is None:
        # Void
        return (
            f"    register_bridge_fn!(linker, \"{name}\",\n"
            f"        |{arg_str}| {{ {body} }});"
        )
    else:
        return (
            f"    register_bridge_fn!(linker, \"{name}\",\n"
            f"        |{arg_str}| -> {ret_ty} {{ {body} }});"
        )


def main():
    text = PLUGIN_TOML.read_text()
    entries = parse_entries(text)
    print(f"// {len(entries)} canvas bridge stubs generated from frame.canvas/plugin.toml", file=sys.stderr)

    out = []
    out.append("//! Frame.canvas bridge function stubs for the server runtime.")
    out.append("//!")
    out.append("//! The frame.canvas plugin is a client-only Layer 4 plugin: canvas drawing,")
    out.append("//! audio, and input are handled in the browser. However, WASM modules that use")
    out.append("//! `canvasScene:` blocks emit imports for every `_canvas_*`, `_input_*`,")
    out.append("//! `_audio_*`, `_sprite_*`, etc. bridge function regardless of whether the")
    out.append("//! module ultimately runs server-side.")
    out.append("//!")
    out.append("//! Without these stubs, `Linker::instantiate` fails with `unknown import: env::_canvas_init`")
    out.append("//! whenever the server loads a `.wasm` that has any canvas scene in it.")
    out.append("//!")
    out.append("//! This file registers a no-op for every bridge entry declared in")
    out.append("//! `clean-framework/plugins/frame.canvas/plugin.toml [bridge]`. The signatures")
    out.append("//! match the plugin.toml declarations exactly so the linker is satisfied.")
    out.append("//! None of these stubs perform any work — canvas scenes are not executed")
    out.append("//! server-side.")
    out.append("//!")
    out.append("//! AUTO-GENERATED by `tools/gen_canvas_stubs.py`. Do not edit by hand.")
    out.append("//! To regenerate after updating frame.canvas/plugin.toml:")
    out.append("//!   python3 tools/gen_canvas_stubs.py > src/bridge_canvas_stubs.rs")
    out.append("")
    out.append("use crate::error::{RuntimeError, RuntimeResult};")
    out.append("use crate::wasm::WasmState;")
    out.append("use host_bridge::write_string_to_caller;")
    out.append("use wasmtime::{Caller, Linker};")
    out.append("")
    out.append("/// Register Layer 3 bridge stubs for all frame.canvas host functions.")
    out.append("///")
    out.append("/// Macro mirrors the canonical `_namespace_fn` → `namespace.fn` dual registration")
    out.append("/// used in `bridge.rs`. See `foundation/platform-architecture/HOST_BRIDGE.md`")
    out.append("/// § Dual Naming.")
    out.append("macro_rules! register_bridge_fn {")
    out.append("    ($linker:expr, $name:literal, $func:expr) => {{")
    out.append("        $linker")
    out.append("            .func_wrap(\"env\", $name, $func)")
    out.append("            .map_err(|e| RuntimeError::wasm(format!(\"Failed to define {}: {}\", $name, e)))?;")
    out.append("        let _stripped: &str = $name.trim_start_matches('_');")
    out.append("        if $name.starts_with('_') && !$name.starts_with(\"__\") {")
    out.append("            if let Some(_dot_idx) = _stripped.find('_') {")
    out.append("                let _dot_name = format!(")
    out.append("                    \"{}.{}\",")
    out.append("                    &_stripped[.._dot_idx],")
    out.append("                    &_stripped[_dot_idx + 1..]")
    out.append("                );")
    out.append("                $linker")
    out.append("                    .alias(\"env\", $name, \"env\", &_dot_name)")
    out.append("                    .map_err(|e| RuntimeError::wasm(format!(")
    out.append("                        \"Failed to alias {} -> {}: {}\", $name, _dot_name, e")
    out.append("                    )))?;")
    out.append("            }")
    out.append("        }")
    out.append("    }};")
    out.append("}")
    out.append("")
    out.append("pub fn register_canvas_stubs(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {")

    for name, params, ret in entries:
        out.append(emit_stub(name, params, ret))

    out.append("")
    out.append("    Ok(())")
    out.append("}")
    out.append("")

    print("\n".join(out))


if __name__ == "__main__":
    main()
