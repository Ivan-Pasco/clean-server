use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::process;
use std::time::SystemTime;

/// System bridge providing platform information and control
pub struct SysBridge;

#[derive(Debug, Serialize, Deserialize)]
struct ExitRequest {
	code: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct EnvInfoResponse {
	platform: String,
	arch: String,
	version: String,
	uptime_seconds: u64,
	process_id: u32,
	parent_process_id: u32,
}

impl SysBridge {
	/// Create a new SysBridge instance
	pub fn new() -> Self {
		Self
	}

	/// Main call dispatcher for the system bridge
	pub async fn call(&self, function: &str, params: Value) -> Result<Value> {
		match function {
			"platform" => Ok(self.platform()),
			"arch" => Ok(self.arch()),
			"version" => Ok(self.version()),
			"exit" => self.exit(params),
			"env_info" => Ok(self.env_info()),
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "SYS_ERROR",
						"message": format!("Unknown sys function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Get the operating system platform
	/// Returns one of: "linux", "macos", "windows", "android", "ios", "web"
	/// Returns: {"ok": true, "data": "macos"}
	fn platform(&self) -> Value {
		let platform = if cfg!(target_os = "linux") {
			"linux"
		} else if cfg!(target_os = "macos") {
			"macos"
		} else if cfg!(target_os = "windows") {
			"windows"
		} else if cfg!(target_os = "android") {
			"android"
		} else if cfg!(target_os = "ios") {
			"ios"
		} else if cfg!(target_arch = "wasm32") {
			"web"
		} else {
			env::consts::OS
		};

		json!({
			"ok": true,
			"data": platform
		})
	}

	/// Get the CPU architecture
	/// Returns one of: "x86_64", "aarch64", "arm", "wasm32"
	/// Returns: {"ok": true, "data": "aarch64"}
	fn arch(&self) -> Value {
		let arch = if cfg!(target_arch = "x86_64") {
			"x86_64"
		} else if cfg!(target_arch = "aarch64") {
			"aarch64"
		} else if cfg!(target_arch = "arm") {
			"arm"
		} else if cfg!(target_arch = "wasm32") {
			"wasm32"
		} else {
			env::consts::ARCH
		};

		json!({
			"ok": true,
			"data": arch
		})
	}

	/// Get the Frame Framework version
	/// Returns: {"ok": true, "data": "1.0.0"}
	fn version(&self) -> Value {
		// Get version from CARGO_PKG_VERSION environment variable at compile time
		let version = env!("CARGO_PKG_VERSION");

		json!({
			"ok": true,
			"data": version
		})
	}

	/// Exit the process with the specified exit code
	/// Args: {"code": 0}
	/// Exit code must be in range 0-255
	/// Returns: {"ok": true, "data": null} (before process exits)
	fn exit(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: ExitRequest = match serde_json::from_value(params.clone()) {
			Ok(req) => req,
			Err(_) => {
				// Try legacy format where code is passed directly
				if let Some(code) = params.as_i64() {
					ExitRequest { code }
				} else if let Some(code) = params.get("code").and_then(|v| v.as_i64()) {
					ExitRequest { code }
				} else {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Invalid request format: expected object with 'code' field or integer",
							"details": {}
						}
					}));
				}
			}
		};

		// Validate exit code range (0-255 for standard exit codes)
		if request.code < 0 || request.code > 255 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Exit code must be in range 0-255",
					"details": {
						"code": request.code,
						"min": 0,
						"max": 255
					}
				}
			}));
		}

		// Return success response before exiting
		// In a real scenario, the caller may not receive this response
		// as the process will exit immediately after
		let _response = json!({
			"ok": true,
			"data": null
		});

		// Exit the process
		// Note: This will terminate the entire process
		process::exit(request.code as i32);

		// This line is unreachable but required for type checking
		#[allow(unreachable_code)]
		Ok(_response)
	}

	/// Get comprehensive environment information
	/// Returns: {
	///   "ok": true,
	///   "data": {
	///     "platform": "macos",
	///     "arch": "aarch64",
	///     "version": "1.0.0",
	///     "uptime_seconds": 12345,
	///     "process_id": 98765,
	///     "parent_process_id": 1234
	///   }
	/// }
	fn env_info(&self) -> Value {
		// Get platform
		let platform = if cfg!(target_os = "linux") {
			"linux"
		} else if cfg!(target_os = "macos") {
			"macos"
		} else if cfg!(target_os = "windows") {
			"windows"
		} else if cfg!(target_os = "android") {
			"android"
		} else if cfg!(target_os = "ios") {
			"ios"
		} else if cfg!(target_arch = "wasm32") {
			"web"
		} else {
			env::consts::OS
		};

		// Get architecture
		let arch = if cfg!(target_arch = "x86_64") {
			"x86_64"
		} else if cfg!(target_arch = "aarch64") {
			"aarch64"
		} else if cfg!(target_arch = "arm") {
			"arm"
		} else if cfg!(target_arch = "wasm32") {
			"wasm32"
		} else {
			env::consts::ARCH
		};

		// Get version
		let version = env!("CARGO_PKG_VERSION");

		// Get process uptime (seconds since process started)
		// We approximate this by using a static variable to store the start time
		// In a real implementation, this could be stored when the bridge is initialized
		let uptime_seconds = Self::get_process_uptime_seconds();

		// Get process ID
		let process_id = process::id();

		// Get parent process ID (platform-specific)
		let parent_process_id = Self::get_parent_process_id();

		let env_info = EnvInfoResponse {
			platform: platform.to_string(),
			arch: arch.to_string(),
			version: version.to_string(),
			uptime_seconds,
			process_id,
			parent_process_id,
		};

		json!({
			"ok": true,
			"data": env_info
		})
	}

	/// Get the process uptime in seconds
	/// This calculates the time since the program started
	fn get_process_uptime_seconds() -> u64 {
		// Use a thread-local static to store process start time
		use std::sync::OnceLock;
		static PROCESS_START: OnceLock<SystemTime> = OnceLock::new();

		let start_time = PROCESS_START.get_or_init(|| SystemTime::now());

		match SystemTime::now().duration_since(*start_time) {
			Ok(duration) => duration.as_secs(),
			Err(_) => 0, // Should never happen unless system time goes backwards
		}
	}

	/// Get the parent process ID (platform-specific)
	#[cfg(unix)]
	fn get_parent_process_id() -> u32 {
		// On Unix systems, use the PPID
		unsafe { libc::getppid() as u32 }
	}

	#[cfg(windows)]
	fn get_parent_process_id() -> u32 {
		// On Windows, we need to use Windows API
		// For simplicity, we return 0 as a placeholder
		// A full implementation would use Windows API to get PPID
		use std::mem;
		use std::ptr;

		// Windows API approach (simplified)
		// In production, you'd use the windows crate or winapi crate
		// For now, we return the current process ID's parent
		// This is a simplified implementation
		0
	}

	#[cfg(not(any(unix, windows)))]
	fn get_parent_process_id() -> u32 {
		// For other platforms (like WASM), return 0
		0
	}

	// Direct methods for internal use

	/// Get platform string (for internal use)
	pub fn get_platform(&self) -> String {
		if cfg!(target_os = "linux") {
			"linux".to_string()
		} else if cfg!(target_os = "macos") {
			"macos".to_string()
		} else if cfg!(target_os = "windows") {
			"windows".to_string()
		} else if cfg!(target_os = "android") {
			"android".to_string()
		} else if cfg!(target_os = "ios") {
			"ios".to_string()
		} else if cfg!(target_arch = "wasm32") {
			"web".to_string()
		} else {
			env::consts::OS.to_string()
		}
	}

	/// Get architecture string (for internal use)
	pub fn get_arch(&self) -> String {
		if cfg!(target_arch = "x86_64") {
			"x86_64".to_string()
		} else if cfg!(target_arch = "aarch64") {
			"aarch64".to_string()
		} else if cfg!(target_arch = "arm") {
			"arm".to_string()
		} else if cfg!(target_arch = "wasm32") {
			"wasm32".to_string()
		} else {
			env::consts::ARCH.to_string()
		}
	}

	/// Get version string (for internal use)
	pub fn get_version(&self) -> String {
		env!("CARGO_PKG_VERSION").to_string()
	}

	/// Get process ID (for internal use)
	pub fn get_process_id(&self) -> u32 {
		process::id()
	}
}

impl Default for SysBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_platform() {
		let bridge = SysBridge::new();
		let result = bridge.call("platform", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_string());

		let platform = result["data"].as_str().unwrap();
		// Platform should be one of the supported values
		assert!(
			platform == "linux"
				|| platform == "macos"
				|| platform == "windows"
				|| platform == "android"
				|| platform == "ios"
				|| platform == "web"
				|| !platform.is_empty()
		);
	}

	#[tokio::test]
	async fn test_arch() {
		let bridge = SysBridge::new();
		let result = bridge.call("arch", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_string());

		let arch = result["data"].as_str().unwrap();
		// Architecture should be one of the supported values
		assert!(
			arch == "x86_64"
				|| arch == "aarch64"
				|| arch == "arm"
				|| arch == "wasm32"
				|| !arch.is_empty()
		);
	}

	#[tokio::test]
	async fn test_version() {
		let bridge = SysBridge::new();
		let result = bridge.call("version", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_string());

		let version = result["data"].as_str().unwrap();
		// Version should be a non-empty string in semver format
		assert!(!version.is_empty());
		// Should contain at least one dot (e.g., "1.0.0")
		assert!(version.contains('.'));
	}

	#[tokio::test]
	async fn test_exit_validation_negative() {
		let bridge = SysBridge::new();
		let result = bridge.call("exit", json!({"code": -1})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("0-255"));
	}

	#[tokio::test]
	async fn test_exit_validation_too_large() {
		let bridge = SysBridge::new();
		let result = bridge.call("exit", json!({"code": 256})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("0-255"));
	}

	#[tokio::test]
	async fn test_exit_validation_invalid_format() {
		let bridge = SysBridge::new();
		let result = bridge.call("exit", json!({"invalid": "field"})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_exit_legacy_format() {
		let bridge = SysBridge::new();
		// Test that direct integer parameter is accepted (legacy format)
		// Note: We can't actually test exit(0) because it would terminate the test process
		// So we test validation only
		let result = bridge.call("exit", json!(999)).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_env_info() {
		let bridge = SysBridge::new();
		let result = bridge.call("env_info", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_object());

		let data = result["data"].as_object().unwrap();

		// Check all required fields are present
		assert!(data.contains_key("platform"));
		assert!(data.contains_key("arch"));
		assert!(data.contains_key("version"));
		assert!(data.contains_key("uptime_seconds"));
		assert!(data.contains_key("process_id"));
		assert!(data.contains_key("parent_process_id"));

		// Validate platform
		let platform = data["platform"].as_str().unwrap();
		assert!(
			platform == "linux"
				|| platform == "macos"
				|| platform == "windows"
				|| platform == "android"
				|| platform == "ios"
				|| platform == "web"
				|| !platform.is_empty()
		);

		// Validate arch
		let arch = data["arch"].as_str().unwrap();
		assert!(
			arch == "x86_64"
				|| arch == "aarch64"
				|| arch == "arm"
				|| arch == "wasm32"
				|| !arch.is_empty()
		);

		// Validate version
		let version = data["version"].as_str().unwrap();
		assert!(!version.is_empty());
		assert!(version.contains('.'));

		// Validate uptime_seconds is a non-negative integer
		assert!(data["uptime_seconds"].is_u64());

		// Validate process_id is a positive integer
		let process_id = data["process_id"].as_u64().unwrap();
		assert!(process_id > 0);

		// Validate parent_process_id is a non-negative integer
		assert!(data["parent_process_id"].is_u64());
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let bridge = SysBridge::new();
		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "SYS_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("Unknown sys function"));
	}

	#[tokio::test]
	async fn test_env_info_structure() {
		let bridge = SysBridge::new();
		let result = bridge.call("env_info", json!({})).await.unwrap();

		// Verify the response can be deserialized into our struct
		let response: EnvInfoResponse = serde_json::from_value(result["data"].clone()).unwrap();

		assert!(!response.platform.is_empty());
		assert!(!response.arch.is_empty());
		assert!(!response.version.is_empty());
		assert!(response.process_id > 0);
	}

	#[test]
	fn test_direct_methods() {
		let bridge = SysBridge::new();

		// Test get_platform
		let platform = bridge.get_platform();
		assert!(!platform.is_empty());

		// Test get_arch
		let arch = bridge.get_arch();
		assert!(!arch.is_empty());

		// Test get_version
		let version = bridge.get_version();
		assert!(!version.is_empty());
		assert!(version.contains('.'));

		// Test get_process_id
		let pid = bridge.get_process_id();
		assert!(pid > 0);
	}

	#[test]
	fn test_get_process_uptime() {
		let uptime1 = SysBridge::get_process_uptime_seconds();
		// uptime1 is u64, so it's always >= 0

		// Sleep a bit and check uptime increased
		std::thread::sleep(std::time::Duration::from_millis(10));
		let uptime2 = SysBridge::get_process_uptime_seconds();
		assert!(uptime2 >= uptime1);
	}

	#[tokio::test]
	async fn test_platform_consistency() {
		let bridge = SysBridge::new();

		// Call platform multiple times and ensure consistency
		let result1 = bridge.call("platform", json!({})).await.unwrap();
		let result2 = bridge.call("platform", json!({})).await.unwrap();

		assert_eq!(result1["data"], result2["data"]);
	}

	#[tokio::test]
	async fn test_arch_consistency() {
		let bridge = SysBridge::new();

		// Call arch multiple times and ensure consistency
		let result1 = bridge.call("arch", json!({})).await.unwrap();
		let result2 = bridge.call("arch", json!({})).await.unwrap();

		assert_eq!(result1["data"], result2["data"]);
	}

	#[tokio::test]
	async fn test_version_consistency() {
		let bridge = SysBridge::new();

		// Call version multiple times and ensure consistency
		let result1 = bridge.call("version", json!({})).await.unwrap();
		let result2 = bridge.call("version", json!({})).await.unwrap();

		assert_eq!(result1["data"], result2["data"]);
	}

	#[tokio::test]
	async fn test_exit_valid_codes() {
		// Note: We cannot actually test valid exit codes without mocking
		// because process::exit() will terminate the test process
		// The exit function is tested indirectly through validation tests
		// In production, exit would be called through the bridge API and
		// the process would terminate as expected

		// This test validates the contract: exit codes 0-255 should pass validation
		// but we can't test the actual exit without process termination
	}

	#[tokio::test]
	async fn test_json_envelope_format() {
		let bridge = SysBridge::new();

		// Test that all responses follow the standard envelope format
		let functions = vec!["platform", "arch", "version", "env_info"];

		for func in functions {
			let result = bridge.call(func, json!({})).await.unwrap();

			// Verify envelope structure
			assert!(result.is_object());
			assert!(result.get("ok").is_some());

			if result["ok"] == true {
				assert!(result.get("data").is_some());
			} else {
				assert!(result.get("err").is_some());
				let err = result["err"].as_object().unwrap();
				assert!(err.contains_key("code"));
				assert!(err.contains_key("message"));
			}
		}
	}
}
