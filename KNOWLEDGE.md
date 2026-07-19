# KNOWLEDGE.md — Clean Server

Known considerations and areas to watch. Read before modifying server code.

---

## 1. Host Bridge Contract

**What:** The server implements the Layer 2 (Host Bridge) and Layer 3 (Server Extensions) functions that WASM modules import. The function signatures in the server MUST match exactly what the compiler generates as WASM imports. Any mismatch causes runtime link failures.

**Where:** Bridge function implementations, WASM module instantiation code

**Watch for:** Adding or modifying bridge functions requires coordinating with `foundation/spec/platform/HOST_BRIDGE.md` and the corresponding plugin.toml `[bridge]` section. The compiler reads plugin.toml to generate import declarations.

---

## 2. String Passing Convention

**What:** Strings cross the WASM-host boundary as `(ptr, len)` pairs. The server must read from WASM memory using the length-prefix format (4 bytes length + content). Functions with `expand_strings = true` in plugin.toml have their string parameters automatically expanded by the compiler.

**Watch for:** Changes to string memory layout in the compiler must be mirrored in the server's string reading/writing code.

---

## 3. Planned Improvements

- Windows process checking (`core/frame.rs`) — not yet implemented
- Plugin checksum verification (`plugin/registry.rs`) — planned
