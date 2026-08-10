//! The reload channel wire protocol (§1.10.2, SRVH-06..08).
//!
//! One message format is shared by the local dev socket and the admin HTTP
//! API, so `cln dev`, `cln reload`, third-party dev tooling and cluster
//! orchestrators all speak the same thing.
//!
//! Schema source of truth:
//! `foundation/02 components/hosts/clean-server/schema/reload-channel.json.md`.
//!
//! Responses are canonical JSON — keys sorted, no insignificant whitespace —
//! because the socket frames messages as newline-delimited JSON and a caller
//! reading them line-by-line cannot tolerate embedded newlines.

use std::time::Instant;

use clean_host_core::DeploymentMode;

/// A request on the reload channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Reload the guest, optionally from a new path.
    ReloadGuest { guest: Option<String> },
    /// Recompose the whole chain from `host.toml`.
    ReloadChain,
    /// Swap exactly one middleware entry (dev-mode only).
    SwapMiddleware {
        target: SwapTarget,
        replacement: String,
    },
}

/// Which middleware entry a swap targets: a component path XOR an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapTarget {
    Component(String),
    Index(u32),
}

/// Why a request could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// The `op` field, when it was readable — the response must echo it back.
    pub op: Option<String>,
}

impl Request {
    /// The `op` name, for echoing in the response.
    pub fn op(&self) -> &'static str {
        match self {
            Self::ReloadGuest { .. } => "reload-guest",
            Self::ReloadChain => "reload-chain",
            Self::SwapMiddleware { .. } => "swap-middleware",
        }
    }

    /// Parse one request from JSON.
    ///
    /// Hand-rolled rather than serde-derived: the protocol is three shapes and
    /// the error messages need to name the offending field precisely, which a
    /// derived error does not do well.
    pub fn parse(body: &str) -> Result<Self, ParseError> {
        let value: serde_json::Value = serde_json::from_str(body).map_err(|e| ParseError {
            message: format!("malformed JSON: {e}"),
            op: None,
        })?;

        let op = value.get("op").and_then(|v| v.as_str()).map(str::to_string);
        let Some(op) = op else {
            return Err(ParseError {
                message: "missing `op`".into(),
                op: None,
            });
        };

        match op.as_str() {
            "reload-guest" => Ok(Self::ReloadGuest {
                guest: value
                    .get("guest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            }),
            "reload-chain" => Ok(Self::ReloadChain),
            "swap-middleware" => {
                let replacement = value
                    .get("replacement")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ParseError {
                        message: "`swap-middleware` requires `replacement`".into(),
                        op: Some(op.clone()),
                    })?
                    .to_string();

                let target = value.get("target").ok_or_else(|| ParseError {
                    message: "`swap-middleware` requires `target`".into(),
                    op: Some(op.clone()),
                })?;

                let component = target.get("component").and_then(|v| v.as_str());
                let index = target.get("index").and_then(|v| v.as_u64());

                let target = match (component, index) {
                    // The schema says component XOR index; accepting both would
                    // leave which one wins undefined.
                    (Some(_), Some(_)) => {
                        return Err(ParseError {
                            message: "`target` must carry `component` or `index`, not both".into(),
                            op: Some(op.clone()),
                        })
                    }
                    (Some(c), None) => SwapTarget::Component(c.to_string()),
                    (None, Some(i)) => SwapTarget::Index(i as u32),
                    (None, None) => {
                        return Err(ParseError {
                            message: "`target` must carry `component` or `index`".into(),
                            op: Some(op.clone()),
                        })
                    }
                };

                Ok(Self::SwapMiddleware {
                    target,
                    replacement,
                })
            }
            other => Err(ParseError {
                message: format!("unknown op `{other}`"),
                op: Some(op.clone()),
            }),
        }
    }
}

/// A response on the reload channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok {
        op: String,
        duration_ms: u64,
        drained_in_flight: u32,
    },
    Error {
        op: String,
        error_code: Option<String>,
        error_message: String,
    },
    Refused {
        op: String,
        reason: String,
    },
}

impl Response {
    pub fn ok(op: &str, started: Instant, drained_in_flight: u32) -> Self {
        Self::Ok {
            op: op.to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
            drained_in_flight,
        }
    }

    pub fn error(op: &str, message: impl Into<String>) -> Self {
        Self::Error {
            op: op.to_string(),
            error_code: None,
            error_message: message.into(),
        }
    }

    pub fn refused(op: &str, reason: impl Into<String>) -> Self {
        Self::Refused {
            op: op.to_string(),
            reason: reason.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Render as canonical JSON: keys sorted, no insignificant whitespace, one
    /// line. The socket is newline-delimited, so a multi-line response would
    /// desynchronise the caller.
    pub fn to_json(&self) -> String {
        match self {
            Self::Ok {
                op,
                duration_ms,
                drained_in_flight,
            } => format!(
                r#"{{"drained-in-flight":{drained_in_flight},"duration-ms":{duration_ms},"op":"{}","status":"ok"}}"#,
                escape(op)
            ),
            Self::Error {
                op,
                error_code,
                error_message,
            } => match error_code {
                Some(code) => format!(
                    r#"{{"error-code":"{}","error-message":"{}","op":"{}","status":"error"}}"#,
                    escape(code),
                    escape(error_message),
                    escape(op)
                ),
                None => format!(
                    r#"{{"error-message":"{}","op":"{}","status":"error"}}"#,
                    escape(error_message),
                    escape(op)
                ),
            },
            Self::Refused { op, reason } => format!(
                r#"{{"op":"{}","reason":"{}","status":"refused"}}"#,
                escape(op),
                escape(reason)
            ),
        }
    }
}

/// Escape a string for embedding in JSON, including the control characters
/// that would otherwise break newline framing.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Whether policy permits this request (SRVH-06).
///
/// Returns the refusal reason, or None when the request may proceed.
pub fn policy_refusal(request: &Request, mode: DeploymentMode) -> Option<String> {
    match request {
        Request::SwapMiddleware { .. } => {
            if mode == DeploymentMode::Production {
                // SRVH-06: per-middleware swap is dev-mode only. Production
                // reload remains the whole-chain path.
                Some("deployment-mode = production".to_string())
            } else {
                // The chain this would mutate does not exist: `[http-chain]` is
                // not in the canonical host.toml schema, and wasi:http/middleware
                // is unavailable in the toolchain. Refusing is honest; silently
                // succeeding would report a swap that never happened.
                Some(
                    "swap-middleware is not implemented: no [http-chain] is configured \
                     (wasi:http/middleware is unavailable in the current toolchain)"
                        .to_string(),
                )
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_reload_guest_parses() {
        let r = Request::parse(r#"{"op":"reload-guest"}"#).unwrap();
        assert_eq!(r, Request::ReloadGuest { guest: None });
        assert_eq!(r.op(), "reload-guest");
    }

    #[test]
    fn reload_guest_carries_an_optional_path() {
        let r = Request::parse(r#"{"op":"reload-guest","guest":"./dist/app.wasm"}"#).unwrap();
        assert_eq!(
            r,
            Request::ReloadGuest {
                guest: Some("./dist/app.wasm".into())
            }
        );
    }

    #[test]
    fn reload_chain_parses() {
        assert_eq!(
            Request::parse(r#"{"op":"reload-chain"}"#).unwrap(),
            Request::ReloadChain
        );
    }

    #[test]
    fn swap_middleware_parses_a_component_target() {
        let r = Request::parse(
            r#"{"op":"swap-middleware","target":{"component":"./a.wasm"},"replacement":"./b.wasm"}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request::SwapMiddleware {
                target: SwapTarget::Component("./a.wasm".into()),
                replacement: "./b.wasm".into()
            }
        );
    }

    #[test]
    fn swap_middleware_parses_an_index_target() {
        let r = Request::parse(
            r#"{"op":"swap-middleware","target":{"index":2},"replacement":"./b.wasm"}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request::SwapMiddleware {
                target: SwapTarget::Index(2),
                replacement: "./b.wasm".into()
            }
        );
    }

    #[test]
    fn a_target_carrying_both_forms_is_rejected() {
        // The schema says XOR; accepting both leaves precedence undefined.
        let err = Request::parse(
            r#"{"op":"swap-middleware","target":{"component":"./a.wasm","index":0},"replacement":"./b.wasm"}"#,
        )
        .unwrap_err();
        assert!(err.message.contains("not both"), "{}", err.message);
    }

    #[test]
    fn a_swap_without_a_target_is_rejected() {
        let err =
            Request::parse(r#"{"op":"swap-middleware","replacement":"./b.wasm"}"#).unwrap_err();
        assert!(err.message.contains("target"), "{}", err.message);
        assert_eq!(err.op.as_deref(), Some("swap-middleware"));
    }

    #[test]
    fn an_unknown_op_is_rejected_but_echoes_the_op() {
        let err = Request::parse(r#"{"op":"nonsense"}"#).unwrap_err();
        assert!(err.message.contains("unknown op"), "{}", err.message);
        assert_eq!(err.op.as_deref(), Some("nonsense"));
    }

    #[test]
    fn malformed_json_is_rejected() {
        let err = Request::parse("{not json").unwrap_err();
        assert!(err.message.contains("malformed JSON"), "{}", err.message);
    }

    #[test]
    fn a_missing_op_is_rejected() {
        let err = Request::parse(r#"{"guest":"./a.wasm"}"#).unwrap_err();
        assert!(err.message.contains("missing `op`"), "{}", err.message);
    }

    // --- responses ---------------------------------------------------------

    #[test]
    fn an_ok_response_has_sorted_keys_and_one_line() {
        let response = Response::Ok {
            op: "reload-guest".into(),
            duration_ms: 42,
            drained_in_flight: 3,
        };
        assert_eq!(
            response.to_json(),
            r#"{"drained-in-flight":3,"duration-ms":42,"op":"reload-guest","status":"ok"}"#
        );
        assert!(!response.to_json().contains('\n'));
    }

    #[test]
    fn an_error_response_renders_its_message() {
        let response = Response::error("reload-guest", "guest not found");
        assert_eq!(
            response.to_json(),
            r#"{"error-message":"guest not found","op":"reload-guest","status":"error"}"#
        );
    }

    #[test]
    fn a_refused_response_renders_its_reason() {
        let response = Response::refused("swap-middleware", "deployment-mode = production");
        assert_eq!(
            response.to_json(),
            r#"{"op":"swap-middleware","reason":"deployment-mode = production","status":"refused"}"#
        );
    }

    #[test]
    fn a_multiline_error_cannot_break_newline_framing() {
        // Startup diagnostics are multi-line; embedding one raw would
        // desynchronise a caller reading the socket line by line.
        let response = Response::error("reload-guest", "line one\nline two\r\n  indented");
        let json = response.to_json();
        assert!(!json.contains('\n'), "{json}");
        assert!(json.contains("\\n"), "{json}");
    }

    #[test]
    fn quotes_in_a_message_are_escaped() {
        let response = Response::error("reload-guest", r#"missing "app.wasm""#);
        let json = response.to_json();
        assert!(json.contains(r#"\"app.wasm\""#), "{json}");
        // Still parses as JSON.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "error");
    }

    #[test]
    fn every_response_shape_is_valid_json() {
        for response in [
            Response::Ok {
                op: "reload-chain".into(),
                duration_ms: 1,
                drained_in_flight: 0,
            },
            Response::error("reload-guest", "boom"),
            Response::refused("swap-middleware", "nope"),
        ] {
            let parsed: serde_json::Value = serde_json::from_str(&response.to_json()).unwrap();
            assert!(parsed["status"].is_string());
            assert!(parsed["op"].is_string());
        }
    }

    // --- policy ------------------------------------------------------------

    #[test]
    fn swap_middleware_is_refused_in_production() {
        // SRVH-06.
        let request = Request::SwapMiddleware {
            target: SwapTarget::Index(0),
            replacement: "./b.wasm".into(),
        };
        let reason = policy_refusal(&request, DeploymentMode::Production).unwrap();
        assert!(reason.contains("production"), "{reason}");
    }

    #[test]
    fn swap_middleware_is_refused_as_unimplemented_outside_production() {
        // No [http-chain] exists to mutate; reporting success would claim a
        // swap that never happened.
        let request = Request::SwapMiddleware {
            target: SwapTarget::Index(0),
            replacement: "./b.wasm".into(),
        };
        let reason = policy_refusal(&request, DeploymentMode::Development).unwrap();
        assert!(reason.contains("not implemented"), "{reason}");
    }

    #[test]
    fn reload_ops_are_permitted_in_every_mode() {
        for mode in [
            DeploymentMode::Development,
            DeploymentMode::Staging,
            DeploymentMode::Production,
        ] {
            assert!(policy_refusal(&Request::ReloadChain, mode).is_none());
            assert!(
                policy_refusal(&Request::ReloadGuest { guest: None }, mode).is_none(),
                "reload must work in production — it is the deploy path"
            );
        }
    }
}
