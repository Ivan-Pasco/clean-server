//! Host-side envelope interfaces (§1.5).
//!
//! Some interfaces can only be implemented by whatever is holding the HTTP
//! connection: setting a cookie needs the response that has not been flushed
//! yet, and writing to a live socket needs the socket. Those live here rather
//! than in a bridge — the split the session and realtime bridge specs call the
//! "envelope".
//!
//! `clean:session/http-envelope` — cookies and CSRF.
//! `clean:realtime/sockets`      — delivery to a live connection.
//!
//! Both are registered into the wasmtime `Linker` alongside the `clean:http/*`
//! surface, and both appear in `host.wit` because the server really implements
//! them.

use crate::config::ServerConfig;

/// Cookie attributes, mirroring `cookie-options` in the session schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookieOptions {
    pub path: Option<String>,
    pub domain: Option<String>,
    pub max_age_secs: Option<u32>,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<SameSite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl SameSite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "lax" => Some(Self::Lax),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Why an envelope call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    NoActiveRequest,
    HeaderLocked,
    InvalidCookieName,
    InvalidCookieValue,
}

impl EnvelopeError {
    pub fn as_wit(self) -> &'static str {
        match self {
            Self::NoActiveRequest => "no-active-request",
            Self::HeaderLocked => "header-locked",
            Self::InvalidCookieName => "invalid-cookie-name",
            Self::InvalidCookieValue => "invalid-cookie-value",
        }
    }
}

/// Apply the server's cookie defaults (§1.5.1) to what the caller asked for.
///
/// HttpOnly is always on; `Secure` follows `[server] cookie-secure`, which
/// derives from TLS when set to `auto`; SameSite falls back to
/// `[server] cookie-samesite`. A caller that explicitly asks for something
/// weaker than the default still gets the default — these are floors, not
/// suggestions.
pub fn apply_defaults(mut options: CookieOptions, config: &ServerConfig) -> CookieOptions {
    // §1.7: HttpOnly always. A cookie readable from script is a different
    // security posture than the one this envelope promises.
    options.http_only = true;

    if config.cookies_are_secure() {
        options.secure = true;
    }

    if options.same_site.is_none() {
        options.same_site = SameSite::parse(&config.cookie_samesite).or(Some(SameSite::Lax));
    }

    if options.path.is_none() {
        options.path = Some("/".to_string());
    }

    options
}

/// Render a `Set-Cookie` header value.
///
/// Returns an error rather than emitting anything when the name or value
/// contains a character that would let a caller inject additional attributes or
/// terminate the header.
pub fn set_cookie_header(
    name: &str,
    value: &str,
    options: &CookieOptions,
) -> Result<String, EnvelopeError> {
    if name.is_empty() || !name.bytes().all(is_valid_cookie_name_byte) {
        return Err(EnvelopeError::InvalidCookieName);
    }
    if !value.bytes().all(is_valid_cookie_value_byte) {
        return Err(EnvelopeError::InvalidCookieValue);
    }

    let mut header = format!("{name}={value}");

    if let Some(path) = &options.path {
        if !path.bytes().all(is_valid_attribute_byte) {
            return Err(EnvelopeError::InvalidCookieValue);
        }
        header.push_str(&format!("; Path={path}"));
    }
    if let Some(domain) = &options.domain {
        if !domain.bytes().all(is_valid_attribute_byte) {
            return Err(EnvelopeError::InvalidCookieValue);
        }
        header.push_str(&format!("; Domain={domain}"));
    }
    if let Some(max_age) = options.max_age_secs {
        header.push_str(&format!("; Max-Age={max_age}"));
    }
    if options.http_only {
        header.push_str("; HttpOnly");
    }
    if options.secure {
        header.push_str("; Secure");
    }
    if let Some(same_site) = options.same_site {
        header.push_str(&format!("; SameSite={}", same_site.as_str()));
        // SameSite=None is only honoured on a secure cookie; browsers reject
        // the pair otherwise, which would silently drop the cookie.
        if same_site == SameSite::None && !options.secure {
            header.push_str("; Secure");
        }
    }

    Ok(header)
}

/// Read one cookie from a request's `Cookie` header.
pub fn read_cookie(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

/// Cookie names are HTTP tokens (RFC 6265 / RFC 7230).
fn is_valid_cookie_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Cookie values exclude control characters, whitespace, quotes, comma,
/// semicolon and backslash — the characters that would end the value or start
/// a new attribute.
fn is_valid_cookie_value_byte(b: u8) -> bool {
    (0x21..=0x7E).contains(&b) && !matches!(b, b'"' | b',' | b';' | b'\\')
}

/// Attribute values must not carry a separator or a line terminator.
fn is_valid_attribute_byte(b: u8) -> bool {
    (0x20..=0x7E).contains(&b) && !matches!(b, b';' | b',')
}

/// Where the CSRF token lives for the active request.
///
/// The spec (session §5) lets the host choose storage — "typically encrypted in
/// the cookie itself, or written into the session payload via the store
/// interface". With no session bridge composed yet, the token is held for the
/// duration of the request and emitted as a cookie, which is the cookie-side
/// half of that choice. When a session bridge is composed, persistence moves to
/// `clean:session/store` so the token survives across requests.
pub const CSRF_COOKIE: &str = "__Host-csrf";

/// The header a client sends its CSRF token back in.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Whether a request must present a valid CSRF token (§1.7).
///
/// Returns the reason it fails, or `None` when the request may proceed. A
/// request is allowed when the method is safe, when no token was ever issued
/// (nothing to forge yet), or when the submitted token matches the cookie.
///
/// The comparison is constant-time: a byte-by-byte early exit would leak the
/// token one character at a time to an attacker who can time responses.
pub fn csrf_rejection(
    method: &str,
    cookie_header: Option<&str>,
    submitted: Option<&str>,
) -> Option<&'static str> {
    if !is_unsafe_method(method) {
        return None;
    }

    let expected = cookie_header.and_then(|c| read_cookie(c, CSRF_COOKIE));
    let Some(expected) = expected else {
        // No token has been issued for this client, so there is nothing to
        // forge against. Requiring one here would break the first POST of any
        // session that never called set-csrf.
        return None;
    };

    match submitted {
        None => Some("missing CSRF token"),
        Some(got) if constant_time_eq(got.as_bytes(), expected.as_bytes()) => None,
        Some(_) => Some("CSRF token mismatch"),
    }
}

/// Compare without leaking where the first difference is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Whether a request method changes state and therefore needs CSRF validation.
pub fn is_unsafe_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH" | "DELETE"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clean_host_core::HostConfig;

    fn config(block: &str) -> ServerConfig {
        let text = format!(
            r#"
[host]
name = "clean-server"
version = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name = "app"
wasm = "./app.wasm"
world = "clean:host/server@0.1"

{block}
"#
        );
        let host = HostConfig::parse(&text, "/srv/host.toml").unwrap();
        ServerConfig::from_host_config(&host).unwrap()
    }

    #[test]
    fn a_plain_cookie_renders_with_defaults_applied() {
        let options = apply_defaults(CookieOptions::default(), &config(""));
        let header = set_cookie_header("sid", "abc123", &options).unwrap();

        assert!(header.starts_with("sid=abc123"), "{header}");
        assert!(header.contains("; HttpOnly"), "{header}");
        assert!(header.contains("; Path=/"), "{header}");
        assert!(header.contains("; SameSite=Lax"), "{header}");
    }

    #[test]
    fn http_only_cannot_be_opted_out_of() {
        // §1.7 makes HttpOnly unconditional; a caller passing false must not
        // be able to produce a script-readable session cookie.
        let options = apply_defaults(
            CookieOptions {
                http_only: false,
                ..Default::default()
            },
            &config(""),
        );
        assert!(options.http_only);
    }

    #[test]
    fn secure_is_derived_from_tls_status() {
        let plain = apply_defaults(CookieOptions::default(), &config(""));
        assert!(!plain.secure, "no TLS, no Secure");

        let proxied = apply_defaults(
            CookieOptions::default(),
            &config("[server]\ntrust-proxy-headers = true"),
        );
        assert!(proxied.secure, "a trusted proxy terminating TLS counts");
    }

    #[test]
    fn the_configured_samesite_default_is_used() {
        let options = apply_defaults(
            CookieOptions::default(),
            &config("[server]\ncookie-samesite = \"strict\""),
        );
        assert_eq!(options.same_site, Some(SameSite::Strict));
    }

    #[test]
    fn an_explicit_samesite_is_respected() {
        let options = apply_defaults(
            CookieOptions {
                same_site: Some(SameSite::None),
                ..Default::default()
            },
            &config(""),
        );
        assert_eq!(options.same_site, Some(SameSite::None));
    }

    #[test]
    fn samesite_none_forces_secure() {
        // Browsers drop SameSite=None without Secure, so emitting the pair
        // would silently lose the cookie.
        let header = set_cookie_header(
            "sid",
            "x",
            &CookieOptions {
                same_site: Some(SameSite::None),
                secure: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(header.contains("Secure"), "{header}");
    }

    #[test]
    fn max_age_and_domain_are_rendered() {
        let header = set_cookie_header(
            "sid",
            "x",
            &CookieOptions {
                domain: Some("example.com".into()),
                max_age_secs: Some(3600),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(header.contains("; Domain=example.com"), "{header}");
        assert!(header.contains("; Max-Age=3600"), "{header}");
    }

    #[test]
    fn a_semicolon_in_the_value_is_refused() {
        // Otherwise a caller could append arbitrary attributes.
        assert_eq!(
            set_cookie_header("sid", "a; Domain=evil.com", &CookieOptions::default()),
            Err(EnvelopeError::InvalidCookieValue)
        );
    }

    #[test]
    fn a_newline_in_the_value_is_refused() {
        // Header injection: a CRLF would terminate the header entirely.
        assert_eq!(
            set_cookie_header("sid", "a\r\nSet-Cookie: admin=1", &CookieOptions::default()),
            Err(EnvelopeError::InvalidCookieValue)
        );
    }

    #[test]
    fn an_invalid_name_is_refused() {
        assert_eq!(
            set_cookie_header("bad name", "x", &CookieOptions::default()),
            Err(EnvelopeError::InvalidCookieName)
        );
        assert_eq!(
            set_cookie_header("", "x", &CookieOptions::default()),
            Err(EnvelopeError::InvalidCookieName)
        );
    }

    #[test]
    fn a_separator_in_an_attribute_is_refused() {
        assert_eq!(
            set_cookie_header(
                "sid",
                "x",
                &CookieOptions {
                    path: Some("/a; HttpOnly".into()),
                    ..Default::default()
                }
            ),
            Err(EnvelopeError::InvalidCookieValue)
        );
    }

    #[test]
    fn reading_a_cookie_finds_it_among_several() {
        let header = "theme=dark; sid=abc123; lang=en";
        assert_eq!(read_cookie(header, "sid"), Some("abc123".to_string()));
        assert_eq!(read_cookie(header, "theme"), Some("dark".to_string()));
        assert_eq!(read_cookie(header, "absent"), None);
    }

    #[test]
    fn reading_a_cookie_tolerates_loose_spacing() {
        assert_eq!(
            read_cookie("  sid = abc123  ", "sid"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn a_safe_method_never_needs_a_token() {
        assert!(csrf_rejection("GET", Some("__Host-csrf=abc"), None).is_none());
        assert!(csrf_rejection("HEAD", Some("__Host-csrf=abc"), None).is_none());
    }

    #[test]
    fn an_unsafe_method_with_no_issued_token_is_allowed() {
        // Nothing has been issued, so there is nothing to forge against;
        // rejecting here would break the first POST of any session.
        assert!(csrf_rejection("POST", None, None).is_none());
    }

    #[test]
    fn an_unsafe_method_with_a_matching_token_is_allowed() {
        assert!(csrf_rejection("POST", Some("__Host-csrf=abc123"), Some("abc123")).is_none());
    }

    #[test]
    fn an_unsafe_method_missing_its_token_is_rejected() {
        assert_eq!(
            csrf_rejection("POST", Some("__Host-csrf=abc123"), None),
            Some("missing CSRF token")
        );
    }

    #[test]
    fn an_unsafe_method_with_a_wrong_token_is_rejected() {
        assert_eq!(
            csrf_rejection("DELETE", Some("__Host-csrf=abc123"), Some("nope")),
            Some("CSRF token mismatch")
        );
    }

    #[test]
    fn token_comparison_does_not_short_circuit_on_length() {
        // A prefix must not be accepted.
        assert_eq!(
            csrf_rejection("POST", Some("__Host-csrf=abc123"), Some("abc")),
            Some("CSRF token mismatch")
        );
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn state_changing_methods_need_csrf() {
        for method in ["POST", "PUT", "PATCH", "DELETE", "post"] {
            assert!(is_unsafe_method(method), "{method}");
        }
        for method in ["GET", "HEAD", "OPTIONS"] {
            assert!(!is_unsafe_method(method), "{method}");
        }
    }
}
