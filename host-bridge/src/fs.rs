use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use glob::glob as glob_pattern;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Filesystem bridge providing file and directory operations
/// SECURITY: Only available on desktop (Windows, macOS, Linux, FreeBSD) and CLI platforms
/// Disabled on web and mobile platforms for security
pub struct FsBridge {
	/// Optional sandbox root directory - all paths are resolved relative to this
	sandbox_root: Option<PathBuf>,
	/// Maximum file size for read/write operations (default: 100MB)
	max_file_size: u64,
}

// Request structures
#[derive(Debug, Serialize, Deserialize)]
struct ReadRequest {
	path: String,
	#[serde(default = "default_encoding")]
	encoding: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WriteRequest {
	path: String,
	content: String,
	#[serde(default = "default_encoding")]
	encoding: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AppendRequest {
	path: String,
	content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExistsRequest {
	path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeleteRequest {
	path: String,
	#[serde(default)]
	recursive: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct MkdirRequest {
	path: String,
	#[serde(default)]
	recursive: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListRequest {
	path: String,
	#[serde(default)]
	pattern: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StatRequest {
	path: String,
}

// Response structures
#[derive(Debug, Serialize, Deserialize)]
struct FileEntry {
	name: String,
	#[serde(rename = "type")]
	entry_type: String,
	size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListResponse {
	entries: Vec<FileEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StatResponse {
	size: u64,
	is_file: bool,
	is_directory: bool,
	is_symlink: bool,
	created: String,
	modified: String,
	accessed: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	permissions: Option<String>,
}

// Helper functions
fn default_encoding() -> String {
	"utf-8".to_string()
}

impl FsBridge {
	/// Create a new FsBridge instance
	/// Default max file size: 100MB
	pub fn new() -> Self {
		Self {
			sandbox_root: None,
			max_file_size: 100 * 1024 * 1024, // 100MB
		}
	}

	/// Create a new FsBridge with a sandbox root directory
	pub fn with_sandbox(sandbox_root: PathBuf) -> Self {
		Self {
			sandbox_root: Some(sandbox_root),
			max_file_size: 100 * 1024 * 1024,
		}
	}

	/// Set the maximum file size for read/write operations
	pub fn with_max_file_size(mut self, max_size: u64) -> Self {
		self.max_file_size = max_size;
		self
	}

	/// Check if filesystem operations are allowed on this platform
	pub fn is_platform_allowed(&self) -> bool {
		cfg!(any(
			target_os = "windows",
			target_os = "macos",
			target_os = "linux",
			target_os = "freebsd"
		))
	}

	/// Main call dispatcher for the filesystem bridge
	pub async fn call(&self, function: &str, params: Value) -> Result<Value> {
		// Check platform permission first
		if !self.is_platform_allowed() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "PERMISSION_DENIED",
					"message": "Filesystem operations are only available on desktop and CLI platforms",
					"details": {}
				}
			}));
		}

		match function {
			"read" => self.read(params).await,
			"write" => self.write(params).await,
			"append" => self.append(params).await,
			"exists" => self.exists(params).await,
			"delete" => self.delete(params).await,
			"mkdir" => self.mkdir(params).await,
			"list" => self.list(params).await,
			"stat" => self.stat(params).await,
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "FS_ERROR",
						"message": format!("Unknown filesystem function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Read file contents (text or binary)
	/// Args: {"path": "/path/to/file.txt", "encoding": "utf-8"}
	/// Encoding options: "utf-8" (default), "base64", "ascii"
	/// Returns: {"ok": true, "data": "file contents..."}
	async fn read(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: ReadRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Check if file exists
		if !path.exists() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "NOT_FOUND",
					"message": format!("File not found: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// Check if it's a file (not a directory)
		if !path.is_file() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FS_ERROR",
					"message": format!("Path is not a file: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// Check file size
		let metadata = match fs::metadata(&path).await {
			Ok(m) => m,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "FS_ERROR",
						"message": format!("Failed to read file metadata: {}", e),
						"details": {"path": request.path}
					}
				}));
			}
		};

		if metadata.len() > self.max_file_size {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FILE_TOO_LARGE",
					"message": format!("File exceeds maximum size ({} bytes)", self.max_file_size),
					"details": {
						"path": request.path,
						"size": metadata.len(),
						"max_size": self.max_file_size
					}
				}
			}));
		}

		// Read file contents
		let bytes = match fs::read(&path).await {
			Ok(b) => b,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "FS_ERROR",
						"message": format!("Failed to read file: {}", e),
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Encode based on requested encoding
		let content = match request.encoding.to_lowercase().as_str() {
			"utf-8" | "utf8" => {
				match String::from_utf8(bytes) {
					Ok(s) => s,
					Err(e) => {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "FS_ERROR",
								"message": format!("File is not valid UTF-8: {}", e),
								"details": {"path": request.path}
							}
						}));
					}
				}
			}
			"base64" => BASE64.encode(&bytes),
			"ascii" => {
				match String::from_utf8(bytes.clone()) {
					Ok(s) if s.is_ascii() => s,
					_ => {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "FS_ERROR",
								"message": "File is not valid ASCII",
								"details": {"path": request.path}
							}
						}));
					}
				}
			}
			_ => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Unsupported encoding: {}. Supported: utf-8, base64, ascii", request.encoding),
						"details": {"encoding": request.encoding}
					}
				}));
			}
		};

		Ok(json!({
			"ok": true,
			"data": content
		}))
	}

	/// Write file contents (text or binary)
	/// Args: {"path": "/path/to/file.txt", "content": "Hello World", "encoding": "utf-8"}
	/// Encoding options: "utf-8" (default), "base64", "ascii"
	/// Returns: {"ok": true, "data": null}
	async fn write(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: WriteRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' and 'content' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Decode content based on encoding
		let bytes = match request.encoding.to_lowercase().as_str() {
			"utf-8" | "utf8" => request.content.into_bytes(),
			"base64" => {
				match BASE64.decode(&request.content) {
					Ok(b) => b,
					Err(e) => {
						return Ok(json!({
							"ok": false,
							"err": {
								"code": "VALIDATION_ERROR",
								"message": format!("Invalid base64 content: {}", e),
								"details": {}
							}
						}));
					}
				}
			}
			"ascii" => {
				if request.content.is_ascii() {
					request.content.into_bytes()
				} else {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": "Content is not valid ASCII",
							"details": {}
						}
					}));
				}
			}
			_ => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Unsupported encoding: {}. Supported: utf-8, base64, ascii", request.encoding),
						"details": {"encoding": request.encoding}
					}
				}));
			}
		};

		// Check content size
		if bytes.len() as u64 > self.max_file_size {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FILE_TOO_LARGE",
					"message": format!("Content exceeds maximum size ({} bytes)", self.max_file_size),
					"details": {
						"size": bytes.len(),
						"max_size": self.max_file_size
					}
				}
			}));
		}

		// Create parent directories if they don't exist
		if let Some(parent) = path.parent() {
			if !parent.exists() {
				if let Err(e) = fs::create_dir_all(parent).await {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to create parent directories: {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			}
		}

		// Write file
		if let Err(e) = fs::write(&path, &bytes).await {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FS_ERROR",
					"message": format!("Failed to write file: {}", e),
					"details": {"path": request.path}
				}
			}));
		}

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Append content to a file
	/// Args: {"path": "/path/to/file.txt", "content": "Additional content"}
	/// Returns: {"ok": true, "data": null}
	async fn append(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: AppendRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' and 'content' fields",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Read existing content if file exists
		let mut existing_content = if path.exists() {
			match fs::read(&path).await {
				Ok(b) => b,
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to read existing file: {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			}
		} else {
			Vec::new()
		};

		// Append new content
		let new_bytes = request.content.into_bytes();
		existing_content.extend_from_slice(&new_bytes);

		// Check total size
		if existing_content.len() as u64 > self.max_file_size {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FILE_TOO_LARGE",
					"message": format!("Resulting file would exceed maximum size ({} bytes)", self.max_file_size),
					"details": {
						"resulting_size": existing_content.len(),
						"max_size": self.max_file_size
					}
				}
			}));
		}

		// Create parent directories if they don't exist
		if let Some(parent) = path.parent() {
			if !parent.exists() {
				if let Err(e) = fs::create_dir_all(parent).await {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to create parent directories: {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			}
		}

		// Write combined content
		if let Err(e) = fs::write(&path, &existing_content).await {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FS_ERROR",
					"message": format!("Failed to append to file: {}", e),
					"details": {"path": request.path}
				}
			}));
		}

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Check if file or directory exists
	/// Args: {"path": "/path/to/file"}
	/// Returns: {"ok": true, "data": true} or {"ok": true, "data": false}
	async fn exists(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: ExistsRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		let exists = path.exists();

		Ok(json!({
			"ok": true,
			"data": exists
		}))
	}

	/// Delete file or directory
	/// Args: {"path": "/path/to/file", "recursive": false}
	/// Returns: {"ok": true, "data": null}
	async fn delete(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: DeleteRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Check if path exists
		if !path.exists() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "NOT_FOUND",
					"message": format!("Path not found: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// Delete based on type
		if path.is_dir() {
			if request.recursive {
				if let Err(e) = fs::remove_dir_all(&path).await {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to delete directory recursively: {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			} else {
				if let Err(e) = fs::remove_dir(&path).await {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to delete directory (use recursive=true for non-empty directories): {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			}
		} else {
			if let Err(e) = fs::remove_file(&path).await {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "FS_ERROR",
						"message": format!("Failed to delete file: {}", e),
						"details": {"path": request.path}
					}
				}));
			}
		}

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Create directory
	/// Args: {"path": "/path/to/dir", "recursive": false}
	/// Returns: {"ok": true, "data": null}
	async fn mkdir(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: MkdirRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Create directory
		let result = if request.recursive {
			fs::create_dir_all(&path).await
		} else {
			fs::create_dir(&path).await
		};

		if let Err(e) = result {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FS_ERROR",
					"message": format!("Failed to create directory: {}", e),
					"details": {"path": request.path}
				}
			}));
		}

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// List directory contents
	/// Args: {"path": "/path/to/dir", "pattern": "*.txt"}
	/// Pattern is optional glob pattern (e.g., "*.txt", "test_*.rs")
	/// Returns: {"ok": true, "data": {"entries": [...]}}
	async fn list(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: ListRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Check if directory exists
		if !path.exists() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "NOT_FOUND",
					"message": format!("Directory not found: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// Check if it's a directory
		if !path.is_dir() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "FS_ERROR",
					"message": format!("Path is not a directory: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// List entries
		let mut entries = Vec::new();

		// If pattern is specified, use glob matching
		if let Some(pattern) = request.pattern {
			// Build glob pattern
			let glob_path = path.join(&pattern);
			let glob_str = match glob_path.to_str() {
				Some(s) => s,
				None => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": "Invalid path encoding",
							"details": {}
						}
					}));
				}
			};

			// Execute glob
			let glob_results = match glob_pattern(glob_str) {
				Ok(paths) => paths,
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "VALIDATION_ERROR",
							"message": format!("Invalid glob pattern: {}", e),
							"details": {"pattern": pattern}
						}
					}));
				}
			};

			// Process glob results
			for entry_result in glob_results {
				let entry_path = match entry_result {
					Ok(p) => p,
					Err(e) => {
						// Skip entries with errors (e.g., permission denied)
						tracing::warn!("Error reading directory entry: {}", e);
						continue;
					}
				};

				if let Some(file_entry) = self.create_file_entry(&entry_path).await {
					entries.push(file_entry);
				}
			}
		} else {
			// List all entries in directory
			let mut read_dir = match fs::read_dir(&path).await {
				Ok(rd) => rd,
				Err(e) => {
					return Ok(json!({
						"ok": false,
						"err": {
							"code": "FS_ERROR",
							"message": format!("Failed to read directory: {}", e),
							"details": {"path": request.path}
						}
					}));
				}
			};

			// Read all entries
			while let Ok(Some(entry)) = read_dir.next_entry().await {
				if let Some(file_entry) = self.create_file_entry(&entry.path()).await {
					entries.push(file_entry);
				}
			}
		}

		// Sort entries by name for consistent output
		entries.sort_by(|a, b| a.name.cmp(&b.name));

		let response = ListResponse { entries };

		Ok(json!({
			"ok": true,
			"data": response
		}))
	}

	/// Get file or directory metadata
	/// Args: {"path": "/path/to/file"}
	/// Returns: {"ok": true, "data": {"size": 1024, "is_file": true, ...}}
	async fn stat(&self, params: Value) -> Result<Value> {
		// Parse request
		let request: StatRequest = match serde_json::from_value(params) {
			Ok(req) => req,
			Err(_) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": "Invalid request format: expected object with 'path' field",
						"details": {}
					}
				}));
			}
		};

		// Validate and resolve path
		let path = match self.validate_and_resolve_path(&request.path) {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": e.code,
						"message": e.message,
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Check if path exists
		if !path.exists() {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "NOT_FOUND",
					"message": format!("Path not found: {}", request.path),
					"details": {"path": request.path}
				}
			}));
		}

		// Get metadata
		let metadata = match fs::metadata(&path).await {
			Ok(m) => m,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "FS_ERROR",
						"message": format!("Failed to read metadata: {}", e),
						"details": {"path": request.path}
					}
				}));
			}
		};

		// Get symlink metadata separately
		let symlink_metadata = fs::symlink_metadata(&path).await.ok();

		let is_symlink = symlink_metadata
			.as_ref()
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false);

		// Format timestamps
		let created = match metadata.created() {
			Ok(time) => Self::format_system_time(time),
			Err(_) => "unknown".to_string(),
		};

		let modified = match metadata.modified() {
			Ok(time) => Self::format_system_time(time),
			Err(_) => "unknown".to_string(),
		};

		let accessed = match metadata.accessed() {
			Ok(time) => Self::format_system_time(time),
			Err(_) => "unknown".to_string(),
		};

		// Get permissions (Unix-style)
		let permissions = Self::format_permissions(&metadata);

		let stat_response = StatResponse {
			size: metadata.len(),
			is_file: metadata.is_file(),
			is_directory: metadata.is_dir(),
			is_symlink,
			created,
			modified,
			accessed,
			permissions,
		};

		Ok(json!({
			"ok": true,
			"data": stat_response
		}))
	}

	// Helper methods

	/// Validate path and prevent directory traversal attacks
	fn validate_and_resolve_path(&self, path_str: &str) -> Result<PathBuf, PathValidationError> {
		// Check for empty path
		if path_str.is_empty() {
			return Err(PathValidationError {
				code: "VALIDATION_ERROR".to_string(),
				message: "Path cannot be empty".to_string(),
			});
		}

		// Reject paths containing ".."
		if path_str.contains("..") {
			return Err(PathValidationError {
				code: "PATH_TRAVERSAL".to_string(),
				message: "Path traversal detected: '..' is not allowed".to_string(),
			});
		}

		// Validate path characters (allow: alphanumeric, /, -, _, ., space)
		for ch in path_str.chars() {
			if !ch.is_alphanumeric()
				&& ch != '/'
				&& ch != '-'
				&& ch != '_'
				&& ch != '.'
				&& ch != ' '
				&& ch != ':'
			{
				return Err(PathValidationError {
					code: "VALIDATION_ERROR".to_string(),
					message: format!("Invalid character in path: '{}'", ch),
				});
			}
		}

		// Resolve path
		let path = if path_str.starts_with('/') {
			// Absolute path
			PathBuf::from(path_str)
		} else {
			// Relative path - resolve relative to sandbox root or current directory
			if let Some(ref sandbox) = self.sandbox_root {
				sandbox.join(path_str)
			} else {
				// Use current directory if no sandbox
				std::env::current_dir()
					.map_err(|_| PathValidationError {
						code: "FS_ERROR".to_string(),
						message: "Failed to get current directory".to_string(),
					})?
					.join(path_str)
			}
		};

		// Canonicalize path to resolve any remaining '..' or symlinks
		// For existing paths, use full canonicalization
		// For non-existing paths, find the first existing ancestor and reconstruct the path
		let canonical_path = if path.exists() {
			path.canonicalize().map_err(|_| PathValidationError {
				code: "FS_ERROR".to_string(),
				message: "Failed to resolve path".to_string(),
			})?
		} else {
			// For non-existent paths, find the first existing ancestor
			let mut current = path.clone();
			let mut missing_components = Vec::new();

			// Walk up the tree collecting non-existent components
			loop {
				if current.exists() {
					break;
				}
				if let Some(filename) = current.file_name() {
					missing_components.push(filename.to_os_string());
					if let Some(parent) = current.parent() {
						if parent.as_os_str().is_empty() {
							// We've reached the relative base, stop here
							break;
						}
						current = parent.to_path_buf();
					} else {
						break;
					}
				} else {
					break;
				}
			}

			// Canonicalize the existing ancestor (or use as-is if none exists)
			let canonical_base = if current.exists() && !current.as_os_str().is_empty() {
				current.canonicalize().map_err(|_| PathValidationError {
					code: "FS_ERROR".to_string(),
					message: "Failed to resolve base path".to_string(),
				})?
			} else {
				current
			};

			// Reconstruct the path by joining the missing components in reverse order
			let mut result = canonical_base;
			for component in missing_components.iter().rev() {
				result = result.join(component);
			}

			result
		};

		// If sandbox is enabled, ensure the path is within the sandbox
		if let Some(ref sandbox) = self.sandbox_root {
			// Canonicalize sandbox for comparison
			let canonical_sandbox = sandbox.canonicalize().map_err(|_| PathValidationError {
				code: "FS_ERROR".to_string(),
				message: "Failed to resolve sandbox path".to_string(),
			})?;

			if !canonical_path.starts_with(&canonical_sandbox) {
				return Err(PathValidationError {
					code: "PERMISSION_DENIED".to_string(),
					message: "Path is outside sandbox boundaries".to_string(),
				});
			}
		}

		Ok(canonical_path)
	}

	/// Create a FileEntry from a path
	async fn create_file_entry(&self, path: &Path) -> Option<FileEntry> {
		let name = path.file_name()?.to_str()?.to_string();

		let metadata = fs::metadata(path).await.ok()?;

		let entry_type = if metadata.is_dir() {
			"directory".to_string()
		} else if metadata.is_file() {
			"file".to_string()
		} else {
			"other".to_string()
		};

		Some(FileEntry {
			name,
			entry_type,
			size: metadata.len(),
		})
	}

	/// Format system time to ISO 8601 string
	fn format_system_time(time: std::time::SystemTime) -> String {
		use std::time::UNIX_EPOCH;

		match time.duration_since(UNIX_EPOCH) {
			Ok(duration) => {
				let seconds = duration.as_secs();
				let nanos = duration.subsec_nanos();

				// Convert to ISO 8601 format
				// This is a simplified implementation; for production use chrono
				let datetime = chrono::DateTime::from_timestamp(seconds as i64, nanos)
					.unwrap_or_else(|| chrono::Utc::now());

				datetime.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
			}
			Err(_) => "unknown".to_string(),
		}
	}

	/// Format file permissions in Unix style (e.g., "rw-r--r--")
	#[cfg(unix)]
	fn format_permissions(metadata: &std::fs::Metadata) -> Option<String> {
		use std::os::unix::fs::PermissionsExt;

		let mode = metadata.permissions().mode();

		let user = [
			if mode & 0o400 != 0 { 'r' } else { '-' },
			if mode & 0o200 != 0 { 'w' } else { '-' },
			if mode & 0o100 != 0 { 'x' } else { '-' },
		];

		let group = [
			if mode & 0o040 != 0 { 'r' } else { '-' },
			if mode & 0o020 != 0 { 'w' } else { '-' },
			if mode & 0o010 != 0 { 'x' } else { '-' },
		];

		let other = [
			if mode & 0o004 != 0 { 'r' } else { '-' },
			if mode & 0o002 != 0 { 'w' } else { '-' },
			if mode & 0o001 != 0 { 'x' } else { '-' },
		];

		Some(format!(
			"{}{}{}",
			user.iter().collect::<String>(),
			group.iter().collect::<String>(),
			other.iter().collect::<String>()
		))
	}

	#[cfg(not(unix))]
	fn format_permissions(_metadata: &std::fs::Metadata) -> Option<String> {
		// On Windows, return None (permissions are handled differently)
		None
	}
}

impl Default for FsBridge {
	fn default() -> Self {
		Self::new()
	}
}

// Error type for path validation
struct PathValidationError {
	code: String,
	message: String,
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	// Helper to create a temporary sandbox for testing
	async fn create_test_sandbox() -> (TempDir, FsBridge) {
		let temp_dir = TempDir::new().unwrap();
		let bridge = FsBridge::with_sandbox(temp_dir.path().to_path_buf());
		(temp_dir, bridge)
	}

	#[tokio::test]
	async fn test_platform_check() {
		let bridge = FsBridge::new();

		// Platform check should pass on desktop platforms
		#[cfg(any(
			target_os = "windows",
			target_os = "macos",
			target_os = "linux",
			target_os = "freebsd"
		))]
		{
			assert!(bridge.is_platform_allowed());
		}

		// Platform check should fail on web/mobile
		#[cfg(not(any(
			target_os = "windows",
			target_os = "macos",
			target_os = "linux",
			target_os = "freebsd"
		)))]
		{
			assert!(!bridge.is_platform_allowed());
		}
	}

	#[tokio::test]
	async fn test_write_and_read_text() {
		let (_temp, bridge) = create_test_sandbox().await;

		let content = "Hello, World!";

		// Write file
		let result = bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": content,
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Read file
		let result = bridge
			.call(
				"read",
				json!({
					"path": "test.txt",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_str().unwrap(), content);
	}

	#[tokio::test]
	async fn test_write_and_read_binary() {
		let (_temp, bridge) = create_test_sandbox().await;

		let binary_data = vec![0x00, 0x01, 0x02, 0xFF];
		let base64_content = BASE64.encode(&binary_data);

		// Write file
		let result = bridge
			.call(
				"write",
				json!({
					"path": "test.bin",
					"content": base64_content,
					"encoding": "base64"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Read file
		let result = bridge
			.call(
				"read",
				json!({
					"path": "test.bin",
					"encoding": "base64"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		let read_base64 = result["data"].as_str().unwrap();
		let read_data = BASE64.decode(read_base64).unwrap();
		assert_eq!(read_data, binary_data);
	}

	#[tokio::test]
	async fn test_append() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Write initial content
		bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": "Hello",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// Append content
		let result = bridge
			.call(
				"append",
				json!({
					"path": "test.txt",
					"content": ", World!"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Read file
		let result = bridge
			.call(
				"read",
				json!({
					"path": "test.txt",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"].as_str().unwrap(), "Hello, World!");
	}

	#[tokio::test]
	async fn test_exists() {
		let (_temp, bridge) = create_test_sandbox().await;

		// File doesn't exist yet
		let result = bridge
			.call("exists", json!({"path": "test.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);

		// Create file
		bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": "test",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// File now exists
		let result = bridge
			.call("exists", json!({"path": "test.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);
	}

	#[tokio::test]
	async fn test_delete_file() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create file
		bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": "test",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// Delete file
		let result = bridge
			.call("delete", json!({"path": "test.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// File no longer exists
		let result = bridge
			.call("exists", json!({"path": "test.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);
	}

	#[tokio::test]
	async fn test_mkdir_and_list() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create directory
		let result = bridge
			.call("mkdir", json!({"path": "testdir"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Create files in directory
		bridge
			.call(
				"write",
				json!({
					"path": "testdir/file1.txt",
					"content": "file1",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		bridge
			.call(
				"write",
				json!({
					"path": "testdir/file2.txt",
					"content": "file2",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// List directory
		let result = bridge
			.call("list", json!({"path": "testdir"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		let entries = result["data"]["entries"].as_array().unwrap();
		assert_eq!(entries.len(), 2);
	}

	#[tokio::test]
	async fn test_list_with_pattern() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create directory
		bridge
			.call("mkdir", json!({"path": "testdir"}))
			.await
			.unwrap();

		// Create files
		bridge
			.call(
				"write",
				json!({
					"path": "testdir/file1.txt",
					"content": "file1",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		bridge
			.call(
				"write",
				json!({
					"path": "testdir/file2.rs",
					"content": "file2",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// List with pattern
		let result = bridge
			.call("list", json!({"path": "testdir", "pattern": "*.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		let entries = result["data"]["entries"].as_array().unwrap();
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0]["name"], "file1.txt");
	}

	#[tokio::test]
	async fn test_stat() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create file
		bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": "Hello, World!",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// Get stats
		let result = bridge
			.call("stat", json!({"path": "test.txt"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["is_file"], true);
		assert_eq!(result["data"]["is_directory"], false);
		assert_eq!(result["data"]["size"], 13);
	}

	#[tokio::test]
	async fn test_path_traversal_protection() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Attempt directory traversal
		let result = bridge
			.call(
				"read",
				json!({
					"path": "../../../etc/passwd",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "PATH_TRAVERSAL");
	}

	#[tokio::test]
	async fn test_file_size_limit() {
		let (_temp, _) = create_test_sandbox().await;

		// Attempt to write content larger than limit
		let large_content = "a".repeat(200);

		let temp_dir = TempDir::new().unwrap();
		let bridge_with_sandbox = FsBridge::with_sandbox(temp_dir.path().to_path_buf())
			.with_max_file_size(100);

		let result = bridge_with_sandbox
			.call(
				"write",
				json!({
					"path": "large.txt",
					"content": large_content,
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "FILE_TOO_LARGE");
	}

	#[tokio::test]
	async fn test_mkdir_recursive() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create nested directories
		let result = bridge
			.call(
				"mkdir",
				json!({
					"path": "a/b/c",
					"recursive": true
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Verify directory exists
		let result = bridge
			.call("exists", json!({"path": "a/b/c"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], true);
	}

	#[tokio::test]
	async fn test_delete_directory_recursive() {
		let (_temp, bridge) = create_test_sandbox().await;

		// Create directory with files
		bridge
			.call(
				"mkdir",
				json!({
					"path": "testdir",
					"recursive": true
				}),
			)
			.await
			.unwrap();

		bridge
			.call(
				"write",
				json!({
					"path": "testdir/file.txt",
					"content": "test",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		// Delete recursively
		let result = bridge
			.call(
				"delete",
				json!({
					"path": "testdir",
					"recursive": true
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], true);

		// Verify directory is gone
		let result = bridge
			.call("exists", json!({"path": "testdir"}))
			.await
			.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"], false);
	}

	#[tokio::test]
	async fn test_invalid_encoding() {
		let (_temp, bridge) = create_test_sandbox().await;

		let result = bridge
			.call(
				"write",
				json!({
					"path": "test.txt",
					"content": "test",
					"encoding": "invalid"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_read_nonexistent_file() {
		let (_temp, bridge) = create_test_sandbox().await;

		let result = bridge
			.call(
				"read",
				json!({
					"path": "nonexistent.txt",
					"encoding": "utf-8"
				}),
			)
			.await
			.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "NOT_FOUND");
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let bridge = FsBridge::new();
		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "FS_ERROR");
	}
}
