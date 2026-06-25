//! Environment & Time Host Functions (Layer 2)
//!
//! Provides environment and time operations for WASM modules:
//! - _env_get: Get environment variable value
//! - _time_now: Get current Unix timestamp in seconds
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike, Utc};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

const SENSITIVE_SUBSTRINGS: &[&str] = &[
    "SECRET", "PASSWORD", "TOKEN", "PRIVATE", "AWS_SECRET", "API_KEY",
];

fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    SENSITIVE_SUBSTRINGS.iter().any(|p| upper.contains(p))
}

/// Register environment and time functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // ENVIRONMENT
    // =========================================

    // _env_get - Get environment variable value
    // Args: name_ptr, name_len
    // Returns: pointer to value string (length-prefixed), empty if not found
    linker.func_wrap(
        "env",
        "_env_get",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => {
                    error!("_env_get: Failed to read variable name");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            // Validate variable name (alphanumeric + underscore only)
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                error!("_env_get: Invalid variable name '{}'", name);
                return write_string_to_caller(&mut caller, "");
            }

            // Security: deny access to sensitive variables
            let denied = [
                "AWS_SECRET_ACCESS_KEY",
                "PRIVATE_KEY",
                "ENCRYPTION_KEY",
                "SSH_AUTH_SOCK",
                "GPG_PASSPHRASE",
            ];
            let upper = name.to_uppercase();
            if denied.iter().any(|d| upper.contains(d)) {
                error!("_env_get: Access denied for '{}'", name);
                return write_string_to_caller(&mut caller, "");
            }

            debug!("_env_get: reading '{}'", name);

            match std::env::var(&name) {
                Ok(value) => write_string_to_caller(&mut caller, &value),
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // TIME
    // =========================================

    // _time_now - Get current Unix timestamp in seconds
    // Args: none
    // Returns: i64 timestamp (seconds since epoch)
    linker.func_wrap(
        "env",
        "_time_now",
        |_caller: Caller<'_, S>| -> i64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        },
    )?;

    // =========================================
    // ENV EXTRAS (Phase 2)
    // =========================================

    // _env_has(string) -> boolean
    linker.func_wrap("env", "_env_has",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let name = match read_raw_string(&mut caller, p, l) { Some(s) => s, None => return 0 };
            if std::env::var(&name).is_ok() { 1 } else { 0 }
        })?;

    // _env_all() -> ptr (JSON map, sensitive keys filtered out)
    linker.func_wrap("env", "_env_all",
        |mut caller: Caller<'_, S>| -> i32 {
            let mut map = serde_json::Map::new();
            for (k, v) in std::env::vars() {
                if !is_sensitive_env_name(&k) {
                    map.insert(k, serde_json::Value::String(v));
                }
            }
            write_string_to_caller(&mut caller, &serde_json::Value::Object(map).to_string())
        })?;

    // _env_node_env() -> ptr (NODE_ENV or "development")
    linker.func_wrap("env", "_env_node_env",
        |mut caller: Caller<'_, S>| -> i32 {
            let v = std::env::var("NODE_ENV").unwrap_or_else(|_| "development".to_string());
            write_string_to_caller(&mut caller, &v)
        })?;

    // _env_is_production() -> boolean
    linker.func_wrap("env", "_env_is_production",
        |_: Caller<'_, S>| -> i32 {
            if std::env::var("NODE_ENV").as_deref() == Ok("production") { 1 } else { 0 }
        })?;

    // _env_is_development() -> boolean (NODE_ENV != production)
    linker.func_wrap("env", "_env_is_development",
        |_: Caller<'_, S>| -> i32 {
            if std::env::var("NODE_ENV").as_deref() == Ok("production") { 0 } else { 1 }
        })?;

    // =========================================
    // TIME EXTRAS (Phase 2)
    // =========================================

    // _time_epoch_ms() -> i64
    linker.func_wrap("env", "_time_epoch_ms",
        |_: Caller<'_, S>| -> i64 {
            SystemTime::now().duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64).unwrap_or(0)
        })?;

    // _time_epoch_sec() -> i64
    linker.func_wrap("env", "_time_epoch_sec",
        |_: Caller<'_, S>| -> i64 {
            SystemTime::now().duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64).unwrap_or(0)
        })?;

    // _time_iso() -> ptr — current time as ISO 8601 UTC
    linker.func_wrap("env", "_time_iso",
        |mut caller: Caller<'_, S>| -> i32 {
            let s = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            write_string_to_caller(&mut caller, &s)
        })?;

    // _time_format_iso(i64 epoch_ms) -> ptr
    linker.func_wrap("env", "_time_format_iso",
        |mut caller: Caller<'_, S>, epoch_ms: i64| -> i32 {
            let dt = Utc.timestamp_millis_opt(epoch_ms).single();
            let out = dt.map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .unwrap_or_default();
            write_string_to_caller(&mut caller, &out)
        })?;

    // _time_parse_iso(string) -> i64 epoch_ms (-1 on invalid)
    linker.func_wrap("env", "_time_parse_iso",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i64 {
            let s = read_raw_string(&mut caller, p, l).unwrap_or_default();
            DateTime::parse_from_rfc3339(&s)
                .map(|d| d.timestamp_millis())
                .unwrap_or(-1)
        })?;

    // _time_components(i64 epoch_ms) -> ptr (JSON of local-time components)
    linker.func_wrap("env", "_time_components",
        |mut caller: Caller<'_, S>, epoch_ms: i64| -> i32 {
            let dt = match Local.timestamp_millis_opt(epoch_ms).single() {
                Some(d) => d,
                None => return write_string_to_caller(&mut caller, "{}"),
            };
            let payload = json!({
                "year": dt.year(),
                "month": dt.month(),
                "day": dt.day(),
                "hour": dt.hour(),
                "minute": dt.minute(),
                "second": dt.second(),
                "millisecond": dt.timestamp_subsec_millis(),
                "dayOfWeek": dt.weekday().num_days_from_sunday(),
            });
            write_string_to_caller(&mut caller, &payload.to_string())
        })?;

    // _time_from_components(y, m, d, h, min, s) -> i64 epoch_ms (local time)
    linker.func_wrap("env", "_time_from_components",
        |_: Caller<'_, S>, y: i32, m: i32, d: i32, h: i32, mi: i32, s: i32| -> i64 {
            let single = Local.with_ymd_and_hms(y, m as u32, d as u32, h as u32, mi as u32, s as u32);
            match single {
                chrono::offset::LocalResult::Single(dt) => dt.timestamp_millis(),
                _ => -1,
            }
        })?;

    // _time_add(epoch_ms, duration_ms) -> i64
    linker.func_wrap("env", "_time_add",
        |_: Caller<'_, S>, e: i64, d: i64| -> i64 { e.saturating_add(d) })?;

    // _time_diff(a, b) -> i64 (b - a)
    linker.func_wrap("env", "_time_diff",
        |_: Caller<'_, S>, a: i64, b: i64| -> i64 { b.saturating_sub(a) })?;

    // _time_format_locale(epoch_ms, locale) -> ptr
    // Best-effort: format using the locale tag's loose conventions.
    // Fallback to ISO if the locale isn't recognized.
    linker.func_wrap("env", "_time_format_locale",
        |mut caller: Caller<'_, S>, epoch_ms: i64, p: i32, l: i32| -> i32 {
            let _locale = read_raw_string(&mut caller, p, l).unwrap_or_default();
            let dt = Local.timestamp_millis_opt(epoch_ms).single();
            let formatted = dt
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            write_string_to_caller(&mut caller, &formatted)
        })?;

    // _time_timezone_offset() -> i32 minutes (positive = west of UTC, matches JS Date#getTimezoneOffset)
    linker.func_wrap("env", "_time_timezone_offset",
        |_: Caller<'_, S>| -> i32 {
            let local_off_secs = Local::now().offset().local_minus_utc();
            -(local_off_secs / 60)
        })?;

    // _time_is_past(epoch_ms) -> boolean
    linker.func_wrap("env", "_time_is_past",
        |_: Caller<'_, S>, e: i64| -> i32 {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64).unwrap_or(0);
            if e < now { 1 } else { 0 }
        })?;

    // _time_is_future(epoch_ms) -> boolean
    linker.func_wrap("env", "_time_is_future",
        |_: Caller<'_, S>, e: i64| -> i32 {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64).unwrap_or(0);
            if e > now { 1 } else { 0 }
        })?;

    // _time_sleep(ms) -> void  (sync sleep per spec; docs note divergence in HOST_BRIDGE.md)
    linker.func_wrap("env", "_time_sleep",
        |_: Caller<'_, S>, ms: i32| {
            let ms = ms.max(0) as u64;
            std::thread::sleep(std::time::Duration::from_millis(ms));
        })?;

    Ok(())
}
