//! Route matching (§1.4.2 step 2).
//!
//! Routes come from the guest: it calls `clean:http/routing.register` during
//! `init`, and the server matches incoming requests against what it registered.
//! M0 matches literal paths only — path parameters (`/users/:id`) land in
//! Phase 2 with the dynamic routing table.

use crate::guest::Route;

/// The result of matching a request against the routing table.
#[derive(Debug, PartialEq, Eq)]
pub enum Match {
    /// Dispatch to this handler.
    Found {
        handler_id: u32,
    },
    /// The path exists but not for this method. Carries the methods that are
    /// allowed, for the `Allow` header a 405 must include.
    MethodNotAllowed {
        allowed: Vec<String>,
    },
    NotFound,
}

#[derive(Debug, Default)]
pub struct Router {
    routes: Vec<Route>,
    mount: String,
}

impl Router {
    /// Build a router over the guest's registered routes.
    ///
    /// `mount` is the `[server] mount` prefix every guest route sits behind.
    pub fn new(routes: Vec<Route>, mount: &str) -> Self {
        Self {
            routes,
            mount: normalize_mount(mount),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// Match a request path and method.
    pub fn match_route(&self, method: &str, path: &str) -> Match {
        let Some(rel) = self.strip_mount(path) else {
            return Match::NotFound;
        };
        let rel = normalize_path(rel);
        let method = method.to_uppercase();

        let mut allowed = Vec::new();
        for route in &self.routes {
            if normalize_path(&route.path) != rel {
                continue;
            }
            if route.method == method {
                return Match::Found {
                    handler_id: route.handler_id,
                };
            }
            // HEAD is served by the GET handler; the response body is dropped
            // when writing the reply.
            if method == "HEAD" && route.method == "GET" {
                return Match::Found {
                    handler_id: route.handler_id,
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

/// Compare paths without being tripped up by a trailing slash.
fn normalize_path(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    if p.is_empty() {
        "/"
    } else {
        p
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
        }
    }

    #[test]
    fn matches_the_registered_method_and_path() {
        let r = Router::new(vec![route("GET", "/", 1)], "/");
        assert_eq!(r.match_route("GET", "/"), Match::Found { handler_id: 1 });
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
        assert_eq!(r.match_route("HEAD", "/"), Match::Found { handler_id: 7 });
    }

    #[test]
    fn method_matching_is_case_insensitive() {
        let r = Router::new(vec![route("GET", "/", 1)], "/");
        assert_eq!(r.match_route("get", "/"), Match::Found { handler_id: 1 });
    }

    #[test]
    fn trailing_slashes_do_not_change_the_match() {
        let r = Router::new(vec![route("GET", "/users", 1)], "/");
        assert_eq!(
            r.match_route("GET", "/users/"),
            Match::Found { handler_id: 1 }
        );
    }

    #[test]
    fn mount_prefix_is_stripped_before_matching() {
        let r = Router::new(vec![route("GET", "/users", 1)], "/api");
        assert_eq!(
            r.match_route("GET", "/api/users"),
            Match::Found { handler_id: 1 }
        );
        // Outside the mount.
        assert_eq!(r.match_route("GET", "/users"), Match::NotFound);
    }

    #[test]
    fn mount_root_matches_the_bare_prefix() {
        let r = Router::new(vec![route("GET", "/", 1)], "/api");
        assert_eq!(r.match_route("GET", "/api"), Match::Found { handler_id: 1 });
    }

    #[test]
    fn a_mount_prefix_does_not_match_a_longer_sibling_segment() {
        let r = Router::new(vec![route("GET", "/x", 1)], "/api");
        // `/apiary` must not be treated as inside `/api`.
        assert_eq!(r.match_route("GET", "/apiary/x"), Match::NotFound);
    }
}
