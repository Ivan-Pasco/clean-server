//! Route matching (§1.4.2 step 2).
//!
//! Routes come from the guest: it calls `clean:http/routing.register` during
//! `init`, and the server matches incoming requests against what it registered.
//!
//! Patterns support three segment kinds:
//!
//! - **literal** — `/users`, matches itself.
//! - **parameter** — `/users/:id`, matches one segment and captures it.
//! - **wildcard** — `/static/*path`, matches the rest of the path and captures
//!   it whole.
//!
//! Matching is specificity-ordered rather than registration-ordered: a literal
//! beats a parameter, and a parameter beats a wildcard, at each segment. That
//! makes `/users/me` reachable even when `/users/:id` is registered first,
//! which registration order alone would not guarantee.

use std::cmp::Ordering;

// Re-exported: a route is the routing table's input, so callers building one
// should not have to reach into the guest module for it.
pub use crate::guest::Route;

/// The result of matching a request against the routing table.
#[derive(Debug, PartialEq, Eq)]
pub enum Match {
    /// Dispatch to this handler, with any captured path parameters.
    Found {
        handler_id: u32,
        params: Vec<(String, String)>,
        /// Whether this route wants CSRF validation (§1.7).
        csrf: bool,
    },
    /// The path exists but not for this method. Carries the methods that are
    /// allowed, for the `Allow` header a 405 must include.
    MethodNotAllowed {
        allowed: Vec<String>,
    },
    NotFound,
}

/// One segment of a compiled route pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param(String),
    /// Matches every remaining segment; captures them joined by `/`.
    Wildcard(String),
}

impl Segment {
    /// Lower is more specific. Drives the ordering that makes `/users/me` win
    /// over `/users/:id`.
    fn specificity(&self) -> u8 {
        match self {
            Self::Literal(_) => 0,
            Self::Param(_) => 1,
            Self::Wildcard(_) => 2,
        }
    }
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    method: String,
    segments: Vec<Segment>,
    handler_id: u32,
    csrf: bool,
    /// Kept for diagnostics and for the startup log.
    pattern: String,
}

impl CompiledRoute {
    fn compile(route: &Route) -> Self {
        let segments = split_path(&route.path)
            .map(|seg| {
                if let Some(name) = seg.strip_prefix(':') {
                    Segment::Param(name.to_string())
                } else if let Some(name) = seg.strip_prefix('*') {
                    Segment::Wildcard(name.to_string())
                } else {
                    Segment::Literal(seg.to_string())
                }
            })
            .collect();

        Self {
            method: route.method.to_uppercase(),
            segments,
            handler_id: route.handler_id,
            csrf: route.csrf,
            pattern: route.path.clone(),
        }
    }

    /// Try to match a request path's segments, capturing parameters.
    fn match_path(&self, path_segments: &[&str]) -> Option<Vec<(String, String)>> {
        let mut params = Vec::new();

        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                Segment::Wildcard(name) => {
                    // Consumes everything left, including nothing.
                    let rest = path_segments[i.min(path_segments.len())..].join("/");
                    params.push((name.clone(), rest));
                    return Some(params);
                }
                Segment::Literal(expected) => {
                    if path_segments.get(i) != Some(&expected.as_str()) {
                        return None;
                    }
                }
                Segment::Param(name) => {
                    let value = path_segments.get(i)?;
                    // An empty segment is not a value; `/users/` must not match
                    // `/users/:id` with an empty id.
                    if value.is_empty() {
                        return None;
                    }
                    params.push((name.clone(), (*value).to_string()));
                }
            }
        }

        // Every pattern segment matched; the path must not have extra ones.
        if path_segments.len() == self.segments.len() {
            Some(params)
        } else {
            None
        }
    }

    /// Order two routes by specificity, most specific first.
    fn cmp_specificity(&self, other: &Self) -> Ordering {
        for (a, b) in self.segments.iter().zip(other.segments.iter()) {
            match a.specificity().cmp(&b.specificity()) {
                Ordering::Equal => continue,
                non_equal => return non_equal,
            }
        }
        // A shorter pattern is more specific when one is a prefix of the other,
        // because the longer one must be reaching further with params/wildcards.
        self.segments.len().cmp(&other.segments.len())
    }
}

#[derive(Debug, Default)]
pub struct Router {
    routes: Vec<CompiledRoute>,
    mount: String,
}

impl Router {
    /// Build a router over the guest's registered routes.
    ///
    /// `mount` is the `[server] mount` prefix every guest route sits behind.
    pub fn new(routes: Vec<Route>, mount: &str) -> Self {
        let mut compiled: Vec<CompiledRoute> = routes.iter().map(CompiledRoute::compile).collect();

        // Sort once at startup so matching is a straight scan; the table does
        // not change between reloads.
        compiled.sort_by(|a, b| a.cmp_specificity(b));

        Self {
            routes: compiled,
            mount: normalize_mount(mount),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Registered patterns, for the startup log.
    pub fn patterns(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.routes
            .iter()
            .map(|r| (r.method.as_str(), r.pattern.as_str(), r.handler_id))
    }

    /// Match a request path and method.
    pub fn match_route(&self, method: &str, path: &str) -> Match {
        let Some(rel) = self.strip_mount(path) else {
            return Match::NotFound;
        };
        let segments: Vec<&str> = split_path(rel).collect();
        let method = method.to_uppercase();

        let mut allowed: Vec<String> = Vec::new();

        for route in &self.routes {
            let Some(params) = route.match_path(&segments) else {
                continue;
            };

            let method_matches = route.method == method
                // HEAD is served by the GET handler; the body is dropped when
                // the response is written.
                || (method == "HEAD" && route.method == "GET");

            if method_matches {
                return Match::Found {
                    handler_id: route.handler_id,
                    params,
                    csrf: route.csrf,
                };
            }

            if !allowed.contains(&route.method) {
                allowed.push(route.method.clone());
            }
        }

        if allowed.is_empty() {
            Match::NotFound
        } else {
            allowed.sort();
            Match::MethodNotAllowed { allowed }
        }
    }

    /// Remove the mount prefix, or return None when the path is outside it.
    fn strip_mount<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.mount.is_empty() {
            return Some(path);
        }
        match path.strip_prefix(&self.mount) {
            // `/api` must match `/api` and `/api/x`, but not `/apiary`.
            Some(rest) if rest.is_empty() || rest.starts_with('/') => Some(rest),
            _ => None,
        }
    }
}

/// Split a path into non-empty segments.
///
/// This makes `/a/b`, `/a/b/`, and `a/b` equivalent, which is what keeps a
/// trailing slash from being a separate route.
fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// `"/"` and `""` both mean "no prefix"; anything else keeps a leading slash
/// and drops a trailing one.
fn normalize_mount(mount: &str) -> String {
    let trimmed = mount.trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(method: &str, path: &str, id: u32) -> Route {
        Route {
            method: method.to_string(),
            path: path.to_string(),
            handler_id: id,
            csrf: true,
        }
    }

    fn route_without_csrf(method: &str, path: &str, id: u32) -> Route {
        Route {
            csrf: false,
            ..route(method, path, id)
        }
    }

    fn found(m: Match) -> (u32, Vec<(String, String)>) {
        match m {
            Match::Found {
                handler_id, params, ..
            } => (handler_id, params),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    fn csrf_of(m: Match) -> bool {
        match m {
            Match::Found { csrf, .. } => csrf,
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn matches_the_registered_method_and_path() {
        let r = Router::new(vec![route("GET", "/", 1)], "/");
        assert_eq!(found(r.match_route("GET", "/")).0, 1);
    }

    #[test]
    fn unknown_path_is_not_found() {
        let r = Router::new(vec![route("GET", "/", 1)], "/");
        assert_eq!(r.match_route("GET", "/nope"), Match::NotFound);
    }

    #[test]
    fn known_path_wrong_method_reports_what_is_allowed() {
        let r = Router::new(
            vec![route("GET", "/users", 1), route("POST", "/users", 2)],
            "/",
        );
        match r.match_route("DELETE", "/users") {
            Match::MethodNotAllowed { allowed } => assert_eq!(allowed, vec!["GET", "POST"]),
            other => panic!("expected 405, got {other:?}"),
        }
    }

    #[test]
    fn head_is_served_by_the_get_handler() {
        let r = Router::new(vec![route("GET", "/", 7)], "/");
        assert_eq!(found(r.match_route("HEAD", "/")).0, 7);
    }

    #[test]
    fn method_matching_is_case_insensitive() {
        let r = Router::new(vec![route("GET", "/", 1)], "/");
        assert_eq!(found(r.match_route("get", "/")).0, 1);
    }

    #[test]
    fn trailing_slashes_do_not_change_the_match() {
        let r = Router::new(vec![route("GET", "/users", 1)], "/");
        assert_eq!(found(r.match_route("GET", "/users/")).0, 1);
    }

    #[test]
    fn mount_prefix_is_stripped_before_matching() {
        let r = Router::new(vec![route("GET", "/users", 1)], "/api");
        assert_eq!(found(r.match_route("GET", "/api/users")).0, 1);
        assert_eq!(r.match_route("GET", "/users"), Match::NotFound);
    }

    #[test]
    fn mount_root_matches_the_bare_prefix() {
        let r = Router::new(vec![route("GET", "/", 1)], "/api");
        assert_eq!(found(r.match_route("GET", "/api")).0, 1);
    }

    #[test]
    fn a_mount_prefix_does_not_match_a_longer_sibling_segment() {
        let r = Router::new(vec![route("GET", "/x", 1)], "/api");
        assert_eq!(r.match_route("GET", "/apiary/x"), Match::NotFound);
    }

    // --- dynamic routing ---------------------------------------------------

    #[test]
    fn a_route_wants_csrf_by_default() {
        let r = Router::new(vec![route("POST", "/pay", 1)], "/");
        assert!(csrf_of(r.match_route("POST", "/pay")));
    }

    #[test]
    fn a_route_can_opt_out_of_csrf() {
        // A webhook verifying an HMAC signature has no browser-originated
        // form to protect, and cannot present a token.
        let r = Router::new(vec![route_without_csrf("POST", "/hook", 1)], "/");
        assert!(!csrf_of(r.match_route("POST", "/hook")));
    }

    #[test]
    fn opting_one_route_out_does_not_affect_its_neighbours() {
        let r = Router::new(
            vec![
                route_without_csrf("POST", "/hook", 1),
                route("POST", "/pay", 2),
            ],
            "/",
        );
        assert!(!csrf_of(r.match_route("POST", "/hook")));
        assert!(csrf_of(r.match_route("POST", "/pay")));
    }

    #[test]
    fn a_parameter_segment_captures_its_value() {
        let r = Router::new(vec![route("GET", "/users/:id", 1)], "/");
        let (handler, params) = found(r.match_route("GET", "/users/42"));
        assert_eq!(handler, 1);
        assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn multiple_parameters_are_captured_in_order() {
        let r = Router::new(vec![route("GET", "/orgs/:org/repos/:repo", 1)], "/");
        let (_, params) = found(r.match_route("GET", "/orgs/clean/repos/server"));
        assert_eq!(
            params,
            vec![
                ("org".to_string(), "clean".to_string()),
                ("repo".to_string(), "server".to_string()),
            ]
        );
    }

    #[test]
    fn a_literal_beats_a_parameter_regardless_of_registration_order() {
        // `/users/:id` registered FIRST; `/users/me` must still win.
        let r = Router::new(
            vec![route("GET", "/users/:id", 1), route("GET", "/users/me", 2)],
            "/",
        );
        assert_eq!(found(r.match_route("GET", "/users/me")).0, 2);
        assert_eq!(found(r.match_route("GET", "/users/99")).0, 1);
    }

    #[test]
    fn a_parameter_beats_a_wildcard() {
        let r = Router::new(
            vec![
                route("GET", "/files/*rest", 1),
                route("GET", "/files/:name", 2),
            ],
            "/",
        );
        assert_eq!(found(r.match_route("GET", "/files/readme")).0, 2);
    }

    #[test]
    fn a_wildcard_captures_the_remaining_path() {
        let r = Router::new(vec![route("GET", "/static/*path", 1)], "/");
        let (_, params) = found(r.match_route("GET", "/static/css/site.css"));
        assert_eq!(
            params,
            vec![("path".to_string(), "css/site.css".to_string())]
        );
    }

    #[test]
    fn a_wildcard_matches_an_empty_remainder() {
        let r = Router::new(vec![route("GET", "/static/*path", 1)], "/");
        let (_, params) = found(r.match_route("GET", "/static"));
        assert_eq!(params, vec![("path".to_string(), String::new())]);
    }

    #[test]
    fn a_parameter_does_not_match_across_a_separator() {
        // `:id` is one segment, so `/users/a/b` must not match `/users/:id`.
        let r = Router::new(vec![route("GET", "/users/:id", 1)], "/");
        assert_eq!(r.match_route("GET", "/users/a/b"), Match::NotFound);
    }

    #[test]
    fn a_parameter_does_not_match_a_missing_segment() {
        let r = Router::new(vec![route("GET", "/users/:id", 1)], "/");
        assert_eq!(r.match_route("GET", "/users"), Match::NotFound);
    }

    #[test]
    fn params_are_captured_under_a_mount_prefix() {
        let r = Router::new(vec![route("GET", "/users/:id", 1)], "/api");
        let (_, params) = found(r.match_route("GET", "/api/users/7"));
        assert_eq!(params, vec![("id".to_string(), "7".to_string())]);
    }

    #[test]
    fn a_405_is_still_reported_for_a_parameterised_path() {
        let r = Router::new(vec![route("GET", "/users/:id", 1)], "/");
        match r.match_route("DELETE", "/users/3") {
            Match::MethodNotAllowed { allowed } => assert_eq!(allowed, vec!["GET"]),
            other => panic!("expected 405, got {other:?}"),
        }
    }

    #[test]
    fn percent_encoded_values_are_captured_verbatim() {
        // Decoding is the guest's business; the router must not silently
        // rewrite what it captured.
        let r = Router::new(vec![route("GET", "/search/:q", 1)], "/");
        let (_, params) = found(r.match_route("GET", "/search/a%20b"));
        assert_eq!(params, vec![("q".to_string(), "a%20b".to_string())]);
    }
}
