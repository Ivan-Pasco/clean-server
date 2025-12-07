use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::RwLock;

/// Environment variable bridge with security controls
pub struct EnvBridge {
	/// Allowlist of environment variables that can be accessed
	/// If None, all variables are accessible (use with caution)
	allowlist: RwLock<Option<HashSet<String>>>,
	/// Denylist of environment variables that cannot be accessed
	/// Useful for blocking sensitive system variables
	denylist: RwLock<HashSet<String>>,
	/// Whether setting environment variables is permitted
	allow_set: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetRequest {
	name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetRequest {
	name: String,
	value: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HasRequest {
	name: String,
}

impl EnvBridge {
	/// Create a new EnvBridge with default security settings
	pub fn new() -> Self {
		Self {
			allowlist: RwLock::new(None),
			denylist: RwLock::new(Self::default_denylist()),
			allow_set: false,
		}
	}

	/// Create an EnvBridge that allows all access (development mode)
	pub fn new_unrestricted() -> Self {
		Self {
			allowlist: RwLock::new(None),
			denylist: RwLock::new(HashSet::new()),
			allow_set: true,
		}
	}

	/// Create an EnvBridge with a specific allowlist
	pub fn with_allowlist(allowed: Vec<String>) -> Self {
		let mut set = HashSet::new();
		for key in allowed {
			set.insert(key);
		}

		Self {
			allowlist: RwLock::new(Some(set)),
			denylist: RwLock::new(HashSet::new()),
			allow_set: false,
		}
	}

	/// Default denylist of sensitive environment variables
	fn default_denylist() -> HashSet<String> {
		let mut set = HashSet::new();

		// Sensitive credential variables
		set.insert("AWS_SECRET_ACCESS_KEY".to_string());
		set.insert("AWS_SESSION_TOKEN".to_string());
		set.insert("PRIVATE_KEY".to_string());
		set.insert("ENCRYPTION_KEY".to_string());
		set.insert("MASTER_KEY".to_string());

		// System variables that could leak sensitive info
		set.insert("SSH_AUTH_SOCK".to_string());
		set.insert("GPG_AGENT_INFO".to_string());

		set
	}

	/// Set the allowlist of permitted environment variables
	pub fn set_allowlist(&self, allowed: Vec<String>) {
		let mut set = HashSet::new();
		for key in allowed {
			set.insert(key);
		}
		*self.allowlist.write().unwrap() = Some(set);
	}

	/// Clear the allowlist (allow all variables except denylisted)
	pub fn clear_allowlist(&self) {
		*self.allowlist.write().unwrap() = None;
	}

	/// Add a variable to the denylist
	pub fn add_to_denylist(&self, key: String) {
		self.denylist.write().unwrap().insert(key);
	}

	/// Remove a variable from the denylist
	pub fn remove_from_denylist(&self, key: &str) {
		self.denylist.write().unwrap().remove(key);
	}

	/// Clear the denylist
	pub fn clear_denylist(&self) {
		self.denylist.write().unwrap().clear();
	}

	/// Check if access to a variable is permitted
	fn is_permitted(&self, name: &str) -> bool {
		// Check denylist first
		if self.denylist.read().unwrap().contains(name) {
			return false;
		}

		// Check allowlist if it exists
		if let Some(ref allowlist) = *self.allowlist.read().unwrap() {
			return allowlist.contains(name);
		}

		// No allowlist means all non-denylisted variables are permitted
		true
	}

	/// Validate environment variable name
	fn is_valid_name(name: &str) -> bool {
		if name.is_empty() {
			return false;
		}

		// Environment variable names should only contain alphanumeric characters and underscores
		// and should not start with a digit
		let first_char = name.chars().next().unwrap();
		if first_char.is_ascii_digit() {
			return false;
		}

		name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
	}

	/// Main call dispatcher for the bridge
	pub async fn call(&self, function: &str, params: Value) -> Result<Value> {
		match function {
			"get" => self.get_env(params),
			"set" => self.set_env(params),
			"has" => self.has_env(params),
			"list" | "all" => self.list_env(params),
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "ENV_ERROR",
						"message": format!("Unknown env function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Get an environment variable value
	fn get_env(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: GetRequest = match serde_json::from_value(params.clone()) {
			Ok(req) => req,
			Err(_) => {
				// Try legacy string format for backwards compatibility
				if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
					GetRequest {
						name: name.to_string(),
					}
				} else {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Invalid request format: expected object with 'name' field",
							"details": {}
						}
					}));
				}
			}
		};

		// Validate name
		if !Self::is_valid_name(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": format!("Invalid environment variable name: {}", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Check permissions
		if !self.is_permitted(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "PERMISSION_DENIED",
					"message": format!("Access to environment variable '{}' is not permitted", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Get the value
		match env::var(&request.name) {
			Ok(value) => Ok(json!({
				"ok": true,
				"data": value
			})),
			Err(_) => Ok(json!({
				"ok": false,
				"err": {
					"code": "NOT_FOUND",
					"message": format!("Environment variable '{}' not found", request.name),
					"details": { "name": request.name }
				}
			}))
		}
	}

	/// Set an environment variable value
	fn set_env(&self, params: Value) -> Result<Value> {
		// Check if setting is allowed
		if !self.allow_set {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "PERMISSION_DENIED",
					"message": "Setting environment variables is not permitted",
					"details": {}
				}
			}));
		}

		// Parse request
		let request: SetRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'name' and 'value' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate name
		if !Self::is_valid_name(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": format!("Invalid environment variable name: {}", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Check permissions
		if !self.is_permitted(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "PERMISSION_DENIED",
					"message": format!("Access to environment variable '{}' is not permitted", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Set the value
		env::set_var(&request.name, &request.value);

		Ok(json!({
			"ok": true,
			"data": {
				"name": request.name,
				"value": request.value
			}
		}))
	}

	/// Check if an environment variable exists
	fn has_env(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: HasRequest = match serde_json::from_value(params.clone()) {
			Ok(req) => req,
			Err(_) => {
				// Try legacy string format
				if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
					HasRequest {
						name: name.to_string(),
					}
				} else {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Invalid request format: expected object with 'name' field",
							"details": {}
						}
					}));
				}
			}
		};

		// Validate name
		if !Self::is_valid_name(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": format!("Invalid environment variable name: {}", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Check permissions
		if !self.is_permitted(&request.name) {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "PERMISSION_DENIED",
					"message": format!("Access to environment variable '{}' is not permitted", request.name),
					"details": { "name": request.name }
				}
			}));
		}

		// Check if the variable exists
		let exists = env::var(&request.name).is_ok();

		Ok(json!({
			"ok": true,
			"data": exists
		}))
	}

	/// List all accessible environment variables
	fn list_env(&self, _params: Value) -> Result<Value> {
		let mut result: HashMap<String, String> = HashMap::new();

		// Iterate through all environment variables
		for (key, value) in env::vars() {
			// Only include permitted variables
			if self.is_permitted(&key) {
				result.insert(key, value);
			}
		}

		Ok(json!({
			"ok": true,
			"data": result
		}))
	}

	/// Direct method to get a variable (for internal use)
	pub fn get(&self, name: &str) -> Option<String> {
		if !Self::is_valid_name(name) || !self.is_permitted(name) {
			return None;
		}
		env::var(name).ok()
	}

	/// Direct method to set a variable (for internal use)
	pub fn set(&self, name: &str, value: &str) -> Result<()> {
		if !self.allow_set {
			anyhow::bail!("Setting environment variables is not permitted");
		}

		if !Self::is_valid_name(name) {
			anyhow::bail!("Invalid environment variable name: {}", name);
		}

		if !self.is_permitted(name) {
			anyhow::bail!("Access to environment variable '{}' is not permitted", name);
		}

		env::set_var(name, value);
		Ok(())
	}

	/// Direct method to check if a variable exists (for internal use)
	pub fn has(&self, name: &str) -> bool {
		if !Self::is_valid_name(name) || !self.is_permitted(name) {
			return false;
		}
		env::var(name).is_ok()
	}

	/// Direct method to get all accessible variables (for internal use)
	pub fn all(&self) -> HashMap<String, String> {
		let mut result = HashMap::new();
		for (key, value) in env::vars() {
			if self.is_permitted(&key) {
				result.insert(key, value);
			}
		}
		result
	}
}

impl Default for EnvBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_get_existing_var() {
		let bridge = EnvBridge::new_unrestricted();

		// Set a test variable
		env::set_var("TEST_VAR_GET", "test_value");

		let params = json!({ "name": "TEST_VAR_GET" });
		let result = bridge.call("get", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], "test_value");

		// Cleanup
		env::remove_var("TEST_VAR_GET");
	}

	#[tokio::test]
	async fn test_get_nonexistent_var() {
		let bridge = EnvBridge::new_unrestricted();

		let params = json!({ "name": "NONEXISTENT_VAR_12345" });
		let result = bridge.call("get", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "NOT_FOUND");
	}

	#[tokio::test]
	async fn test_set_var() {
		let bridge = EnvBridge::new_unrestricted();

		let params = json!({
			"name": "TEST_VAR_SET",
			"value": "new_value"
		});
		let result = bridge.call("set", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(env::var("TEST_VAR_SET").unwrap(), "new_value");

		// Cleanup
		env::remove_var("TEST_VAR_SET");
	}

	#[tokio::test]
	async fn test_set_var_permission_denied() {
		let bridge = EnvBridge::new(); // Default doesn't allow set

		let params = json!({
			"name": "TEST_VAR_DENIED",
			"value": "value"
		});
		let result = bridge.call("set", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "PERMISSION_DENIED");
	}

	#[tokio::test]
	async fn test_has_var() {
		let bridge = EnvBridge::new_unrestricted();

		// Set a test variable
		env::set_var("TEST_VAR_HAS", "value");

		let params = json!({ "name": "TEST_VAR_HAS" });
		let result = bridge.call("has", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);

		// Test nonexistent variable
		let params = json!({ "name": "NONEXISTENT_VAR_HAS" });
		let result = bridge.call("has", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);

		// Cleanup
		env::remove_var("TEST_VAR_HAS");
	}

	#[tokio::test]
	async fn test_list_vars() {
		let bridge = EnvBridge::new_unrestricted();

		// Set some test variables
		env::set_var("TEST_LIST_1", "value1");
		env::set_var("TEST_LIST_2", "value2");

		let result = bridge.call("list", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_object());

		let data = result["data"].as_object().unwrap();
		assert_eq!(data.get("TEST_LIST_1").unwrap(), "value1");
		assert_eq!(data.get("TEST_LIST_2").unwrap(), "value2");

		// Cleanup
		env::remove_var("TEST_LIST_1");
		env::remove_var("TEST_LIST_2");
	}

	#[tokio::test]
	async fn test_invalid_var_name() {
		let bridge = EnvBridge::new_unrestricted();

		// Test invalid names
		let invalid_names = vec!["", "123ABC", "TEST-VAR", "TEST VAR", "TEST.VAR"];

		for name in invalid_names {
			let params = json!({ "name": name });
			let result = bridge.call("get", params).await.unwrap();

			assert_eq!(result["ok"], false);
			assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		}
	}

	#[tokio::test]
	async fn test_allowlist() {
		let bridge = EnvBridge::with_allowlist(vec![
			"ALLOWED_VAR".to_string(),
		]);

		// Set test variables
		env::set_var("ALLOWED_VAR", "allowed");
		env::set_var("DENIED_VAR", "denied");

		// Test allowed variable
		let params = json!({ "name": "ALLOWED_VAR" });
		let result = bridge.call("get", params).await.unwrap();
		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], "allowed");

		// Test denied variable
		let params = json!({ "name": "DENIED_VAR" });
		let result = bridge.call("get", params).await.unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "PERMISSION_DENIED");

		// Cleanup
		env::remove_var("ALLOWED_VAR");
		env::remove_var("DENIED_VAR");
	}

	#[tokio::test]
	async fn test_denylist() {
		let bridge = EnvBridge::new_unrestricted();
		bridge.add_to_denylist("SENSITIVE_VAR".to_string());

		// Set test variable
		env::set_var("SENSITIVE_VAR", "secret");

		let params = json!({ "name": "SENSITIVE_VAR" });
		let result = bridge.call("get", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "PERMISSION_DENIED");

		// Cleanup
		env::remove_var("SENSITIVE_VAR");
	}

	#[test]
	fn test_direct_methods() {
		let bridge = EnvBridge::new_unrestricted();

		// Set a test variable
		env::set_var("TEST_DIRECT", "direct_value");

		// Test get
		assert_eq!(bridge.get("TEST_DIRECT"), Some("direct_value".to_string()));

		// Test has
		assert!(bridge.has("TEST_DIRECT"));
		assert!(!bridge.has("NONEXISTENT_DIRECT"));

		// Test set
		assert!(bridge.set("TEST_DIRECT_SET", "new_direct_value").is_ok());
		assert_eq!(env::var("TEST_DIRECT_SET").unwrap(), "new_direct_value");

		// Test all
		let all_vars = bridge.all();
		assert!(all_vars.contains_key("TEST_DIRECT"));

		// Cleanup
		env::remove_var("TEST_DIRECT");
		env::remove_var("TEST_DIRECT_SET");
	}

	#[test]
	fn test_is_valid_name() {
		assert!(EnvBridge::is_valid_name("VALID_VAR"));
		assert!(EnvBridge::is_valid_name("VALID_VAR_123"));
		assert!(EnvBridge::is_valid_name("_VALID"));

		assert!(!EnvBridge::is_valid_name(""));
		assert!(!EnvBridge::is_valid_name("123INVALID"));
		assert!(!EnvBridge::is_valid_name("INVALID-VAR"));
		assert!(!EnvBridge::is_valid_name("INVALID VAR"));
		assert!(!EnvBridge::is_valid_name("INVALID.VAR"));
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let bridge = EnvBridge::new();

		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "ENV_ERROR");
	}
}
