# CLAUDE.md - Clean Server Development Guide

This file provides guidance when working with the Clean Server codebase.

## Project Overview

Clean Server is the runtime server for Clean Language applications compiled to WebAssembly.

## Important Constraints

- **NEVER** write any reference to AI assistants in any documents, git commits, or any part of the code
- **NEVER** mention AI tools in git commit messages or any part of the codebase

## Cross-Component Work Policy

**CRITICAL: You are a Team Developer AI.** When you discover something in another component, choose the correct channel based on what you found:

| What you found | Channel | Why |
|---|---|---|
| A **bug** (crash, wrong output, spec violation, regression) | **`report_error` MCP tool** — MANDATORY | Fingerprint dedup, occurrence tracking, automatic user notification on fix, visible on errors.cleanlanguage.dev |
| A **design proposal, directive change, schema/API request, architectural ask** | Markdown file in `../foundation/management/cross-component-prompts/` | Requires discussion, not auto-fix |

**Never** write a markdown file for something that is a bug. Bug reports in markdown are invisible to the dashboard, don't notify users when fixed, and can't be queried via `list_component_bugs`.

### What You CAN Do

- Read files from other components to understand interfaces
- Call `report_error` for bugs found in other components
- Write markdown prompts for design/architecture discussions
- Update your component to work with existing interfaces

### What You MUST NOT Do

- Directly edit code in other components
- Make changes to other components' configuration files
- Write a markdown file for something that is a bug — use `report_error` instead

See `../foundation/management/USER_TYPES_AND_ERROR_REPORTING.md` for the full policy.

## Function Registry Spec Compliance

**CRITICAL: All host function signatures are enforced by the shared function registry.**

The file `../foundation/platform-architecture/function-registry.toml` is the single source of truth for ALL host function signatures (Layer 2 + Layer 3). Two automated spec compliance tests validate that every registered function matches the implementation:

- **Layer 2 test** (`test_spec_compliance` in `host-bridge/src/wasm_linker/mod.rs`) — validates 154 portable host function imports
- **Layer 3 test** (`test_layer3_spec_compliance` in `src/bridge.rs`) — validates 47 server-specific function imports

Both tests dynamically parse the TOML registry, expand high-level types to WASM types, generate WAT import declarations, and instantiate them against the linker.

### Rules for Modifying Host Functions

1. **NEVER** change a host function's WASM signature without updating `function-registry.toml`
2. **NEVER** modify the registry just to make a test pass — fix the implementation instead
3. When adding or changing a host function, follow this order:
   1. Update `../foundation/platform-architecture/function-registry.toml` (the authoritative source)
   2. Update the implementation (host-bridge for Layer 2, bridge.rs for Layer 3)
   3. Run `cargo test` to verify everything matches
4. The registry uses high-level types that expand to WASM types:
   - `"string"` → `(i32, i32)` ptr + len pair
   - `"integer"` → `(i64)` 64-bit signed
   - `"number"` → `(f64)` 64-bit float
   - `"boolean"` → `(i32)` 0 or 1
   - `"ptr"` → `(i32)` return: length-prefixed string pointer

### Key Signature Convention

- ALL string input parameters use raw `(ptr: i32, len: i32)` pairs
- Return strings use length-prefixed format: `[4-byte LE length][UTF-8 data]`
- Integer values use `i64` (not `i32`) for `print_integer`, `int_to_string`, `string_to_int`

### Running the Compliance Tests

```bash
# Layer 2 (host-bridge portable functions)
cd host-bridge && cargo test test_spec_compliance

# Layer 3 (server-specific functions)
cargo test test_layer3_spec_compliance
```

If a test fails, the error message identifies exactly which function has the wrong signature.

## Bridge Function Naming — Dual Registration (Temporary)

The compiler currently emits both `_namespace_fn` and `namespace.fn` import styles.
Both must be registered until the compiler is fixed to emit only canonical names
(tracked in `compiler-dual-naming-registry-sync.md`).

Use `register_bridge_fn!` when adding new bridge functions. See
`foundation/platform-architecture/HOST_BRIDGE.md § Dual Naming`.

### How it works

- **New functions (Layer 2):** use `host_bridge::register_bridge_fn!(linker, "env", "_namespace_fn", closure)?;` — macro derives the `namespace.fn` alias automatically.
- **New functions (Layer 3):** use the local `register_bridge_fn!(linker, "_namespace_fn", closure);` macro defined at the top of `src/bridge.rs`.
- **Existing functions:** covered by `register_dot_aliases()` post-registration loops in `host-bridge/src/wasm_linker/mod.rs` and `src/bridge.rs`.

### Enforcement

`tests/bridge_contract_test.rs::bridge_covers_registry` probes every canonical name and alias from `function-registry.toml` individually and reports all missing registrations in one failure message. This test must stay green.

## Documentation Sync Protocol

Facts about the language live in `foundation/spec/` (at the project root). Facts about the platform live in `foundation/platform-architecture/`. Do not duplicate them here — link to them instead.

**When you make a change in this component, update the corresponding spec file in the same commit:**

| Change type | Update required |
|-------------|-----------------|
| New or changed host bridge function | `foundation/platform-architecture/HOST_BRIDGE.md` |
| New or changed execution layer | `foundation/platform-architecture/EXECUTION_LAYERS.md` |
| New or changed plugin contract | `foundation/spec/plugins/plugin-contract.md` |

The spec files are the single source of truth. Component documentation explains implementation — it does not redefine language rules.
