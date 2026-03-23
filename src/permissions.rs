//! Plugin permission enforcement
//!
//! Reads the `clean:permissions` custom section from WASM binaries and enforces
//! function-level access control at bridge call time.
//!
//! # Permission model
//!
//! - If a WASM module contains a `clean:permissions` custom section, only the
//!   function names listed in that section may be called through the Layer 3
//!   server bridge.
//! - If no `clean:permissions` section is present, all functions are permitted
//!   (backward-compatible with modules compiled before this feature existed).
//! - If the section exists but its content is not valid JSON, the module is
//!   denied access to every bridge function as a safety measure.

use std::collections::HashSet;
use tracing::warn;

/// Permission gate for a loaded WASM module.
///
/// - `allowed_functions == None`  → unrestricted (no `clean:permissions` section found)
/// - `allowed_functions == Some(set)` → only the named functions are permitted
#[derive(Debug, Clone)]
pub struct PermissionGate {
    allowed_functions: Option<HashSet<String>>,
    module_name: String,
}

impl PermissionGate {
    /// Create a gate that allows every function (backward-compatible default).
    pub fn allow_all() -> Self {
        Self {
            allowed_functions: None,
            module_name: String::new(),
        }
    }

    /// Create a gate that restricts calls to exactly the provided set.
    pub fn from_allowlist(functions: HashSet<String>, module_name: String) -> Self {
        Self {
            allowed_functions: Some(functions),
            module_name,
        }
    }

    /// Returns `true` when the named function is permitted.
    pub fn is_allowed(&self, function_name: &str) -> bool {
        match &self.allowed_functions {
            None => true,
            Some(allowed) => allowed.contains(function_name),
        }
    }

    /// Convenience wrapper that logs a `WARN` when access is denied and returns
    /// whether the call should proceed.
    pub fn check(&self, function_name: &str) -> bool {
        if self.is_allowed(function_name) {
            return true;
        }
        warn!(
            "Plugin '{}' attempted to call '{}' without permission — call blocked",
            self.module_name, function_name
        );
        false
    }

    /// Returns `true` when the gate actively restricts functions (i.e. a
    /// `clean:permissions` section was found and successfully parsed).
    pub fn is_enforcing(&self) -> bool {
        self.allowed_functions.is_some()
    }

    /// Returns the number of allowed functions, or `None` when unrestricted.
    pub fn allowed_count(&self) -> Option<usize> {
        self.allowed_functions.as_ref().map(|s| s.len())
    }
}

/// Parse the `clean:permissions` custom section from raw WASM bytes and return
/// the appropriate [`PermissionGate`].
///
/// # Behavior
///
/// | Condition | Result |
/// |-----------|--------|
/// | No `clean:permissions` section | [`PermissionGate::allow_all`] |
/// | Section present, valid JSON array of strings | Restricted gate with those names |
/// | Section present, invalid JSON | Empty allowlist (deny all bridge functions) |
/// | `wasm_bytes` is not a valid WASM binary | [`PermissionGate::allow_all`] (parse stops at first error) |
pub fn parse_permissions(wasm_bytes: &[u8], module_name: &str) -> PermissionGate {
    use wasmparser::{Parser, Payload};

    let parser = Parser::new(0);

    for payload in parser.parse_all(wasm_bytes) {
        match payload {
            Ok(Payload::CustomSection(section)) => {
                if section.name() != "clean:permissions" {
                    continue;
                }

                let data = section.data();
                match serde_json::from_slice::<Vec<String>>(data) {
                    Ok(functions) => {
                        let count = functions.len();
                        let allowed: HashSet<String> = functions.into_iter().collect();
                        tracing::info!(
                            module = module_name,
                            allowed_count = count,
                            "Loaded clean:permissions manifest"
                        );
                        return PermissionGate::from_allowlist(allowed, module_name.to_string());
                    }
                    Err(e) => {
                        tracing::error!(
                            module = module_name,
                            error = %e,
                            "Failed to parse clean:permissions section — denying all bridge access"
                        );
                        // Malformed manifest: deny everything as a safety measure.
                        return PermissionGate::from_allowlist(
                            HashSet::new(),
                            module_name.to_string(),
                        );
                    }
                }
            }
            Ok(_) => {
                // Not a custom section — continue scanning.
                continue;
            }
            Err(_) => {
                // WASM parse error — stop scanning, treat as if no section exists.
                break;
            }
        }
    }

    // No clean:permissions section found → unrestricted (backward-compatible).
    PermissionGate::allow_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid WASM module with no custom sections (magic + version only).
    // This is the smallest legal WASM binary.
    const EMPTY_WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    /// Build a WASM binary that contains a `clean:permissions` custom section
    /// with the given payload bytes appended after the header.
    fn wasm_with_custom_section(section_name: &str, payload: &[u8]) -> Vec<u8> {
        // WASM binary format:
        //   magic (4 bytes) + version (4 bytes)
        //   custom section:
        //     id = 0x00 (1 byte)
        //     section_size (varuint32, LEB128)
        //     name_len (varuint32, LEB128)
        //     name bytes
        //     data bytes
        let name_bytes = section_name.as_bytes();
        let name_len = encode_leb128(name_bytes.len() as u32);
        let section_content_len = name_len.len() + name_bytes.len() + payload.len();
        let section_size = encode_leb128(section_content_len as u32);

        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        wasm.push(0x00); // custom section id
        wasm.extend_from_slice(&section_size);
        wasm.extend_from_slice(&name_len);
        wasm.extend_from_slice(name_bytes);
        wasm.extend_from_slice(payload);
        wasm
    }

    /// Encode an unsigned 32-bit integer as LEB128.
    fn encode_leb128(mut value: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
        bytes
    }

    #[test]
    fn test_allow_all_when_no_section() {
        let gate = parse_permissions(EMPTY_WASM, "test_module");
        assert!(!gate.is_enforcing(), "Should not be enforcing when no section exists");
        assert!(gate.is_allowed("_session_store"), "Should allow any function");
        assert!(gate.is_allowed("_roles_register"), "Should allow any function");
        assert!(gate.check("anything"), "check() should return true when unrestricted");
    }

    #[test]
    fn test_allow_all_constructor() {
        let gate = PermissionGate::allow_all();
        assert!(!gate.is_enforcing());
        assert!(gate.is_allowed("_session_store"));
        assert!(gate.allowed_count().is_none());
    }

    #[test]
    fn test_from_allowlist() {
        let mut set = HashSet::new();
        set.insert("_session_get".to_string());
        set.insert("_session_store".to_string());

        let gate = PermissionGate::from_allowlist(set, "my_plugin".to_string());
        assert!(gate.is_enforcing());
        assert_eq!(gate.allowed_count(), Some(2));
        assert!(gate.is_allowed("_session_get"));
        assert!(gate.is_allowed("_session_store"));
        assert!(!gate.is_allowed("_roles_register"));
        assert!(!gate.is_allowed("_auth_set_session"));
    }

    #[test]
    fn test_parse_valid_permissions_section() {
        let manifest = br#"["_session_get","_session_store","_req_body"]"#;
        let wasm = wasm_with_custom_section("clean:permissions", manifest);

        let gate = parse_permissions(&wasm, "plugin_a");
        assert!(gate.is_enforcing());
        assert_eq!(gate.allowed_count(), Some(3));
        assert!(gate.is_allowed("_session_get"));
        assert!(gate.is_allowed("_session_store"));
        assert!(gate.is_allowed("_req_body"));
        assert!(!gate.is_allowed("_roles_register"));
        assert!(!gate.is_allowed("_auth_set_session"));
    }

    #[test]
    fn test_parse_empty_permissions_array() {
        let manifest = b"[]";
        let wasm = wasm_with_custom_section("clean:permissions", manifest);

        let gate = parse_permissions(&wasm, "locked_plugin");
        assert!(gate.is_enforcing(), "Empty array = enforcing with no permissions");
        assert_eq!(gate.allowed_count(), Some(0));
        assert!(!gate.is_allowed("_session_get"));
        assert!(!gate.is_allowed("_roles_register"));
    }

    #[test]
    fn test_parse_invalid_json_denies_all() {
        let bad_manifest = b"not valid json {{{";
        let wasm = wasm_with_custom_section("clean:permissions", bad_manifest);

        let gate = parse_permissions(&wasm, "broken_plugin");
        assert!(gate.is_enforcing(), "Invalid JSON should trigger deny-all enforcement");
        assert_eq!(gate.allowed_count(), Some(0));
        assert!(!gate.is_allowed("_session_get"));
    }

    #[test]
    fn test_unrelated_custom_section_is_ignored() {
        let wasm = wasm_with_custom_section("name", b"\x00some_debug_info");
        let gate = parse_permissions(&wasm, "debug_module");
        assert!(!gate.is_enforcing(), "Unrelated custom section should not affect permissions");
        assert!(gate.is_allowed("_session_store"));
    }

    #[test]
    fn test_check_logs_warning_and_returns_false() {
        let mut set = HashSet::new();
        set.insert("_session_get".to_string());
        let gate = PermissionGate::from_allowlist(set, "restricted_plugin".to_string());

        assert!(!gate.check("_roles_register"), "Denied function should return false");
        assert!(gate.check("_session_get"), "Allowed function should return true");
    }
}
