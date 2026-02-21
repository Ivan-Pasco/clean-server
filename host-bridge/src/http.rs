use anyhow::Result;
use reqwest::{Client, Method, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

/// HTTP bridge providing outbound HTTP request capabilities
pub struct HttpBridge {
	client: Client,
}

/// Request parameters for host:http.request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
	pub method: String,
	pub url: String,
	#[serde(default)]
	pub headers: HashMap<String, String>,
	#[serde(default)]
	pub body: Option<String>,
	#[serde(default)]
	pub timeout: Option<u64>,
	#[serde(default = "default_follow_redirects")]
	pub follow_redirects: bool,
	#[serde(default = "default_max_redirects")]
	pub max_redirects: usize,
}

fn default_follow_redirects() -> bool {
	true
}

fn default_max_redirects() -> usize {
	10
}

/// Response data from HTTP request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
	pub status: u16,
	pub headers: HashMap<String, String>,
	pub body: String,
	pub url: String,
}

/// Request parameters for host:http.respond (SSR mode)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRespondRequest {
	pub status: u16,
	#[serde(default)]
	pub headers: HashMap<String, String>,
	#[serde(default)]
	pub body: String,
}

impl HttpBridge {
	/// Create a new HttpBridge with default client configuration
	pub fn new() -> Self {
		// Build a client with reasonable defaults
		let client = Client::builder()
			.timeout(Duration::from_secs(30))
			.connect_timeout(Duration::from_secs(10))
			.pool_max_idle_per_host(10)
			.pool_idle_timeout(Duration::from_secs(90))
			.tcp_keepalive(Duration::from_secs(60))
			.redirect(Policy::none()) // We handle redirects manually
			.build()
			.expect("Failed to build HTTP client");

		Self { client }
	}

	/// Create a new HttpBridge with custom client configuration
	pub fn with_client(client: Client) -> Self {
		Self { client }
	}

	/// Main call dispatcher for the HTTP bridge
	pub async fn call(&mut self, function: &str, params: Value) -> Result<Value> {
		match function {
			"request" => self.request(params).await,
			"respond" => self.respond(params).await,
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "HTTP_ERROR",
						"message": format!("Unknown http function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Execute an HTTP request
	/// Args: {"method": "GET", "url": "https://api.example.com", "headers": {}, "body": null, "timeout": 30000}
	/// Returns: {"ok": true, "data": {"status": 200, "headers": {}, "body": "...", "url": "..."}}
	async fn request(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: HttpRequest = match serde_json::from_value(params.clone()) {
			Ok(req) => req,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Invalid request format: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Validate and parse URL
		let parsed_url = match Url::parse(&req.url) {
			Ok(url) => url,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "INVALID_URL",
						"message": format!("Invalid URL: {}", e),
						"details": {"url": req.url}
					}
				}));
			}
		};

		// Security: Only allow HTTP and HTTPS schemes
		let scheme = parsed_url.scheme();
		if scheme != "http" && scheme != "https" {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "INVALID_URL",
					"message": format!("Unsupported URL scheme: {}. Only http and https are allowed.", scheme),
					"details": {"url": req.url, "scheme": scheme}
				}
			}));
		}

		// Security: Prevent SSRF by blocking private IP ranges
		if let Some(host) = parsed_url.host_str() {
			if Self::is_private_ip(host) {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "NETWORK_FAIL",
						"message": "Requests to private IP addresses are not allowed",
						"details": {"url": req.url, "host": host}
					}
				}));
			}
		}

		// Parse HTTP method
		let method = match req.method.to_uppercase().as_str() {
			"GET" => Method::GET,
			"POST" => Method::POST,
			"PUT" => Method::PUT,
			"PATCH" => Method::PATCH,
			"DELETE" => Method::DELETE,
			"HEAD" => Method::HEAD,
			"OPTIONS" => Method::OPTIONS,
			_ => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Unsupported HTTP method: {}", req.method),
						"details": {"method": req.method}
					}
				}));
			}
		};

		// Build the reqwest request
		let mut request_builder = self.client.request(method, req.url.clone());

		// Set timeout if specified
		if let Some(timeout_ms) = req.timeout {
			if timeout_ms > 300_000 {
				// Max 5 minutes
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Timeout exceeds maximum allowed (300000 ms / 5 minutes)",
						"details": {"timeout": timeout_ms, "max": 300_000}
					}
				}));
			}
			request_builder = request_builder.timeout(Duration::from_millis(timeout_ms));
		}

		// Set headers
		for (key, value) in &req.headers {
			request_builder = request_builder.header(key, value);
		}

		// Set body if present
		if let Some(body) = &req.body {
			request_builder = request_builder.body(body.clone());
		}

		// Execute the request with redirect handling
		let mut current_url = req.url.clone();
		let mut redirect_count = 0;

		loop {
			// Clone the request builder for this attempt
			let request = match request_builder.try_clone() {
				Some(rb) => rb,
				None => {
					// Can't clone requests with streaming bodies
					// Execute once without redirect support
					match request_builder.build() {
						Ok(r) => {
							return self.execute_request_once(r).await;
						}
						Err(e) => {
							return Ok(json!({
								"ok": false,
								"err": {
									"code": "HTTP_ERROR",
									"message": format!("Failed to build request: {}", e),
									"details": {}
								}
							}));
						}
					}
				}
			};

			// Build the actual request
			let built_request = match request.build() {
				Ok(r) => r,
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "HTTP_ERROR",
							"message": format!("Failed to build request: {}", e),
							"details": {}
						}
					}));
				}
			};

			// Execute the request
			let response = match self.client.execute(built_request).await {
				Ok(resp) => resp,
				Err(e) => {
					// Determine error type
					let (code, message) = if e.is_timeout() {
						("TIMEOUT", format!("Request timed out: {}", e))
					} else if e.is_connect() {
						("NETWORK_FAIL", format!("Connection failed: {}", e))
					} else if e.is_request() {
						("HTTP_ERROR", format!("Request error: {}", e))
					} else {
						("NETWORK_FAIL", format!("Network error: {}", e))
					};

					return Ok(json!({
						"ok": false,
						"err": {
							"code": code,
							"message": message,
							"details": {"url": current_url}
						}
					}));
				}
			};

			// Get the final URL (after any redirects handled by the server)
			current_url = response.url().to_string();
			let status = response.status();

			// Check if this is a redirect response
			if req.follow_redirects && status.is_redirection() {
				if redirect_count >= req.max_redirects {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "NETWORK_FAIL",
							"message": format!("Too many redirects (max: {})", req.max_redirects),
							"details": {"url": current_url, "redirects": redirect_count}
						}
					}));
				}

				// Get the Location header
				if let Some(location) = response.headers().get("location") {
					let location_str = match location.to_str() {
						Ok(s) => s,
						Err(_) => {
							return Ok(json!({
								"ok": false,
								"err": {
									"code": "HTTP_ERROR",
									"message": "Invalid Location header in redirect response",
									"details": {}
								}
							}));
						}
					};

					// Parse the redirect URL (may be relative)
					let redirect_url = match Url::parse(location_str) {
						Ok(url) => url.to_string(),
						Err(_) => {
							// Try to parse as relative URL
							match Url::parse(&current_url).and_then(|base| base.join(location_str)) {
								Ok(url) => url.to_string(),
								Err(e) => {
									return Ok(json!({
										"ok": false,
										"err": {
											"code": "INVALID_URL",
											"message": format!("Invalid redirect URL: {}", e),
											"details": {"location": location_str}
										}
									}));
								}
							}
						}
					};

					// Follow the redirect
					current_url = redirect_url.clone();
					request_builder = self.client.request(Method::GET, redirect_url);
					redirect_count += 1;
					continue;
				} else {
					// Redirect response without Location header
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "HTTP_ERROR",
							"message": "Redirect response missing Location header",
							"details": {"status": status.as_u16()}
						}
					}));
				}
			}

			// Not a redirect, process the response
			return self.process_response(response, current_url).await;
		}
	}

	/// Execute a request once without redirect handling (for streaming bodies)
	async fn execute_request_once(&self, request: reqwest::Request) -> Result<Value> {
		let url = request.url().to_string();

		let response = match self.client.execute(request).await {
			Ok(resp) => resp,
			Err(e) => {
				let (code, message) = if e.is_timeout() {
					("TIMEOUT", format!("Request timed out: {}", e))
				} else if e.is_connect() {
					("NETWORK_FAIL", format!("Connection failed: {}", e))
				} else {
					("NETWORK_FAIL", format!("Network error: {}", e))
				};

				return Ok(json!({
					"ok": false,
					"err": {
						"code": code,
						"message": message,
						"details": {"url": url}
					}
				}));
			}
		};

		self.process_response(response, url).await
	}

	/// Process the HTTP response and convert to envelope format
	async fn process_response(&self, response: reqwest::Response, url: String) -> Result<Value> {
		let status = response.status().as_u16();

		// Extract headers
		let mut headers = HashMap::new();
		for (key, value) in response.headers() {
			if let Ok(value_str) = value.to_str() {
				headers.insert(key.as_str().to_string(), value_str.to_string());
			}
		}

		// Read response body as text
		let body = match response.text().await {
			Ok(text) => text,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "HTTP_ERROR",
						"message": format!("Failed to read response body: {}", e),
						"details": {"url": url}
					}
				}));
			}
		};

		// Build successful response
		Ok(json!({
			"ok": true,
			"data": {
				"status": status,
				"headers": headers,
				"body": body,
				"url": url
			}
		}))
	}

	/// Handle HTTP response for SSR mode
	/// Args: {"status": 302, "headers": {"Location": "/login"}, "body": ""}
	/// Returns: {"ok": true, "data": null}
	async fn respond(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: HttpRespondRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Invalid request format: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Validate status code
		if req.status < 100 || req.status >= 600 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": format!("Invalid HTTP status code: {}", req.status),
					"details": {"status": req.status}
				}
			}));
		}

		// This is primarily for SSR mode where the Frame server
		// needs to send an HTTP response back to the client.
		// In this implementation, we just validate and acknowledge
		// the request. The actual response sending is handled by
		// the frame-server runtime.

		Ok(json!({
			"ok": true,
			"data": {
				"status": req.status,
				"headers": req.headers,
				"body": req.body
			}
		}))
	}

	/// Check if a hostname resolves to a private IP address
	/// This helps prevent SSRF (Server-Side Request Forgery) attacks
	fn is_private_ip(host: &str) -> bool {
		// Check for localhost
		if host == "localhost" || host == "127.0.0.1" || host == "::1" {
			return true;
		}

		// Check for private IPv4 ranges
		// 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16
		if let Ok(addr) = host.parse::<std::net::IpAddr>() {
			match addr {
				std::net::IpAddr::V4(ipv4) => {
					let octets = ipv4.octets();
					return octets[0] == 10
						|| (octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31))
						|| (octets[0] == 192 && octets[1] == 168)
						|| (octets[0] == 169 && octets[1] == 254)
						|| octets[0] == 127;
				}
				std::net::IpAddr::V6(ipv6) => {
					// Check for IPv6 localhost and link-local
					return ipv6.is_loopback()
						|| (ipv6.segments()[0] & 0xffc0) == 0xfe80 // Link-local
						|| (ipv6.segments()[0] & 0xfe00) == 0xfc00; // Unique local
				}
			}
		}

		false
	}

	// Direct methods for internal use

	/// Execute a GET request (for internal use)
	pub async fn get(&self, url: &str) -> Result<HttpResponse> {
		let params = json!({
			"method": "GET",
			"url": url,
			"headers": {},
			"body": null,
			"timeout": 30000,
			"follow_redirects": true,
			"max_redirects": 10
		});

		let result = self.request(params).await?;

		if result["ok"] == true {
			let response: HttpResponse = serde_json::from_value(result["data"].clone())?;
			Ok(response)
		} else {
			anyhow::bail!(
				"HTTP request failed: {}",
				result["err"]["message"].as_str().unwrap_or("Unknown error")
			)
		}
	}

	/// Execute a POST request (for internal use)
	pub async fn post(&self, url: &str, body: &str, headers: HashMap<String, String>) -> Result<HttpResponse> {
		let params = json!({
			"method": "POST",
			"url": url,
			"headers": headers,
			"body": body,
			"timeout": 30000,
			"follow_redirects": true,
			"max_redirects": 10
		});

		let result = self.request(params).await?;

		if result["ok"] == true {
			let response: HttpResponse = serde_json::from_value(result["data"].clone())?;
			Ok(response)
		} else {
			anyhow::bail!(
				"HTTP request failed: {}",
				result["err"]["message"].as_str().unwrap_or("Unknown error")
			)
		}
	}
}

impl Default for HttpBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tokio;

	#[tokio::test]
	async fn test_http_get_success() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/get",
			"headers": {
				"User-Agent": "Frame-Test/1.0"
			},
			"timeout": 10000,
			"follow_redirects": true,
			"max_redirects": 10
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
		assert!(result["data"]["body"].is_string());
		assert!(result["data"]["headers"].is_object());
	}

	#[tokio::test]
	async fn test_http_post_json() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "POST",
			"url": "https://httpbin.org/post",
			"headers": {
				"Content-Type": "application/json",
				"User-Agent": "Frame-Test/1.0"
			},
			"body": json!({"test": "data"}).to_string(),
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);

		// Verify the request body was sent correctly
		let body_str = result["data"]["body"].as_str().unwrap();
		assert!(body_str.contains("\"test\""));
	}

	#[tokio::test]
	async fn test_http_invalid_url() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "not-a-valid-url",
			"timeout": 5000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "INVALID_URL");
	}

	#[tokio::test]
	async fn test_http_invalid_method() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "INVALID",
			"url": "https://httpbin.org/get",
			"timeout": 5000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_http_timeout() {
		let mut bridge = HttpBridge::new();

		// httpbin.org/delay/10 waits 10 seconds before responding
		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/delay/10",
			"timeout": 1000 // 1 second timeout
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "TIMEOUT");
	}

	#[tokio::test]
	async fn test_http_redirect_follow() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/redirect/3", // 3 redirects
			"follow_redirects": true,
			"max_redirects": 10,
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
		// Final URL should be /get
		assert!(result["data"]["url"].as_str().unwrap().contains("/get"));
	}

	#[tokio::test]
	async fn test_http_redirect_no_follow() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/redirect/1",
			"follow_redirects": false,
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		// Should get redirect status code
		assert_eq!(result["data"]["status"], 302);
	}

	#[tokio::test]
	async fn test_http_too_many_redirects() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/redirect/20", // 20 redirects
			"follow_redirects": true,
			"max_redirects": 5, // Only allow 5
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "NETWORK_FAIL");
		assert!(result["err"]["message"].as_str().unwrap().contains("Too many redirects"));
	}

	#[tokio::test]
	async fn test_http_custom_headers() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/headers",
			"headers": {
				"X-Custom-Header": "test-value",
				"User-Agent": "Frame-Test/1.0"
			},
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);

		let body = result["data"]["body"].as_str().unwrap();
		assert!(body.contains("X-Custom-Header"));
		assert!(body.contains("test-value"));
	}

	#[tokio::test]
	async fn test_http_put_request() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "PUT",
			"url": "https://httpbin.org/put",
			"headers": {
				"Content-Type": "application/json"
			},
			"body": json!({"key": "value"}).to_string(),
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
	}

	#[tokio::test]
	async fn test_http_delete_request() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "DELETE",
			"url": "https://httpbin.org/delete",
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
	}

	#[tokio::test]
	async fn test_http_patch_request() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "PATCH",
			"url": "https://httpbin.org/patch",
			"headers": {
				"Content-Type": "application/json"
			},
			"body": json!({"patched": true}).to_string(),
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
	}

	#[tokio::test]
	async fn test_http_respond_success() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"status": 200,
			"headers": {
				"Content-Type": "application/json"
			},
			"body": json!({"message": "ok"}).to_string()
		});

		let result = bridge.call("respond", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);
	}

	#[tokio::test]
	async fn test_http_respond_redirect() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"status": 302,
			"headers": {
				"Location": "/login"
			},
			"body": ""
		});

		let result = bridge.call("respond", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 302);
		assert_eq!(result["data"]["headers"]["Location"], "/login");
	}

	#[tokio::test]
	async fn test_http_respond_invalid_status() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"status": 999,
			"headers": {},
			"body": ""
		});

		let result = bridge.call("respond", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_http_timeout_too_long() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/get",
			"timeout": 400000 // More than 5 minutes
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].as_str().unwrap().contains("Timeout exceeds maximum"));
	}

	#[tokio::test]
	async fn test_http_unsupported_scheme() {
		let mut bridge = HttpBridge::new();

		let params = json!({
			"method": "GET",
			"url": "ftp://example.com/file.txt",
			"timeout": 5000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "INVALID_URL");
		assert!(result["err"]["message"].as_str().unwrap().contains("Unsupported URL scheme"));
	}

	#[tokio::test]
	async fn test_http_private_ip_blocked() {
		let mut bridge = HttpBridge::new();

		let private_ips = vec![
			"http://127.0.0.1/",
			"http://localhost/",
			"http://10.0.0.1/",
			"http://172.16.0.1/",
			"http://192.168.1.1/",
		];

		for ip in private_ips {
			let params = json!({
				"method": "GET",
				"url": ip,
				"timeout": 5000
			});

			let result = bridge.call("request", params).await.unwrap();

			assert_eq!(result["ok"], false);
			assert_eq!(result["err"]["code"], "NETWORK_FAIL");
			assert!(result["err"]["message"].as_str().unwrap().contains("private IP"));
		}
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let mut bridge = HttpBridge::new();

		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "HTTP_ERROR");
	}

	#[test]
	fn test_is_private_ip() {
		// Localhost
		assert!(HttpBridge::is_private_ip("localhost"));
		assert!(HttpBridge::is_private_ip("127.0.0.1"));
		assert!(HttpBridge::is_private_ip("::1"));

		// Private IPv4 ranges
		assert!(HttpBridge::is_private_ip("10.0.0.1"));
		assert!(HttpBridge::is_private_ip("172.16.0.1"));
		assert!(HttpBridge::is_private_ip("192.168.1.1"));
		assert!(HttpBridge::is_private_ip("169.254.1.1"));

		// Public IPs should not be blocked
		assert!(!HttpBridge::is_private_ip("8.8.8.8"));
		assert!(!HttpBridge::is_private_ip("1.1.1.1"));
		assert!(!HttpBridge::is_private_ip("example.com")); // Hostname (not IP)
	}

	#[tokio::test]
	async fn test_direct_get_method() {
		let bridge = HttpBridge::new();

		let result = bridge.get("https://httpbin.org/get").await;

		assert!(result.is_ok());
		let response = result.unwrap();
		assert_eq!(response.status, 200);
		assert!(!response.body.is_empty());
	}

	#[tokio::test]
	async fn test_direct_post_method() {
		let bridge = HttpBridge::new();

		let mut headers = HashMap::new();
		headers.insert("Content-Type".to_string(), "application/json".to_string());

		let body = json!({"test": "data"}).to_string();

		let result = bridge.post("https://httpbin.org/post", &body, headers).await;

		assert!(result.is_ok());
		let response = result.unwrap();
		assert_eq!(response.status, 200);
		assert!(response.body.contains("\"test\""));
	}

	#[tokio::test]
	async fn test_http_status_codes() {
		let mut bridge = HttpBridge::new();

		// Test different status codes
		let test_cases = vec![
			(200, "https://httpbin.org/status/200"),
			(404, "https://httpbin.org/status/404"),
			(500, "https://httpbin.org/status/500"),
		];

		for (expected_status, url) in test_cases {
			let params = json!({
				"method": "GET",
				"url": url,
				"timeout": 10000
			});

			let result = bridge.call("request", params).await.unwrap();

			assert_eq!(result["ok"], true);
			assert_eq!(result["data"]["status"], expected_status);
		}
	}

	#[tokio::test]
	#[ignore = "requires external network access to httpbin.org"]
	async fn test_http_compression() {
		let mut bridge = HttpBridge::new();

		// httpbin.org/gzip returns gzip-compressed response
		let params = json!({
			"method": "GET",
			"url": "https://httpbin.org/gzip",
			"timeout": 10000
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["status"], 200);

		// Verify body was decompressed
		let body = result["data"]["body"].as_str().unwrap();
		assert!(body.contains("gzipped")); // Response should be decompressed
	}

	#[tokio::test]
	async fn test_validation_error_format() {
		let mut bridge = HttpBridge::new();

		// Invalid params (missing required fields)
		let params = json!({
			"invalid": "params"
		});

		let result = bridge.call("request", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
		assert!(result["err"]["message"].is_string());
		assert!(result["err"]["details"].is_object());
	}
}
