use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc, Local, FixedOffset};
use chrono::format::strftime::StrftimeItems;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::sleep as tokio_sleep;

/// Time bridge providing time-related operations
pub struct TimeBridge;

#[derive(Debug, Serialize, Deserialize)]
struct SleepRequest {
	ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct FormatRequest {
	timestamp: i64,
	format: String,
	#[serde(default)]
	timezone: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParseRequest {
	date_string: String,
	format: String,
	#[serde(default)]
	timezone: Option<String>,
}

impl TimeBridge {
	/// Create a new TimeBridge instance
	pub fn new() -> Self {
		Self
	}

	/// Main call dispatcher for the time bridge
	pub async fn call(&self, function: &str, params: Value) -> Result<Value> {
		match function {
			"now" => Ok(self.now()),
			"sleep" => self.sleep(params).await,
			"timestamp" => Ok(self.timestamp()),
			"format" => self.format(params),
			"parse" => self.parse(params),
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "TIME_ERROR",
						"message": format!("Unknown time function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Get current time in both ISO 8601 format and Unix timestamp
	/// Returns: {"ok": true, "data": {"iso": "2025-11-19T10:30:00.000Z", "epoch": 1732015800000}}
	fn now(&self) -> Value {
		let now = Utc::now();
		let iso = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
		let epoch = now.timestamp_millis();

		json!({
			"ok": true,
			"data": {
				"iso": iso,
				"epoch": epoch
			}
		})
	}

	/// Get current Unix timestamp in milliseconds
	/// Returns: {"ok": true, "data": 1732015800000}
	fn timestamp(&self) -> Value {
		let now = Utc::now();
		let epoch = now.timestamp_millis();

		json!({
			"ok": true,
			"data": epoch
		})
	}

	/// Sleep for specified milliseconds (async operation)
	/// Args: {"ms": 500}
	/// Returns: {"ok": true, "data": null}
	async fn sleep(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: SleepRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'ms' field",
						"details": {}
					}
				}));
			}
		};

		// Validate sleep duration
		if request.ms < 0 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Sleep duration must be non-negative",
					"details": {"ms": request.ms}
				}
			}));
		}

		// Maximum sleep duration: 1 hour (3600000 ms)
		// This prevents indefinite blocking
		if request.ms > 3_600_000 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Sleep duration exceeds maximum allowed (3600000 ms / 1 hour)",
					"details": {"ms": request.ms, "max": 3_600_000}
				}
			}));
		}

		// Perform the sleep
		tokio_sleep(Duration::from_millis(request.ms as u64)).await;

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Format a Unix timestamp (milliseconds) to a string
	/// Args: {"timestamp": 1732015800000, "format": "%Y-%m-%d %H:%M:%S", "timezone": "UTC"}
	/// Supported formats:
	/// - "iso8601" or "ISO8601" - ISO 8601 format
	/// - "rfc2822" or "RFC2822" - RFC 2822 format
	/// - "rfc3339" or "RFC3339" - RFC 3339 format
	/// - Any strftime format string (e.g., "%Y-%m-%d %H:%M:%S")
	/// Returns: {"ok": true, "data": "2025-11-19 10:30:00"}
	fn format(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: FormatRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'timestamp' and 'format' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate format string is not empty
		if request.format.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Format string cannot be empty",
					"details": {}
				}
			}));
		}

		// Parse timezone (default to UTC)
		let timezone = request.timezone.as_deref().unwrap_or("UTC");

		// Convert timestamp to DateTime
		let datetime = match Self::timestamp_to_datetime(request.timestamp, timezone) {
			Ok(dt) => dt,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TIME_ERROR",
						"message": format!("Failed to parse timestamp: {}", e),
						"details": {"timestamp": request.timestamp, "timezone": timezone}
					}
				}));
			}
		};

		// Format the datetime
		let formatted = match Self::format_datetime(&datetime, &request.format) {
			Ok(s) => s,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TIME_ERROR",
						"message": format!("Failed to format datetime: {}", e),
						"details": {"format": request.format}
					}
				}));
			}
		};

		Ok(json!({
			"ok": true,
			"data": formatted
		}))
	}

	/// Parse a date string to Unix timestamp (milliseconds)
	/// Args: {"date_string": "2025-11-19 10:30:00", "format": "%Y-%m-%d %H:%M:%S", "timezone": "UTC"}
	/// Supported formats:
	/// - "iso8601" or "ISO8601" - ISO 8601 format
	/// - "rfc2822" or "RFC2822" - RFC 2822 format
	/// - "rfc3339" or "RFC3339" - RFC 3339 format
	/// - Any strftime format string (e.g., "%Y-%m-%d %H:%M:%S")
	/// Returns: {"ok": true, "data": 1732015800000}
	fn parse(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: ParseRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'date_string' and 'format' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate date string is not empty
		if request.date_string.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Date string cannot be empty",
					"details": {}
				}
			}));
		}

		// Validate format string is not empty
		if request.format.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Format string cannot be empty",
					"details": {}
				}
			}));
		}

		// Parse timezone (default to UTC)
		let timezone = request.timezone.as_deref().unwrap_or("UTC");

		// Parse the date string
		let timestamp = match Self::parse_datetime(&request.date_string, &request.format, timezone) {
			Ok(ts) => ts,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TIME_ERROR",
						"message": format!("Failed to parse date string: {}", e),
						"details": {
							"date_string": request.date_string,
							"format": request.format,
							"timezone": timezone
						}
					}
				}));
			}
		};

		Ok(json!({
			"ok": true,
			"data": timestamp
		}))
	}

	// Helper functions

	/// Convert Unix timestamp (milliseconds) to DateTime with timezone
	fn timestamp_to_datetime(timestamp_ms: i64, timezone: &str) -> Result<DateTime<FixedOffset>> {
		// Convert milliseconds to seconds and nanoseconds
		let secs = timestamp_ms / 1000;
		let nanos = ((timestamp_ms % 1000) * 1_000_000) as u32;

		// Handle potential overflow for very large timestamps
		if secs > i64::MAX / 1000 || secs < i64::MIN / 1000 {
			anyhow::bail!("Timestamp out of valid range");
		}

		// Create DateTime from timestamp (UTC)
		let utc_dt = match DateTime::from_timestamp(secs, nanos) {
			Some(dt) => dt,
			None => anyhow::bail!("Invalid timestamp: out of range"),
		};

		// Convert to NaiveDateTime for timezone processing
		let naive = utc_dt.naive_utc();

		// Apply timezone - convert from UTC to target timezone
		let datetime = match timezone.to_uppercase().as_str() {
			"UTC" => {
				let utc = Utc.from_utc_datetime(&naive);
				utc.with_timezone(&FixedOffset::east_opt(0).unwrap())
			}
			"LOCAL" => {
				// Convert UTC datetime to local timezone
				let utc = Utc.from_utc_datetime(&naive);
				let local = utc.with_timezone(&Local);
				local.with_timezone(local.offset())
			}
			_ => {
				// Try to parse as offset (e.g., "+0200", "-0500")
				if timezone.starts_with('+') || timezone.starts_with('-') {
					let offset = Self::parse_timezone_offset(timezone)?;
					// Convert UTC datetime to the target timezone
					let utc = Utc.from_utc_datetime(&naive);
					utc.with_timezone(&offset)
				} else {
					// Named timezones not supported - would require timezone database
					anyhow::bail!("Named timezones not supported. Use 'UTC', 'LOCAL', or offset format (e.g., '+0200')")
				}
			}
		};

		Ok(datetime)
	}

	/// Format DateTime to string using specified format
	fn format_datetime(datetime: &DateTime<FixedOffset>, format: &str) -> Result<String> {
		match format.to_uppercase().as_str() {
			"ISO8601" => {
				Ok(datetime.to_rfc3339_opts(chrono::SecondsFormat::Millis, false))
			}
			"RFC2822" => {
				Ok(datetime.to_rfc2822())
			}
			"RFC3339" => {
				Ok(datetime.to_rfc3339())
			}
			_ => {
				// Use strftime format
				// Validate format string by attempting to parse it
				if let Err(e) = Self::validate_strftime_format(format) {
					anyhow::bail!("Invalid strftime format: {}", e);
				}

				Ok(datetime.format(format).to_string())
			}
		}
	}

	/// Parse date string to Unix timestamp (milliseconds)
	fn parse_datetime(date_string: &str, format: &str, timezone: &str) -> Result<i64> {
		// Handle standard formats
		let naive = match format.to_uppercase().as_str() {
			"ISO8601" | "RFC3339" => {
				// Parse ISO 8601 / RFC 3339
				let dt = DateTime::parse_from_rfc3339(date_string)
					.map_err(|e| anyhow::anyhow!("Failed to parse ISO8601/RFC3339: {}", e))?;
				return Ok(dt.timestamp_millis());
			}
			"RFC2822" => {
				// Parse RFC 2822
				let dt = DateTime::parse_from_rfc2822(date_string)
					.map_err(|e| anyhow::anyhow!("Failed to parse RFC2822: {}", e))?;
				return Ok(dt.timestamp_millis());
			}
			_ => {
				// Use strftime format
				// Validate format string
				if let Err(e) = Self::validate_strftime_format(format) {
					anyhow::bail!("Invalid strftime format: {}", e);
				}

				NaiveDateTime::parse_from_str(date_string, format)
					.map_err(|e| anyhow::anyhow!("Failed to parse date string: {}", e))?
			}
		};

		// Apply timezone
		let datetime = match timezone.to_uppercase().as_str() {
			"UTC" => {
				Utc.from_utc_datetime(&naive).timestamp_millis()
			}
			"LOCAL" => {
				let local = Local.from_local_datetime(&naive).single()
					.ok_or_else(|| anyhow::anyhow!("Ambiguous local datetime"))?;
				local.timestamp_millis()
			}
			_ => {
				// Try to parse as offset
				if timezone.starts_with('+') || timezone.starts_with('-') {
					let offset = Self::parse_timezone_offset(timezone)?;
					let dt = offset.from_local_datetime(&naive).single()
						.ok_or_else(|| anyhow::anyhow!("Invalid datetime for timezone"))?;
					dt.timestamp_millis()
				} else {
					anyhow::bail!("Named timezones not supported. Use 'UTC', 'LOCAL', or offset format (e.g., '+0200')")
				}
			}
		};

		Ok(datetime)
	}

	/// Parse timezone offset string (e.g., "+0200", "-0530")
	fn parse_timezone_offset(tz_str: &str) -> Result<FixedOffset> {
		let sign = if tz_str.starts_with('+') { 1 } else { -1 };
		let tz_str = &tz_str[1..]; // Remove sign

		// Parse hours and minutes
		let (hours, minutes) = if tz_str.len() == 4 {
			let hours: i32 = tz_str[0..2].parse()
				.map_err(|_| anyhow::anyhow!("Invalid timezone offset: invalid hours"))?;
			let minutes: i32 = tz_str[2..4].parse()
				.map_err(|_| anyhow::anyhow!("Invalid timezone offset: invalid minutes"))?;
			(hours, minutes)
		} else if tz_str.len() == 2 {
			// Just hours (e.g., "+02")
			let hours: i32 = tz_str.parse()
				.map_err(|_| anyhow::anyhow!("Invalid timezone offset: invalid hours"))?;
			(hours, 0)
		} else {
			anyhow::bail!("Invalid timezone offset format. Expected format: +HHMM or -HHMM")
		};

		// Validate ranges
		if hours > 14 || hours < 0 {
			anyhow::bail!("Invalid timezone offset: hours out of range (0-14)");
		}
		if minutes > 59 || minutes < 0 {
			anyhow::bail!("Invalid timezone offset: minutes out of range (0-59)");
		}

		let total_seconds = sign * (hours * 3600 + minutes * 60);

		FixedOffset::east_opt(total_seconds)
			.ok_or_else(|| anyhow::anyhow!("Invalid timezone offset"))
	}

	/// Validate strftime format string
	fn validate_strftime_format(format: &str) -> Result<()> {
		// Try to parse the format string
		// This will catch invalid format specifiers
		let items = StrftimeItems::new(format);

		// Iterate through items to ensure they're all valid
		for item in items {
			match item {
				chrono::format::Item::Error => {
					anyhow::bail!("Invalid format specifier in format string");
				}
				_ => {}
			}
		}

		Ok(())
	}

	// Direct methods for internal use

	/// Get current Unix timestamp in seconds (for internal use)
	pub fn now_seconds(&self) -> i64 {
		Utc::now().timestamp()
	}

	/// Get current Unix timestamp in milliseconds (for internal use)
	pub fn now_millis(&self) -> i64 {
		Utc::now().timestamp_millis()
	}

	/// Get current ISO 8601 formatted time (for internal use)
	pub fn now_iso(&self) -> String {
		Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
	}
}

impl Default for TimeBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_now() {
		let bridge = TimeBridge::new();
		let result = bridge.call("now", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"]["iso"].is_string());
		assert!(result["data"]["epoch"].is_i64());

		// Verify the timestamp is reasonable (within last 24 hours)
		let epoch = result["data"]["epoch"].as_i64().unwrap();
		let now = Utc::now().timestamp_millis();
		assert!((now - epoch).abs() < 86_400_000); // Less than 24 hours difference
	}

	#[tokio::test]
	async fn test_timestamp() {
		let bridge = TimeBridge::new();
		let result = bridge.call("timestamp", json!({})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_i64());

		// Verify the timestamp is reasonable
		let epoch = result["data"].as_i64().unwrap();
		let now = Utc::now().timestamp_millis();
		assert!((now - epoch).abs() < 1000); // Less than 1 second difference
	}

	#[tokio::test]
	async fn test_sleep() {
		let bridge = TimeBridge::new();

		let start = std::time::Instant::now();
		let result = bridge.call("sleep", json!({"ms": 100})).await.unwrap();
		let elapsed = start.elapsed();

		assert_eq!(result["ok"], true);
		assert!(elapsed.as_millis() >= 100);
		assert!(elapsed.as_millis() < 200); // Allow some margin
	}

	#[tokio::test]
	async fn test_sleep_negative() {
		let bridge = TimeBridge::new();
		let result = bridge.call("sleep", json!({"ms": -100})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_sleep_too_long() {
		let bridge = TimeBridge::new();
		let result = bridge.call("sleep", json!({"ms": 4_000_000})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_format_iso8601() {
		let bridge = TimeBridge::new();

		// 2024-11-19 11:30:00 UTC (1732015800000 ms)
		let timestamp: i64 = 1732015800000;

		let result = bridge.call("format", json!({
			"timestamp": timestamp,
			"format": "ISO8601",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let formatted = result["data"].as_str().unwrap();
		assert!(formatted.starts_with("2024-11-19T11:30:00"));
	}

	#[tokio::test]
	async fn test_format_rfc2822() {
		let bridge = TimeBridge::new();

		let timestamp: i64 = 1732015800000;

		let result = bridge.call("format", json!({
			"timestamp": timestamp,
			"format": "RFC2822",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert!(result["data"].is_string());
	}

	#[tokio::test]
	async fn test_format_custom() {
		let bridge = TimeBridge::new();

		let timestamp: i64 = 1732015800000;

		let result = bridge.call("format", json!({
			"timestamp": timestamp,
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_str().unwrap(), "2024-11-19 11:30:00");
	}

	#[tokio::test]
	async fn test_format_with_offset() {
		let bridge = TimeBridge::new();

		let timestamp: i64 = 1732015800000;

		let result = bridge.call("format", json!({
			"timestamp": timestamp,
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "+0200"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		// UTC 11:30 + 2 hours = 13:30
		assert_eq!(result["data"].as_str().unwrap(), "2024-11-19 13:30:00");
	}

	#[tokio::test]
	async fn test_format_invalid_format() {
		let bridge = TimeBridge::new();

		let result = bridge.call("format", json!({
			"timestamp": 1732015800000_i64,
			"format": "",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_format_invalid_timestamp() {
		let bridge = TimeBridge::new();

		// Timestamp way out of range
		let result = bridge.call("format", json!({
			"timestamp": i64::MAX,
			"format": "ISO8601",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "TIME_ERROR");
	}

	#[tokio::test]
	async fn test_parse_iso8601() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "2024-11-19T11:30:00Z",
			"format": "ISO8601",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_i64().unwrap(), 1732015800000);
	}

	#[tokio::test]
	async fn test_parse_rfc3339() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "2024-11-19T11:30:00+00:00",
			"format": "RFC3339",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_i64().unwrap(), 1732015800000);
	}

	#[tokio::test]
	async fn test_parse_custom() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "2024-11-19 11:30:00",
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_i64().unwrap(), 1732015800000);
	}

	#[tokio::test]
	async fn test_parse_with_offset() {
		let bridge = TimeBridge::new();

		// 13:30 with +0200 offset should be 11:30 UTC
		let result = bridge.call("parse", json!({
			"date_string": "2024-11-19 13:30:00",
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "+0200"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_i64().unwrap(), 1732015800000);
	}

	#[tokio::test]
	async fn test_parse_invalid_date_string() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "invalid date",
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "TIME_ERROR");
	}

	#[tokio::test]
	async fn test_parse_empty_string() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "",
			"format": "%Y-%m-%d",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_parse_invalid_format() {
		let bridge = TimeBridge::new();

		let result = bridge.call("parse", json!({
			"date_string": "2025-11-19",
			"format": "",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let bridge = TimeBridge::new();
		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "TIME_ERROR");
	}

	#[tokio::test]
	async fn test_format_parse_roundtrip() {
		let bridge = TimeBridge::new();

		let original_timestamp: i64 = 1732015800000;

		// Format to string
		let format_result = bridge.call("format", json!({
			"timestamp": original_timestamp,
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(format_result["ok"], true);
		let formatted = format_result["data"].as_str().unwrap();

		// Parse back to timestamp
		let parse_result = bridge.call("parse", json!({
			"date_string": formatted,
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(parse_result["ok"], true);
		assert_eq!(parse_result["data"].as_i64().unwrap(), original_timestamp);
	}

	#[test]
	fn test_direct_methods() {
		let bridge = TimeBridge::new();

		// Test now_seconds
		let seconds = bridge.now_seconds();
		let now = Utc::now().timestamp();
		assert!((now - seconds).abs() <= 1);

		// Test now_millis
		let millis = bridge.now_millis();
		let now = Utc::now().timestamp_millis();
		assert!((now - millis).abs() <= 1000);

		// Test now_iso
		let iso = bridge.now_iso();
		assert!(iso.contains("T"));
		assert!(iso.contains("Z"));
	}

	#[test]
	fn test_parse_timezone_offset() {
		// Valid offsets
		assert!(TimeBridge::parse_timezone_offset("+0000").is_ok());
		assert!(TimeBridge::parse_timezone_offset("+0200").is_ok());
		assert!(TimeBridge::parse_timezone_offset("-0500").is_ok());
		assert!(TimeBridge::parse_timezone_offset("+1400").is_ok());
		assert!(TimeBridge::parse_timezone_offset("+05").is_ok());

		// Invalid offsets
		assert!(TimeBridge::parse_timezone_offset("+1500").is_err()); // Hours > 14
		assert!(TimeBridge::parse_timezone_offset("+0060").is_err()); // Minutes > 59
		assert!(TimeBridge::parse_timezone_offset("0200").is_err());  // No sign
		assert!(TimeBridge::parse_timezone_offset("+02000").is_err()); // Too long
	}

	#[test]
	fn test_validate_strftime_format() {
		// Valid formats
		assert!(TimeBridge::validate_strftime_format("%Y-%m-%d").is_ok());
		assert!(TimeBridge::validate_strftime_format("%Y-%m-%d %H:%M:%S").is_ok());
		assert!(TimeBridge::validate_strftime_format("%a %b %d %Y").is_ok());

		// These should work - chrono is pretty lenient
		assert!(TimeBridge::validate_strftime_format("").is_ok()); // Empty format
		assert!(TimeBridge::validate_strftime_format("no format specifiers").is_ok());
	}

	#[tokio::test]
	async fn test_leap_year_handling() {
		let bridge = TimeBridge::new();

		// February 29, 2024 (leap year)
		let result = bridge.call("parse", json!({
			"date_string": "2024-02-29 12:00:00",
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], true);

		// February 29, 2023 (not a leap year) - should fail
		let result = bridge.call("parse", json!({
			"date_string": "2023-02-29 12:00:00",
			"format": "%Y-%m-%d %H:%M:%S",
			"timezone": "UTC"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
	}
}
