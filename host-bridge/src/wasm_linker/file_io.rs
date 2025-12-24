//! File I/O Host Functions
//!
//! Provides file system operations for WASM modules:
//! - file_read: Read file contents
//! - file_write: Write to a file
//! - file_exists: Check if file exists
//! - file_delete: Delete a file
//! - file_append: Append to a file
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use std::fs;
use std::io::Write;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register all file I/O functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // FILE READ
    // =========================================

    // file_read - Read file contents
    // Args: path_ptr, path_len
    // Returns: pointer to file contents as length-prefixed string
    linker.func_wrap(
        "env",
        "file_read",
        |mut caller: Caller<'_, S>, path_ptr: i32, path_len: i32| -> i32 {
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
    // Args: path_ptr, path_len, content_ptr, content_len
    // Returns: 0 on success, -1 on error
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
                    return -1;
                }
            };

            let content = match read_raw_string(&mut caller, content_ptr, content_len) {
                Some(s) => s,
                None => {
                    error!("file_write: Failed to read content");
                    return -1;
                }
            };

            debug!("file_write: path={}, content_len={}", path, content.len());

            match fs::write(&path, content) {
                Ok(_) => 0,
                Err(e) => {
                    error!("file_write: Failed to write file '{}': {}", path, e);
                    -1
                }
            }
        },
    )?;

    // =========================================
    // FILE EXISTS
    // =========================================

    // file_exists - Check if file exists
    // Args: path_ptr, path_len
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
    // Args: path_ptr, path_len
    // Returns: 0 on success, -1 on error
    linker.func_wrap(
        "env",
        "file_delete",
        |mut caller: Caller<'_, S>, path_ptr: i32, path_len: i32| -> i32 {
            let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => {
                    error!("file_delete: Failed to read path");
                    return -1;
                }
            };

            debug!("file_delete: path={}", path);

            match fs::remove_file(&path) {
                Ok(_) => 0,
                Err(e) => {
                    error!("file_delete: Failed to delete file '{}': {}", path, e);
                    -1
                }
            }
        },
    )?;

    // =========================================
    // FILE APPEND
    // =========================================

    // file_append - Append to a file
    // Args: path_ptr, path_len, content_ptr, content_len
    // Returns: 0 on success, -1 on error
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
                    return -1;
                }
            };

            let content = match read_raw_string(&mut caller, content_ptr, content_len) {
                Some(s) => s,
                None => {
                    error!("file_append: Failed to read content");
                    return -1;
                }
            };

            debug!("file_append: path={}, content_len={}", path, content.len());

            match fs::OpenOptions::new().append(true).create(true).open(&path) {
                Ok(mut file) => match file.write_all(content.as_bytes()) {
                    Ok(_) => 0,
                    Err(e) => {
                        error!("file_append: Failed to write to file '{}': {}", path, e);
                        -1
                    }
                },
                Err(e) => {
                    error!("file_append: Failed to open file '{}': {}", path, e);
                    -1
                }
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // File I/O tests would require temp file setup
}
