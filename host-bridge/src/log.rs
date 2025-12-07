use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::RwLock;
use chrono::Utc;

/// Log levels supported by the bridge
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
	Debug,
	Info,
	Warn,
	Error,
}

impl LogLevel {
	/// Convert string to LogLevel
	pub fn from_str(s: &str) -> Option<Self> {
		match s.to_uppercase().as_str() {
			"DEBUG" => Some(LogLevel::Debug),
			"INFO" => Some(LogLevel::Info),
			"WARN" | "WARNING" => Some(LogLevel::Warn),
			"ERROR" => Some(LogLevel::Error),
			_ => None,
		}
	}

	/// Convert to string representation
	pub fn as_str(&self) -> &'static str {
		match self {
			LogLevel::Debug => "DEBUG",
			LogLevel::Info => "INFO",
			LogLevel::Warn => "WARN",
			LogLevel::Error => "ERROR",
		}
	}
}

/// Log entry structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
	pub timestamp: String,
	pub level: LogLevel,
	pub message: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub data: Option<Value>,
}

impl LogEntry {
	/// Create a new log entry
	pub fn new(level: LogLevel, message: String, data: Option<Value>) -> Self {
		Self {
			timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
			level,
			message,
			data,
		}
	}

	/// Convert to JSON value
	pub fn to_json(&self) -> Value {
		serde_json::to_value(self).unwrap_or_else(|_| {
			json!({
				"timestamp": self.timestamp,
				"level": self.level.as_str(),
				"message": self.message,
			})
		})
	}
}

/// Request format for log functions
#[derive(Debug, Deserialize)]
struct LogRequest {
	message: String,
	#[serde(default)]
	data: Option<Value>,
}

/// Configuration for the log bridge
#[derive(Debug, Clone)]
pub struct LogConfig {
	/// Minimum log level to output
	pub min_level: LogLevel,
	/// Whether to output JSON format
	pub json_output: bool,
	/// Whether to include timestamps
	pub include_timestamp: bool,
	/// Maximum message size (in bytes)
	pub max_message_size: usize,
}

impl Default for LogConfig {
	fn default() -> Self {
		Self {
			min_level: LogLevel::Debug,
			json_output: true,
			include_timestamp: true,
			max_message_size: 1_048_576, // 1MB
		}
	}
}

/// Log bridge providing structured logging capabilities
pub struct LogBridge {
	config: RwLock<LogConfig>,
}

impl LogBridge {
	/// Create a new LogBridge with default configuration
	pub fn new() -> Self {
		Self {
			config: RwLock::new(LogConfig::default()),
		}
	}

	/// Create a new LogBridge with custom configuration
	pub fn with_config(config: LogConfig) -> Self {
		Self {
			config: RwLock::new(config),
		}
	}

	/// Set the minimum log level
	pub fn set_min_level(&self, level: LogLevel) {
		self.config.write().unwrap().min_level = level;
	}

	/// Get the current minimum log level
	pub fn get_min_level(&self) -> LogLevel {
		self.config.read().unwrap().min_level
	}

	/// Set JSON output mode
	pub fn set_json_output(&self, enabled: bool) {
		self.config.write().unwrap().json_output = enabled;
	}

	/// Set timestamp inclusion
	pub fn set_include_timestamp(&self, enabled: bool) {
		self.config.write().unwrap().include_timestamp = enabled;
	}

	/// Set maximum message size
	pub fn set_max_message_size(&self, size: usize) {
		self.config.write().unwrap().max_message_size = size;
	}

	/// Main call dispatcher for the log bridge
	pub fn call(&self, function: &str, params: Value) -> Result<Value> {
		match function {
			"debug" => self.log_bridge_call(LogLevel::Debug, params),
			"info" => self.log_bridge_call(LogLevel::Info, params),
			"warn" => self.log_bridge_call(LogLevel::Warn, params),
			"error" => self.log_bridge_call(LogLevel::Error, params),
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "LOG_ERROR",
						"message": format!("Unknown log function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Internal method to handle log calls from bridge
	fn log_bridge_call(&self, level: LogLevel, params: Value) -> Result<Value> {
		// Parse request - support both old format (string) and new format (object)
		let (message, data) = if params.is_string() {
			// Legacy format: just a string message
			(params.as_str().unwrap_or("").to_string(), None)
		} else if params.is_object() {
			// New format: object with message and optional data
			let request: LogRequest = match serde_json::from_value(params.clone()) {
				Ok(req) => req,
				Err(_) => {
					// Try to extract fields manually for better error handling
					let message = params.get("message")
						.and_then(|v| v.as_str())
						.unwrap_or("")
						.to_string();

					if message.is_empty() {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "VALIDATION_ERROR",
								"message": "Invalid request format: expected object with 'message' field",
								"details": {}
							}
						}));
					}

					let data = params.get("data").cloned();
					LogRequest { message, data }
				}
			};
			(request.message, request.data)
		} else {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Invalid request format: expected string or object with 'message' field",
					"details": {}
				}
			}));
		};

		// Validate message
		if message.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Log message cannot be empty",
					"details": {}
				}
			}));
		}

		// Check message size
		let config = self.config.read().unwrap();
		if message.len() > config.max_message_size {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": format!("Log message exceeds maximum size ({} bytes)", config.max_message_size),
					"details": {
						"max_size": config.max_message_size,
						"actual_size": message.len()
					}
				}
			}));
		}

		// Check if this log level should be output
		if level < config.min_level {
			// Silently ignore logs below min level
			return Ok(json!({
				"ok": true,
				"data": null
			}));
		}

		// Release the read lock before logging
		drop(config);

		// Perform the actual logging
		self.log_internal(level, message, data);

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Internal method to perform actual logging
	fn log_internal(&self, level: LogLevel, message: String, data: Option<Value>) {
		let entry = LogEntry::new(level, message, data);

		let config = self.config.read().unwrap();

		if config.json_output {
			// Output as JSON
			let json_str = serde_json::to_string(&entry).unwrap_or_else(|_| {
				format!(r#"{{"timestamp":"{}","level":"{}","message":"{}"}}"#,
					entry.timestamp, entry.level.as_str(), entry.message)
			});

			// Use eprintln for log output (stderr)
			eprintln!("{}", json_str);
		} else {
			// Output as plain text
			if config.include_timestamp {
				if let Some(data) = &entry.data {
					eprintln!("[{}] {} - {} | data: {}",
						entry.timestamp, entry.level.as_str(), entry.message,
						serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string()));
				} else {
					eprintln!("[{}] {} - {}", entry.timestamp, entry.level.as_str(), entry.message);
				}
			} else {
				if let Some(data) = &entry.data {
					eprintln!("{} - {} | data: {}",
						entry.level.as_str(), entry.message,
						serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string()));
				} else {
					eprintln!("{} - {}", entry.level.as_str(), entry.message);
				}
			}
		}

		// Also output to tracing for integration with other Rust logging
		match level {
			LogLevel::Debug => {
				if let Some(data) = &entry.data {
					tracing::debug!(message = %entry.message, data = %data, "host:log.debug");
				} else {
					tracing::debug!(message = %entry.message, "host:log.debug");
				}
			}
			LogLevel::Info => {
				if let Some(data) = &entry.data {
					tracing::info!(message = %entry.message, data = %data, "host:log.info");
				} else {
					tracing::info!(message = %entry.message, "host:log.info");
				}
			}
			LogLevel::Warn => {
				if let Some(data) = &entry.data {
					tracing::warn!(message = %entry.message, data = %data, "host:log.warn");
				} else {
					tracing::warn!(message = %entry.message, "host:log.warn");
				}
			}
			LogLevel::Error => {
				if let Some(data) = &entry.data {
					tracing::error!(message = %entry.message, data = %data, "host:log.error");
				} else {
					tracing::error!(message = %entry.message, "host:log.error");
				}
			}
		}
	}

	// Direct methods for internal Rust use (not part of bridge API)

	/// Log a debug message (internal use)
	pub fn debug(&self, message: &str) {
		self.log_internal(LogLevel::Debug, message.to_string(), None);
	}

	/// Log a debug message with data (internal use)
	pub fn debug_with_data(&self, message: &str, data: Value) {
		self.log_internal(LogLevel::Debug, message.to_string(), Some(data));
	}

	/// Log an info message (internal use)
	pub fn info(&self, message: &str) {
		self.log_internal(LogLevel::Info, message.to_string(), None);
	}

	/// Log an info message with data (internal use)
	pub fn info_with_data(&self, message: &str, data: Value) {
		self.log_internal(LogLevel::Info, message.to_string(), Some(data));
	}

	/// Log a warning message (internal use)
	pub fn warn(&self, message: &str) {
		self.log_internal(LogLevel::Warn, message.to_string(), None);
	}

	/// Log a warning message with data (internal use)
	pub fn warn_with_data(&self, message: &str, data: Value) {
		self.log_internal(LogLevel::Warn, message.to_string(), Some(data));
	}

	/// Log an error message (internal use)
	pub fn error(&self, message: &str) {
		self.log_internal(LogLevel::Error, message.to_string(), None);
	}

	/// Log an error message with data (internal use)
	pub fn error_with_data(&self, message: &str, data: Value) {
		self.log_internal(LogLevel::Error, message.to_string(), Some(data));
	}
}

impl Default for LogBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_log_level_ordering() {
		assert!(LogLevel::Debug < LogLevel::Info);
		assert!(LogLevel::Info < LogLevel::Warn);
		assert!(LogLevel::Warn < LogLevel::Error);
	}

	#[test]
	fn test_log_level_from_str() {
		assert_eq!(LogLevel::from_str("DEBUG"), Some(LogLevel::Debug));
		assert_eq!(LogLevel::from_str("debug"), Some(LogLevel::Debug));
		assert_eq!(LogLevel::from_str("INFO"), Some(LogLevel::Info));
		assert_eq!(LogLevel::from_str("WARN"), Some(LogLevel::Warn));
		assert_eq!(LogLevel::from_str("WARNING"), Some(LogLevel::Warn));
		assert_eq!(LogLevel::from_str("ERROR"), Some(LogLevel::Error));
		assert_eq!(LogLevel::from_str("INVALID"), None);
	}

	#[test]
	fn test_log_entry_creation() {
		let entry = LogEntry::new(LogLevel::Info, "test message".to_string(), None);
		assert_eq!(entry.level, LogLevel::Info);
		assert_eq!(entry.message, "test message");
		assert!(entry.data.is_none());
		assert!(!entry.timestamp.is_empty());
	}

	#[test]
	fn test_log_entry_with_data() {
		let data = json!({"userId": 123, "action": "login"});
		let entry = LogEntry::new(LogLevel::Info, "User logged in".to_string(), Some(data.clone()));
		assert_eq!(entry.level, LogLevel::Info);
		assert_eq!(entry.message, "User logged in");
		assert_eq!(entry.data, Some(data));
	}

	#[test]
	fn test_log_entry_to_json() {
		let entry = LogEntry::new(LogLevel::Info, "test".to_string(), None);
		let json = entry.to_json();
		assert!(json.is_object());
		assert_eq!(json["level"], "INFO");
		assert_eq!(json["message"], "test");
	}

	#[test]
	fn test_log_bridge_creation() {
		let bridge = LogBridge::new();
		assert_eq!(bridge.get_min_level(), LogLevel::Debug);
	}

	#[test]
	fn test_log_bridge_custom_config() {
		let config = LogConfig {
			min_level: LogLevel::Warn,
			json_output: false,
			include_timestamp: false,
			max_message_size: 1024,
		};
		let bridge = LogBridge::with_config(config);
		assert_eq!(bridge.get_min_level(), LogLevel::Warn);
	}

	#[test]
	fn test_set_min_level() {
		let bridge = LogBridge::new();
		bridge.set_min_level(LogLevel::Error);
		assert_eq!(bridge.get_min_level(), LogLevel::Error);
	}

	#[test]
	fn test_debug_call() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": "Debug message"
		});
		let result = bridge.call("debug", params).unwrap();
		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], json!(null));
	}

	#[test]
	fn test_info_call() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": "Info message"
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_warn_call() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": "Warning message"
		});
		let result = bridge.call("warn", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_error_call() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": "Error message"
		});
		let result = bridge.call("error", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_log_with_data() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": "User action",
			"data": {
				"userId": 123,
				"action": "login",
				"timestamp": 1700000000
			}
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_legacy_string_format() {
		let bridge = LogBridge::new();
		let params = json!("Simple string message");
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_empty_message_error() {
		let bridge = LogBridge::new();
		let params = json!({
			"message": ""
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("empty"));
	}

	#[test]
	fn test_invalid_params_error() {
		let bridge = LogBridge::new();
		let params = json!(123); // Invalid: not string or object
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[test]
	fn test_missing_message_field() {
		let bridge = LogBridge::new();
		let params = json!({
			"data": {"key": "value"}
			// Missing "message" field
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[test]
	fn test_message_size_limit() {
		let bridge = LogBridge::new();
		bridge.set_max_message_size(100);

		// Message within limit
		let params = json!({
			"message": "Short message"
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);

		// Message exceeding limit
		let long_message = "a".repeat(200);
		let params = json!({
			"message": long_message
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("exceeds maximum size"));
	}

	#[test]
	fn test_log_level_filtering() {
		let bridge = LogBridge::new();
		bridge.set_min_level(LogLevel::Warn);

		// Debug should be filtered out
		let result = bridge.call("debug", json!({"message": "Debug"})).unwrap();
		assert_eq!(result["ok"], true); // Succeeds but doesn't output

		// Info should be filtered out
		let result = bridge.call("info", json!({"message": "Info"})).unwrap();
		assert_eq!(result["ok"], true); // Succeeds but doesn't output

		// Warn should pass through
		let result = bridge.call("warn", json!({"message": "Warning"})).unwrap();
		assert_eq!(result["ok"], true);

		// Error should pass through
		let result = bridge.call("error", json!({"message": "Error"})).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_unknown_function() {
		let bridge = LogBridge::new();
		let result = bridge.call("unknown", json!({"message": "test"})).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "LOG_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("Unknown log function"));
	}

	#[test]
	fn test_direct_methods() {
		let bridge = LogBridge::new();

		// These should not panic
		bridge.debug("Debug message");
		bridge.info("Info message");
		bridge.warn("Warning message");
		bridge.error("Error message");
	}

	#[test]
	fn test_direct_methods_with_data() {
		let bridge = LogBridge::new();
		let data = json!({"key": "value"});

		// These should not panic
		bridge.debug_with_data("Debug", data.clone());
		bridge.info_with_data("Info", data.clone());
		bridge.warn_with_data("Warning", data.clone());
		bridge.error_with_data("Error", data.clone());
	}

	#[test]
	fn test_concurrent_logging() {
		use std::sync::Arc;
		use std::thread;

		let bridge = Arc::new(LogBridge::new());
		let mut handles = vec![];

		for i in 0..10 {
			let bridge_clone = Arc::clone(&bridge);
			let handle = thread::spawn(move || {
				let params = json!({
					"message": format!("Message from thread {}", i)
				});
				bridge_clone.call("info", params).unwrap();
			});
			handles.push(handle);
		}

		for handle in handles {
			handle.join().unwrap();
		}
	}

	#[test]
	fn test_unicode_messages() {
		let bridge = LogBridge::new();

		let params = json!({
			"message": "Hello 世界 🌍 Привет",
			"data": {
				"emoji": "🚀",
				"unicode": "日本語"
			}
		});

		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_special_characters() {
		let bridge = LogBridge::new();

		let params = json!({
			"message": "Special chars: \"quotes\" 'apostrophes' \n newlines \t tabs \\ backslashes"
		});

		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_nested_data_structures() {
		let bridge = LogBridge::new();

		let params = json!({
			"message": "Complex data",
			"data": {
				"user": {
					"id": 123,
					"name": "John",
					"roles": ["admin", "user"],
					"metadata": {
						"created": "2024-01-01",
						"lastLogin": "2024-11-19"
					}
				},
				"action": "login",
				"success": true
			}
		});

		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_config_changes() {
		let bridge = LogBridge::new();

		// Test JSON output toggle
		bridge.set_json_output(true);
		let result = bridge.call("info", json!({"message": "test"})).unwrap();
		assert_eq!(result["ok"], true);

		bridge.set_json_output(false);
		let result = bridge.call("info", json!({"message": "test"})).unwrap();
		assert_eq!(result["ok"], true);

		// Test timestamp toggle
		bridge.set_include_timestamp(true);
		let result = bridge.call("info", json!({"message": "test"})).unwrap();
		assert_eq!(result["ok"], true);

		bridge.set_include_timestamp(false);
		let result = bridge.call("info", json!({"message": "test"})).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_large_data_objects() {
		let bridge = LogBridge::new();

		// Create a large data structure
		let mut data = json!({});
		for i in 0..100 {
			data[format!("field_{}", i)] = json!(format!("value_{}", i));
		}

		let params = json!({
			"message": "Large data object",
			"data": data
		});

		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);
	}

	#[test]
	fn test_max_message_size_boundary() {
		let bridge = LogBridge::new();
		bridge.set_max_message_size(1000);

		// Test exactly at boundary
		let message = "a".repeat(1000);
		let params = json!({
			"message": message
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], true);

		// Test one byte over boundary
		let message = "a".repeat(1001);
		let params = json!({
			"message": message
		});
		let result = bridge.call("info", params).unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}
}
