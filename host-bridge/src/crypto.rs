use anyhow::Result;
use argon2::{
	password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
	Argon2,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Crypto bridge providing cryptographic operations
pub struct CryptoBridge;

// Request structures
#[derive(Debug, Serialize, Deserialize)]
struct RandomRequest {
	bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct HashRequest {
	password: String,
	algorithm: String,
	#[serde(default)]
	cost: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerifyRequest {
	password: String,
	hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SignRequest {
	payload: Value,
	secret: String,
	algorithm: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerifyJwtRequest {
	token: String,
	secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DecodeJwtRequest {
	token: String,
}

// JWT Claims structure (flexible)
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
	#[serde(flatten)]
	data: serde_json::Map<String, Value>,
}

impl CryptoBridge {
	/// Create a new CryptoBridge instance
	pub fn new() -> Self {
		Self
	}

	/// Main call dispatcher for the crypto bridge
	pub async fn call(&mut self, function: &str, params: Value) -> Result<Value> {
		match function {
			"random" => self.random(params),
			"hash" => self.hash(params),
			"verify" => self.verify(params),
			"sign" => self.sign(params),
			"verify_jwt" => self.verify_jwt(params),
			"decode_jwt" => self.decode_jwt(params),
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "CRYPTO_ERROR",
						"message": format!("Unknown crypto function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Generate cryptographically secure random bytes
	/// Args: {"bytes": 32}
	/// Returns: {"ok": true, "data": "base64encodedrandombytes=="}
	fn random(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: RandomRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'bytes' field",
						"details": {}
					}
				}));
			}
		};

		// Validate byte count
		if request.bytes == 0 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Byte count must be greater than 0",
					"details": {"bytes": request.bytes}
				}
			}));
		}

		// Maximum 1MB of random data per call
		if request.bytes > 1_048_576 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Byte count exceeds maximum allowed (1048576 bytes / 1MB)",
					"details": {"bytes": request.bytes, "max": 1_048_576}
				}
			}));
		}

		// Generate random bytes using OS entropy
		let mut random_bytes = vec![0u8; request.bytes];
		match OsRng.try_fill_bytes(&mut random_bytes) {
			Ok(_) => {},
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CRYPTO_ERROR",
						"message": format!("Failed to generate random bytes: {}", e),
						"details": {}
					}
				}));
			}
		}

		// Encode to base64
		let base64_data = BASE64.encode(&random_bytes);

		Ok(json!({
			"ok": true,
			"data": base64_data
		}))
	}

	/// Hash a password using bcrypt or argon2
	/// Args: {"password": "secret123", "algorithm": "bcrypt", "cost": 12}
	/// Returns: {"ok": true, "data": "$2b$12$hashvalue..."}
	fn hash(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: HashRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'password' and 'algorithm' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate password length (minimum 1 character)
		if request.password.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Password cannot be empty",
					"details": {}
				}
			}));
		}

		// Maximum password length: 72 bytes for bcrypt, 4GB for argon2 (we limit to 1MB)
		if request.password.len() > 1_048_576 {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Password exceeds maximum allowed length (1048576 bytes / 1MB)",
					"details": {"length": request.password.len(), "max": 1_048_576}
				}
			}));
		}

		// Hash based on algorithm
		let hash_result = match request.algorithm.to_lowercase().as_str() {
			"bcrypt" => {
				// Validate cost factor (10-14, default 12)
				let cost = request.cost.unwrap_or(DEFAULT_COST);
				if cost < 10 || cost > 14 {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Bcrypt cost factor must be between 10 and 14",
							"details": {"cost": cost, "min": 10, "max": 14}
						}
					}));
				}

				// Bcrypt has a maximum password length of 72 bytes
				if request.password.len() > 72 {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Password exceeds bcrypt maximum length (72 bytes)",
							"details": {"length": request.password.len(), "max": 72}
						}
					}));
				}

				// Hash the password
				match hash(&request.password, cost) {
					Ok(h) => h,
					Err(e) => {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "CRYPTO_ERROR",
								"message": format!("Bcrypt hashing failed: {}", e),
								"details": {}
							}
						}));
					}
				}
			}
			"argon2" | "argon2id" => {
				// Use argon2id variant (recommended)
				let argon2 = Argon2::default();

				// Generate a random salt
				let salt = SaltString::generate(&mut OsRng);

				// Hash the password
				match argon2.hash_password(request.password.as_bytes(), &salt) {
					Ok(hash) => hash.to_string(),
					Err(e) => {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "CRYPTO_ERROR",
								"message": format!("Argon2 hashing failed: {}", e),
								"details": {}
							}
						}));
					}
				}
			}
			_ => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "ALGORITHM_ERROR",
						"message": format!("Unsupported hashing algorithm: {}. Supported: bcrypt, argon2, argon2id", request.algorithm),
						"details": {"algorithm": request.algorithm}
					}
				}));
			}
		};

		Ok(json!({
			"ok": true,
			"data": hash_result
		}))
	}

	/// Verify a password against a hash (constant-time comparison)
	/// Args: {"password": "secret123", "hash": "$2b$12$..."}
	/// Returns: {"ok": true, "data": true} or {"ok": true, "data": false}
	fn verify(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: VerifyRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'password' and 'hash' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate inputs
		if request.password.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Password cannot be empty",
					"details": {}
				}
			}));
		}

		if request.hash.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Hash cannot be empty",
					"details": {}
				}
			}));
		}

		// Detect algorithm from hash format
		let is_valid = if request.hash.starts_with("$2") {
			// Bcrypt hash
			// Bcrypt has a maximum password length of 72 bytes
			if request.password.len() > 72 {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Password exceeds bcrypt maximum length (72 bytes)",
						"details": {"length": request.password.len(), "max": 72}
					}
				}));
			}

			// Verify password (constant-time comparison)
			match verify(&request.password, &request.hash) {
				Ok(valid) => valid,
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "CRYPTO_ERROR",
							"message": format!("Bcrypt verification failed: {}", e),
							"details": {}
						}
					}));
				}
			}
		} else if request.hash.starts_with("$argon2") {
			// Argon2 hash
			match PasswordHash::new(&request.hash) {
				Ok(parsed_hash) => {
					let argon2 = Argon2::default();
					// Verify password (constant-time comparison)
					match argon2.verify_password(request.password.as_bytes(), &parsed_hash) {
						Ok(_) => true,
						Err(_) => false,
					}
				}
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "CRYPTO_ERROR",
							"message": format!("Invalid argon2 hash format: {}", e),
							"details": {}
						}
					}));
				}
			}
		} else {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "ALGORITHM_ERROR",
					"message": "Unsupported hash format. Expected bcrypt ($2...) or argon2 ($argon2...)",
					"details": {"hash_prefix": request.hash.chars().take(10).collect::<String>()}
				}
			}));
		};

		Ok(json!({
			"ok": true,
			"data": is_valid
		}))
	}

	/// Sign a JWT token
	/// Args: {"payload": {"userId": 123, "role": "admin"}, "secret": "key", "algorithm": "HS256"}
	/// Returns: {"ok": true, "data": "eyJhbGc..."}
	fn sign(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: SignRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'payload', 'secret', and 'algorithm' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate secret
		if request.secret.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Secret cannot be empty",
					"details": {}
				}
			}));
		}

		// Validate payload is an object
		if !request.payload.is_object() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Payload must be an object",
					"details": {}
				}
			}));
		}

		// Parse algorithm
		let algorithm = match request.algorithm.to_uppercase().as_str() {
			"HS256" => Algorithm::HS256,
			"HS384" => Algorithm::HS384,
			"HS512" => Algorithm::HS512,
			"RS256" => Algorithm::RS256,
			_ => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "ALGORITHM_ERROR",
						"message": format!("Unsupported JWT algorithm: {}. Supported: HS256, HS384, HS512, RS256", request.algorithm),
						"details": {"algorithm": request.algorithm}
					}
				}));
			}
		};

		// Create header
		let header = Header::new(algorithm);

		// Convert payload to Claims
		let claims = Claims {
			data: request.payload.as_object().unwrap().clone(),
		};

		// Sign the token
		let token = match encode(&header, &claims, &EncodingKey::from_secret(request.secret.as_bytes())) {
			Ok(t) => t,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CRYPTO_ERROR",
						"message": format!("JWT signing failed: {}", e),
						"details": {}
					}
				}));
			}
		};

		Ok(json!({
			"ok": true,
			"data": token
		}))
	}

	/// Verify and decode a JWT token
	/// Args: {"token": "eyJhbGc...", "secret": "key"}
	/// Returns: {"ok": true, "data": {"userId": 123, "role": "admin", "exp": 1234567890, "iat": 1234567890}}
	fn verify_jwt(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: VerifyJwtRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'token' and 'secret' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate inputs
		if request.token.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Token cannot be empty",
					"details": {}
				}
			}));
		}

		if request.secret.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Secret cannot be empty",
					"details": {}
				}
			}));
		}

		// Decode and verify token
		// Try multiple algorithms to prevent algorithm confusion attacks
		// We only try the symmetric algorithms that use the secret key
		let algorithms = vec![Algorithm::HS256, Algorithm::HS384, Algorithm::HS512];

		let mut last_error = String::new();
		for algorithm in algorithms {
			let mut validation = Validation::new(algorithm);
			// Allow leeway for clock skew (default is 60 seconds)
			validation.leeway = 60;

			match decode::<Claims>(
				&request.token,
				&DecodingKey::from_secret(request.secret.as_bytes()),
				&validation,
			) {
				Ok(token_data) => {
					// Successfully verified
					return Ok(json!({
						"ok": true,
						"data": token_data.claims.data
					}));
				}
				Err(e) => {
					last_error = e.to_string();
					// Try next algorithm
					continue;
				}
			}
		}

		// All algorithms failed
		Ok(json!({
			"ok": false,
			"err": {
				"code": "AUTH_ERROR",
				"message": format!("JWT verification failed: {}", last_error),
				"details": {}
			}
		}))
	}

	/// Decode a JWT token without verification (for debugging)
	/// Args: {"token": "eyJhbGc..."}
	/// Returns: {"ok": true, "data": {"header": {...}, "payload": {...}}}
	fn decode_jwt(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: DecodeJwtRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'token' field",
						"details": {}
					}
				}));
			}
		};

		// Validate token
		if request.token.is_empty() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "Token cannot be empty",
					"details": {}
				}
			}));
		}

		// Decode without verification by disabling all validations
		let mut validation = Validation::default();
		validation.insecure_disable_signature_validation();
		validation.validate_exp = false;
		validation.validate_nbf = false;
		validation.required_spec_claims.clear(); // Don't require any specific claims

		// Use a dummy key since we're not validating the signature
		let dummy_key = DecodingKey::from_secret(&[]);

		match decode::<Claims>(&request.token, &dummy_key, &validation) {
			Ok(token_data) => {
				Ok(json!({
					"ok": true,
					"data": {
						"header": token_data.header,
						"payload": token_data.claims.data
					}
				}))
			}
			Err(e) => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "CRYPTO_ERROR",
						"message": format!("JWT decoding failed: {}", e),
						"details": {}
					}
				}))
			}
		}
	}
}

impl Default for CryptoBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_random() {
		let mut bridge = CryptoBridge::new();

		// Test random generation
		let result = bridge.call("random", json!({"bytes": 32})).await.unwrap();

		assert_eq!(result["ok"], true);
		let data = result["data"].as_str().unwrap();

		// Decode base64 and verify length
		let decoded = BASE64.decode(data).unwrap();
		assert_eq!(decoded.len(), 32);

		// Test that two calls generate different values
		let result2 = bridge.call("random", json!({"bytes": 32})).await.unwrap();
		let data2 = result2["data"].as_str().unwrap();
		assert_ne!(data, data2);
	}

	#[tokio::test]
	async fn test_random_validation() {
		let mut bridge = CryptoBridge::new();

		// Test zero bytes
		let result = bridge.call("random", json!({"bytes": 0})).await.unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Test too many bytes
		let result = bridge.call("random", json!({"bytes": 2_000_000})).await.unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Test invalid request
		let result = bridge.call("random", json!({})).await.unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_bcrypt_hash_and_verify() {
		let mut bridge = CryptoBridge::new();

		// Hash a password
		let password = "supersecret123";
		let result = bridge.call("hash", json!({
			"password": password,
			"algorithm": "bcrypt",
			"cost": 10
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let hash = result["data"].as_str().unwrap();
		assert!(hash.starts_with("$2"));

		// Verify correct password
		let result = bridge.call("verify", json!({
			"password": password,
			"hash": hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);

		// Verify incorrect password
		let result = bridge.call("verify", json!({
			"password": "wrongpassword",
			"hash": hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);
	}

	#[tokio::test]
	async fn test_bcrypt_default_cost() {
		let mut bridge = CryptoBridge::new();

		// Hash without specifying cost (should use default)
		let result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "bcrypt"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let hash = result["data"].as_str().unwrap();
		assert!(hash.starts_with("$2"));
	}

	#[tokio::test]
	async fn test_bcrypt_cost_validation() {
		let mut bridge = CryptoBridge::new();

		// Cost too low
		let result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "bcrypt",
			"cost": 9
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Cost too high
		let result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "bcrypt",
			"cost": 15
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_bcrypt_max_password_length() {
		let mut bridge = CryptoBridge::new();

		// Password exactly at limit (72 bytes)
		let password = "a".repeat(72);
		let result = bridge.call("hash", json!({
			"password": password,
			"algorithm": "bcrypt"
		})).await.unwrap();

		assert_eq!(result["ok"], true);

		// Password over limit
		let password = "a".repeat(73);
		let result = bridge.call("hash", json!({
			"password": password,
			"algorithm": "bcrypt"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_argon2_hash_and_verify() {
		let mut bridge = CryptoBridge::new();

		// Hash a password with argon2
		let password = "supersecret456";
		let result = bridge.call("hash", json!({
			"password": password,
			"algorithm": "argon2"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let hash = result["data"].as_str().unwrap();
		assert!(hash.starts_with("$argon2"));

		// Verify correct password
		let result = bridge.call("verify", json!({
			"password": password,
			"hash": hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);

		// Verify incorrect password
		let result = bridge.call("verify", json!({
			"password": "wrongpassword",
			"hash": hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);
	}

	#[tokio::test]
	async fn test_argon2id_algorithm() {
		let mut bridge = CryptoBridge::new();

		// Test argon2id variant
		let result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "argon2id"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let hash = result["data"].as_str().unwrap();
		assert!(hash.starts_with("$argon2"));
	}

	#[tokio::test]
	async fn test_hash_validation() {
		let mut bridge = CryptoBridge::new();

		// Empty password
		let result = bridge.call("hash", json!({
			"password": "",
			"algorithm": "bcrypt"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Unsupported algorithm
		let result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "md5"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "ALGORITHM_ERROR");

		// Invalid request format
		let result = bridge.call("hash", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_verify_validation() {
		let mut bridge = CryptoBridge::new();

		// Empty password
		let result = bridge.call("verify", json!({
			"password": "",
			"hash": "$2b$10$test"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Empty hash
		let result = bridge.call("verify", json!({
			"password": "test",
			"hash": ""
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Invalid hash format
		let result = bridge.call("verify", json!({
			"password": "test",
			"hash": "not-a-valid-hash"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "ALGORITHM_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_sign_and_verify() {
		let mut bridge = CryptoBridge::new();

		let secret = "my-super-secret-key";
		let payload = json!({
			"userId": 123,
			"role": "admin",
			"exp": 9999999999_i64
		});

		// Sign a JWT
		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS256"
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		let token = result["data"].as_str().unwrap();
		assert!(token.contains('.'));

		// Verify the JWT
		let result = bridge.call("verify_jwt", json!({
			"token": token,
			"secret": secret
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["userId"], 123);
		assert_eq!(result["data"]["role"], "admin");

		// Verify with wrong secret
		let result = bridge.call("verify_jwt", json!({
			"token": token,
			"secret": "wrong-secret"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "AUTH_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_algorithms() {
		let mut bridge = CryptoBridge::new();

		let secret = "test-secret";
		let payload = json!({"sub": 1, "exp": 9999999999_i64});

		// Test HS256
		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS256"
		})).await.unwrap();
		assert_eq!(result["ok"], true);

		// Test HS384
		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS384"
		})).await.unwrap();
		assert_eq!(result["ok"], true);

		// Test HS512
		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS512"
		})).await.unwrap();
		assert_eq!(result["ok"], true);

		// Test unsupported algorithm
		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "ES256"
		})).await.unwrap();
		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "ALGORITHM_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_sign_validation() {
		let mut bridge = CryptoBridge::new();

		// Empty secret
		let result = bridge.call("sign", json!({
			"payload": {"sub": 1},
			"secret": "",
			"algorithm": "HS256"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Non-object payload
		let result = bridge.call("sign", json!({
			"payload": "not-an-object",
			"secret": "secret",
			"algorithm": "HS256"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Invalid request
		let result = bridge.call("sign", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_verify_validation() {
		let mut bridge = CryptoBridge::new();

		// Empty token
		let result = bridge.call("verify_jwt", json!({
			"token": "",
			"secret": "secret"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Empty secret
		let result = bridge.call("verify_jwt", json!({
			"token": "token",
			"secret": ""
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Invalid token
		let result = bridge.call("verify_jwt", json!({
			"token": "not.a.jwt",
			"secret": "secret"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "AUTH_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_decode_without_verification() {
		let mut bridge = CryptoBridge::new();

		// Create a JWT
		let secret = "secret";
		let payload = json!({"userId": 789, "role": "viewer"});

		let sign_result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS256"
		})).await.unwrap();

		let token = sign_result["data"].as_str().unwrap();

		// Decode without verification
		let result = bridge.call("decode_jwt", json!({
			"token": token
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["payload"]["userId"], 789);
		assert_eq!(result["data"]["payload"]["role"], "viewer");
		assert!(result["data"]["header"].is_object());
	}

	#[tokio::test]
	async fn test_jwt_decode_validation() {
		let mut bridge = CryptoBridge::new();

		// Empty token
		let result = bridge.call("decode_jwt", json!({
			"token": ""
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");

		// Invalid token
		let result = bridge.call("decode_jwt", json!({
			"token": "not-a-jwt"
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "CRYPTO_ERROR");
	}

	#[tokio::test]
	async fn test_jwt_expiration() {
		let mut bridge = CryptoBridge::new();

		let secret = "test-secret";

		// Create an expired token (exp in the past)
		let payload = json!({
			"sub": 1,
			"exp": 1000000000_i64  // Way in the past
		});

		let result = bridge.call("sign", json!({
			"payload": payload,
			"secret": secret,
			"algorithm": "HS256"
		})).await.unwrap();

		let token = result["data"].as_str().unwrap();

		// Verify should fail due to expiration
		let result = bridge.call("verify_jwt", json!({
			"token": token,
			"secret": secret
		})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "AUTH_ERROR");
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let mut bridge = CryptoBridge::new();

		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "CRYPTO_ERROR");
	}

	#[tokio::test]
	async fn test_constant_time_verification() {
		let mut bridge = CryptoBridge::new();

		// Hash a password
		let result = bridge.call("hash", json!({
			"password": "correct",
			"algorithm": "bcrypt",
			"cost": 10
		})).await.unwrap();

		let hash = result["data"].as_str().unwrap();

		// Time correct password verification
		let start = std::time::Instant::now();
		bridge.call("verify", json!({
			"password": "correct",
			"hash": hash
		})).await.unwrap();
		let correct_duration = start.elapsed();

		// Time incorrect password verification
		let start = std::time::Instant::now();
		bridge.call("verify", json!({
			"password": "incorrect",
			"hash": hash
		})).await.unwrap();
		let incorrect_duration = start.elapsed();

		// Both should take roughly the same time (within 50ms for bcrypt cost 10)
		// This is a basic check - true constant-time verification is guaranteed by the bcrypt library
		let diff = if correct_duration > incorrect_duration {
			correct_duration - incorrect_duration
		} else {
			incorrect_duration - correct_duration
		};

		// The difference should be minimal (< 50ms)
		// Note: This is not a perfect test, but it demonstrates the concept
		assert!(diff.as_millis() < 50, "Timing difference too large: {} ms", diff.as_millis());
	}

	#[tokio::test]
	async fn test_cross_algorithm_compatibility() {
		let mut bridge = CryptoBridge::new();

		// Hash with bcrypt, verify detects it automatically
		let bcrypt_result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "bcrypt"
		})).await.unwrap();

		let bcrypt_hash = bcrypt_result["data"].as_str().unwrap();

		let result = bridge.call("verify", json!({
			"password": "test",
			"hash": bcrypt_hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);

		// Hash with argon2, verify detects it automatically
		let argon2_result = bridge.call("hash", json!({
			"password": "test",
			"algorithm": "argon2"
		})).await.unwrap();

		let argon2_hash = argon2_result["data"].as_str().unwrap();

		let result = bridge.call("verify", json!({
			"password": "test",
			"hash": argon2_hash
		})).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);
	}
}
