# CLAUDE.md - Clean Server Development Guide

This file provides guidance when working with the Clean Server codebase.

## Project Overview

Clean Server is the runtime server for Clean Language applications compiled to WebAssembly.

## Important Constraints

- **NEVER** write any reference to AI assistants in any documents, git commits, or any part of the code
- **NEVER** mention AI tools in git commit messages or any part of the codebase

## Cross-Component Work Policy

**CRITICAL: AI Instance Separation of Concerns**

When working in this component and discovering errors, bugs, or required changes in **another component** (different folder in the Clean Language project), you must **NOT** directly fix or modify code in that other component.

Instead:

1. **Document the issue** by creating a prompt/task description
2. **Save the prompt** in a file that can be executed by the AI instance working in the correct folder
3. **Location for cross-component prompts**: Save prompts in `../foundation/management/cross-component-prompts/` at the project root

### Prompt Format for Cross-Component Issues

```
Component: [target component name, e.g., clean-language-compiler]
Issue Type: [bug/feature/enhancement/compatibility]
Priority: [critical/high/medium/low]
Description: [Detailed description of the issue discovered]
Context: [Why this was discovered while working in the current component]
Suggested Fix: [If known, describe the potential solution]
Files Affected: [List of files in the target component that need changes]
```

### Why This Rule Exists

- Each component has its own context, dependencies, and testing requirements
- AI instances are optimized for their specific component's codebase
- Cross-component changes without proper context can introduce bugs
- This maintains clear boundaries and accountability
- Ensures changes are properly tested in the target component's environment

### What You CAN Do

- Read files from other components to understand interfaces
- Document compatibility issues found
- Create detailed prompts for the correct AI instance
- Update your component to work with existing interfaces

### What You MUST NOT Do

- Directly edit code in other components
- Make changes to other components' configuration files
- Modify shared resources without coordination
- Skip the prompt creation step for cross-component issues

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
