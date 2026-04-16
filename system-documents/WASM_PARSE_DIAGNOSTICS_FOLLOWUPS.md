# WASM Parse Diagnostics — Follow-up Tasks

This document tracks work deferred from the initial `RUNTIME_WASM_PARSE`
enrichment (see prompt
`management/cross-component-prompts/server-runtime-wasm-parse-enriched-reports.md`).

The server-side Part A (enriched payload, plugin manifest snapshot,
local disk storage, `clean-server errors ...` CLI) has been implemented.
The items below require changes in other components and are intentionally
out of scope for the clean-server AI instance per the Cross-Component
Work Policy.

---

## 1. Compiler: emit `clean:build` custom section

**Component:** `clean-language-compiler`
**Priority:** Medium
**Status:** DONE — v0.30.54 (commit b1f9ecb, 2026-04-16)

### Implementation
The compiler emits a `clean:build` custom section at
`src/codegen/mir_codegen/utilities.rs` (active MIR codegen path).

Payload format:
```json
{
  "compiler_version": "0.30.54",
  "build_profile": "release"
}
```

**Note:** `git_commit` and `built_at` from the original request are
not yet included — would require a build.rs with `vergen` or similar.
The version alone enables bisection. Future enhancement tracked below.

### Verification
```bash
cln compile tests/cln/examples/simple_test.cln -o /tmp/check.wasm
python3 -c "data=open('/tmp/check.wasm','rb').read(); i=data.find(b'clean:build'); print(data[i:i+90].decode())"
# → clean:build{"build_profile":"release","compiler_version":"0.30.54"}
```

From clean-server: `clean-server errors show <sha>` should now report
`"compiler_version": "0.30.54"` instead of `"unknown"`.

### Remaining (low priority)
- Add `git_commit` via build script (`vergen` crate)
- Add `built_at` timestamp

---

## 2. Website: enriched payload ingestion and lifecycle-tiered retention

**Component:** Web Site Clean (dashboard)
**Priority:** Medium
**Status:** Not started

### Context
Prompt Part B (lifecycle-tiered retention) lives in the dashboard.
The server now emits structured payloads, but the dashboard currently
only accepts the legacy short-string form. Until the dashboard
understands the enriched payload, `clean-server errors publish` just
prints the JSON for a human to paste.

### Change required
1. Database migration adding:
   - `wasm_sha256 TEXT`
   - `wasm_bytes_len BIGINT`
   - `wasmtime_error_full TEXT`
   - `wasm_header_hex TEXT`
   - `wasmparser_validates BOOLEAN`
   - `plugin_manifest_json JSONB`
   - `compiler_version TEXT`
2. `errors_api.cln` endpoint:
   - Accept the enriched payload from `report_error`.
   - Dedupe by `(error_code, wasm_sha256)`.
   - On transition to `resolved`, strip `wasm_header_hex` and
     `plugin_manifest_json`.
3. Regression detector: when a report arrives whose
   `(error_code, wasm_sha256)` matches a resolved entry, auto-promote
   back to `reported` and notify the resolving developer.

---

## 3. MCP: directives + tools so AI instances auto-discover diagnostics

**Component:** Clean MCP
**Priority:** Medium
**Status:** Cross-component prompt written at
`management/cross-component-prompts/mcp-wasm-parse-diagnostics-directives.md`

### Context
The on-disk diagnostic reports are invisible to AI instances until
the MCP server tells them to look. Without this, every developer has
to manually instruct the AI to check `./diagnostics/pending/` — which
defeats the "good UX" goal.

### Change required
See the linked cross-component prompt. Summary:
- Add a session-start directive pointing AIs at
  `list_server_diagnostics` when a user reports a server failure.
- Add `list_server_diagnostics` and `show_server_diagnostic` tools
  that read the JSON reports directly (no shell-out).
- Augment `check_reported_fixes` to cross-reference local diagnostics
  against resolved fixes so the AI can suggest
  `clean-server errors resolve <sha>` at the right moment.

---

## 4. Server: `/_clean/diag/artifact/<sha>` route for the dashboard to pull bytes

**Component:** `clean-server`
**Priority:** Low (deferred by user preference: local-only v1)
**Status:** Deferred

### Context
The prompt specified `POST /api/v1/request-artifact/<sha256>` on the
website side, which would "trigger the originating server to upload
the held `.wasm` / `.cln` bytes". This requires:
- An authenticated read-side HTTP route on `clean-server`
- A reciprocal push from the dashboard

Adding an unauthenticated read route that leaks broken `.wasm` bytes
to any HTTP client would be a regression. Until the dashboard is
ready to call it with proper auth, we retain bytes locally at
`diagnostics/<stage>/<sha>/module.wasm` and expose them via the CLI
(`clean-server errors show --json`).

### Change required (when picked up)
- New route: `GET /_clean/diag/artifact/{sha}`
- Auth: require `Authorization: Bearer $CLEAN_DIAG_ARTIFACT_TOKEN`
- Serve bytes from the diagnostics directory; 404 when missing
- Rate-limit and log every access
