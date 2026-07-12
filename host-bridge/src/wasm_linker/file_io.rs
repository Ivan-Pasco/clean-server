//! File I/O Host Functions
//!
//! Provides file system operations for WASM modules:
//! - file_read: Read file contents
//! - file_write: Write to a file
//! - file_exists: Check if file exists
//! - file_delete: Delete a file
//! - file_append: Append to a file
//!
//! String parameters use raw (ptr, len) pairs via expand_strings convention.
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use base64::Engine as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Ensure the parent directory of `path` exists, creating it (and any missing
/// ancestors) if necessary. Returns Ok on success or if `path` has no parent
/// component (e.g. a bare filename). Returns Err if directory creation fails.
fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        // Path::parent() returns Some("") for bare filenames like "foo.md";
        // skip the call in that case so we don't attempt to create an empty path.
        if !parent.as_os_str().is_empty() {
            return fs::create_dir_all(parent);
        }
    }
    Ok(())
}

/// Write `content` to `path`, creating intermediate parent directories as
/// needed. Returns 1 on success, 0 on failure.
fn write_file_with_parents(path: &str, content: &str) -> i32 {
    if let Err(e) = ensure_parent_dir(path) {
        error!(
            "file_write: Failed to create parent directories for '{}': {}",
            path, e
        );
        return 0;
    }
    match fs::write(path, content) {
        Ok(_) => 1,
        Err(e) => {
            error!("file_write: Failed to write file '{}': {}", path, e);
            0
        }
    }
}

/// Append `content` to `path`, creating intermediate parent directories and
/// the file itself if needed. Returns 1 on success, 0 on failure.
fn append_file_with_parents(path: &str, content: &str) -> i32 {
    if let Err(e) = ensure_parent_dir(path) {
        error!(
            "file_append: Failed to create parent directories for '{}': {}",
            path, e
        );
        return 0;
    }
    match fs::OpenOptions::new().append(true).create(true).open(path) {
        Ok(mut file) => match file.write_all(content.as_bytes()) {
            Ok(_) => 1,
            Err(e) => {
                error!("file_append: Failed to write to file '{}': {}", path, e);
                0
            }
        },
        Err(e) => {
            error!("file_append: Failed to open file '{}': {}", path, e);
            0
        }
    }
}

/// Register all file I/O functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // FILE READ
    // =========================================

    // file_read - Read file contents
    // Signature: (path_ptr: i32, path_len: i32, _mode: i32) -> i32
    // Returns: pointer to file contents as length-prefixed string
    linker.func_wrap(
        "env",
        "file_read",
        |mut caller: Caller<'_, S>, path_ptr: i32, path_len: i32, _mode: i32| -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_read: Failed to read path");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            debug!("file_read: path={}", path);

            match fs::read_to_string(&path) {
                Ok(contents) => write_string_to_caller(&mut caller, &contents),
                Err(e) => {
                    error!("file_read: Failed to read file '{}': {}", path, e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // =========================================
    // FILE WRITE
    // =========================================

    // file_write - Write to a file (overwrites if exists)
    // Signature: (path_ptr: i32, path_len: i32, content_ptr: i32, content_len: i32) -> i32 (boolean)
    // Returns: 1 on success, 0 on failure
    linker.func_wrap(
        "env",
        "file_write",
        |mut caller: Caller<'_, S>,
         path_ptr: i32,
         path_len: i32,
         content_ptr: i32,
         content_len: i32|
         -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_write: Failed to read path");
                    return 0;
                }
            };

            let content = match read_raw_string(&mut caller, content_ptr, content_len) {
                Some(s) => s,
                None => {
                    error!("file_write: Failed to read content");
                    return 0;
                }
            };

            debug!("file_write: path={}, content_len={}", path, content.len());

            write_file_with_parents(&path, &content)
        },
    )?;

    // =========================================
    // FILE EXISTS
    // =========================================

    // file_exists - Check if file exists
    // Signature: (path_ptr: i32, path_len: i32) -> i32
    // Returns: 1 if exists, 0 if not
    linker.func_wrap(
        "env",
        "file_exists",
        |mut caller: Caller<'_, S>, path_ptr: i32, path_len: i32| -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_exists: Failed to read path");
                    return 0;
                }
            };

            debug!("file_exists: path={}", path);

            if std::path::Path::new(&path).exists() {
                1
            } else {
                0
            }
        },
    )?;

    // =========================================
    // FILE DELETE
    // =========================================

    // file_delete - Delete a file
    // Signature: (path_ptr: i32, path_len: i32) -> i32 (boolean)
    // Returns: 1 on success, 0 on failure
    linker.func_wrap(
        "env",
        "file_delete",
        |mut caller: Caller<'_, S>, path_ptr: i32, path_len: i32| -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_delete: Failed to read path");
                    return 0;
                }
            };

            debug!("file_delete: path={}", path);

            match fs::remove_file(&path) {
                Ok(_) => 1,
                Err(e) => {
                    error!("file_delete: Failed to delete file '{}': {}", path, e);
                    0
                }
            }
        },
    )?;

    // =========================================
    // FILE APPEND
    // =========================================

    // file_append - Append to a file
    // Signature: (path_ptr: i32, path_len: i32, content_ptr: i32, content_len: i32) -> i32 (boolean)
    // Returns: 1 on success, 0 on failure
    linker.func_wrap(
        "env",
        "file_append",
        |mut caller: Caller<'_, S>,
         path_ptr: i32,
         path_len: i32,
         content_ptr: i32,
         content_len: i32|
         -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_append: Failed to read path");
                    return 0;
                }
            };

            let content = match read_raw_string(&mut caller, content_ptr, content_len) {
                Some(s) => s,
                None => {
                    error!("file_append: Failed to read content");
                    return 0;
                }
            };

            debug!("file_append: path={}, content_len={}", path, content.len());

            append_file_with_parents(&path, &content)
        },
    )?;

    // =========================================
    // FILE EXTRAS (registry hosts = ["server"])
    // =========================================

    // file_size(string) -> i32 — bytes, -1 on error
    linker.func_wrap(
        "env",
        "file_size",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return -1,
            };
            match fs::metadata(&path) {
                Ok(m) => m.len() as i32,
                Err(_) => -1,
            }
        },
    )?;

    // file_list_dir(string) -> ptr (LP string with JSON array of names)
    linker.func_wrap(
        "env",
        "file_list_dir",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, "[]"),
            };
            let names: Vec<String> = match fs::read_dir(&path) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect(),
                Err(_) => Vec::new(),
            };
            let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
            write_string_to_caller(&mut caller, &json)
        },
    )?;

    // file_mkdir(string) -> i32 (1 on success, 0 on failure) — creates intermediates
    linker.func_wrap(
        "env",
        "file_mkdir",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return 0,
            };
            if fs::create_dir_all(&path).is_ok() {
                1
            } else {
                0
            }
        },
    )?;

    // file_copy(string, string) -> i32
    linker.func_wrap(
        "env",
        "file_copy",
        |mut caller: Caller<'_, S>, sp: i32, sl: i32, dp: i32, dl: i32| -> i32 {
            let src = match read_raw_string(&mut caller, sp, sl) {
                Some(s) => s,
                None => return 0,
            };
            let dst = match read_raw_string(&mut caller, dp, dl) {
                Some(s) => s,
                None => return 0,
            };
            if let Err(e) = ensure_parent_dir(&dst) {
                error!("file_copy: parent dir for '{}': {}", dst, e);
                return 0;
            }
            if fs::copy(&src, &dst).is_ok() {
                1
            } else {
                0
            }
        },
    )?;

    // file_rename(string, string) -> i32
    linker.func_wrap(
        "env",
        "file_rename",
        |mut caller: Caller<'_, S>, sp: i32, sl: i32, dp: i32, dl: i32| -> i32 {
            let src = match read_raw_string(&mut caller, sp, sl) {
                Some(s) => s,
                None => return 0,
            };
            let dst = match read_raw_string(&mut caller, dp, dl) {
                Some(s) => s,
                None => return 0,
            };
            if let Err(e) = ensure_parent_dir(&dst) {
                error!("file_rename: parent dir for '{}': {}", dst, e);
                return 0;
            }
            if fs::rename(&src, &dst).is_ok() {
                1
            } else {
                0
            }
        },
    )?;

    // file_is_directory(string) -> boolean
    linker.func_wrap(
        "env",
        "file_is_directory",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return 0,
            };
            if Path::new(&path).is_dir() {
                1
            } else {
                0
            }
        },
    )?;

    // file_read_binary(string) -> ptr — base64-encoded file contents in an LP string
    linker.func_wrap(
        "env",
        "file_read_binary",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    error!("file_read_binary: '{}': {}", path, e);
                    return write_string_to_caller(&mut caller, "");
                }
            };
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            write_string_to_caller(&mut caller, &encoded)
        },
    )?;

    // file_write_binary(string, string) -> i32 — second string is base64 of bytes to write
    linker.func_wrap(
        "env",
        "file_write_binary",
        |mut caller: Caller<'_, S>, pp: i32, pl: i32, bp: i32, bl: i32| -> i32 {
            let path = match read_raw_string(&mut caller, pp, pl) {
                Some(s) => s,
                None => return 0,
            };
            let b64 = match read_raw_string(&mut caller, bp, bl) {
                Some(s) => s,
                None => return 0,
            };
            let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
                Ok(b) => b,
                Err(e) => {
                    error!(
                        "file_write_binary: base64 decode failed for '{}': {}",
                        path, e
                    );
                    return 0;
                }
            };
            if let Err(e) = ensure_parent_dir(&path) {
                error!("file_write_binary: parent for '{}': {}", path, e);
                return 0;
            }
            if fs::write(&path, &bytes).is_ok() {
                1
            } else {
                0
            }
        },
    )?;

    // file_rmdir(string) -> i32 — recursive removal
    linker.func_wrap(
        "env",
        "file_rmdir",
        |mut caller: Caller<'_, S>, p: i32, l: i32| -> i32 {
            let path = match read_raw_string(&mut caller, p, l) {
                Some(s) => s,
                None => return 0,
            };
            if fs::remove_dir_all(&path).is_ok() {
                1
            } else {
                0
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---------- file_write ----------

    #[test]
    fn file_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c/file.md");
        let result = write_file_with_parents(path.to_str().unwrap(), "hello");
        assert_eq!(result, 1, "write_file_with_parents should return 1");
        assert!(path.exists(), "file should exist on disk");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn file_write_existing_parent_still_works() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("file.md");
        let result = write_file_with_parents(path.to_str().unwrap(), "hi");
        assert_eq!(result, 1);
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hi");
    }

    #[test]
    fn file_write_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested/file.md");
        assert_eq!(write_file_with_parents(path.to_str().unwrap(), "first"), 1);
        assert_eq!(write_file_with_parents(path.to_str().unwrap(), "second"), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn file_write_invalid_path_returns_zero() {
        // Create a regular file, then try to use it AS a parent directory for
        // another write. create_dir_all should fail because the "parent" is a file.
        let tmp = TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "i am a file").unwrap();
        let bad_path = blocker.join("child/file.md");
        let result = write_file_with_parents(bad_path.to_str().unwrap(), "data");
        assert_eq!(result, 0, "should return 0 when parent cannot be created");
    }

    #[test]
    fn file_write_bare_filename_no_parent_component() {
        // ensure_parent_dir should be a no-op for bare filenames; the actual
        // write happens in the current directory of the test process. Use a
        // tempdir as cwd via path joining instead so we don't pollute cwd.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("just-a-name.txt");
        assert_eq!(write_file_with_parents(path.to_str().unwrap(), "x"), 1);
        assert!(path.exists());
    }

    // ---------- file_append ----------

    #[test]
    fn file_append_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c/log.txt");
        let result = append_file_with_parents(path.to_str().unwrap(), "line1\n");
        assert_eq!(result, 1);
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "line1\n");
    }

    #[test]
    fn file_append_existing_parent_still_works() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("log.txt");
        assert_eq!(append_file_with_parents(path.to_str().unwrap(), "a"), 1);
        assert_eq!(append_file_with_parents(path.to_str().unwrap(), "b"), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ab");
    }

    #[test]
    fn file_append_invalid_path_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "i am a file").unwrap();
        let bad_path = blocker.join("child/log.txt");
        let result = append_file_with_parents(bad_path.to_str().unwrap(), "data");
        assert_eq!(result, 0);
    }
}
