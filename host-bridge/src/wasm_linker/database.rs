//! Database Host Functions
//!
//! Provides database operations for WASM modules:
//! - _db_query: Execute SELECT queries
//! - _db_execute: Execute INSERT/UPDATE/DELETE
//! - _db_begin, _db_commit, _db_rollback: Transaction management
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use serde_json::json;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register all database functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // DATABASE QUERY
    // =========================================

    // _db_query - Execute a SELECT query
    // Args: sql_ptr, sql_len, params_ptr, params_len (JSON array of params)
    // Returns: pointer to JSON string with query results
    linker.func_wrap(
        "env",
        "_db_query",
        |mut caller: Caller<'_, S>,
         sql_ptr: i32,
         sql_len: i32,
         params_ptr: i32,
         params_len: i32|
         -> i32 {
            // Read SQL string from WASM memory
            let sql = match read_raw_string(&mut caller, sql_ptr, sql_len) {
                Some(s) => s,
                None => {
                    error!("_db_query: Failed to read SQL string");
                    return write_string_to_caller(
                        &mut caller,
                        "{\"ok\":false,\"err\":{\"code\":\"MEMORY_ERROR\",\"message\":\"Failed to read SQL\"}}",
                    );
                }
            };

            // Read params JSON from WASM memory
            let params_json = if params_len > 0 {
                read_raw_string(&mut caller, params_ptr, params_len).unwrap_or_else(|| "[]".to_string())
            } else {
                "[]".to_string()
            };

            debug!("_db_query: SQL={}, params={}", sql, params_json);

            // Parse params
            let params: Vec<serde_json::Value> =
                serde_json::from_str(&params_json).unwrap_or_default();

            // Get the database bridge from state
            let db_bridge = match caller.data().db_bridge() {
                Some(db) => db,
                None => {
                    return write_string_to_caller(
                        &mut caller,
                        "{\"ok\":false,\"err\":{\"code\":\"NO_DB\",\"message\":\"No database configured\"}}",
                    );
                }
            };

            // Execute query using tokio runtime
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut bridge = db_bridge.write().await;
                    bridge
                        .call(
                            "query",
                            json!({
                                "sql": sql,
                                "params": params
                            }),
                        )
                        .await
                })
            });

            // Convert result to JSON string and return
            let result_str = match result {
                Ok(v) => v.to_string(),
                Err(e) => {
                    json!({
                        "ok": false,
                        "err": {
                            "code": "DB_ERROR",
                            "message": e.to_string()
                        }
                    })
                    .to_string()
                }
            };

            write_string_to_caller(&mut caller, &result_str)
        },
    )?;

    // =========================================
    // DATABASE EXECUTE
    // =========================================

    // _db_execute - Execute an INSERT/UPDATE/DELETE
    // Args: sql_ptr, sql_len, params_ptr, params_len (JSON array of params)
    // Returns: number of affected rows (or -1 on error)
    linker.func_wrap(
        "env",
        "_db_execute",
        |mut caller: Caller<'_, S>,
         sql_ptr: i32,
         sql_len: i32,
         params_ptr: i32,
         params_len: i32|
         -> i32 {
            // Read SQL string from WASM memory
            let sql = match read_raw_string(&mut caller, sql_ptr, sql_len) {
                Some(s) => s,
                None => {
                    error!("_db_execute: Failed to read SQL string");
                    return -1;
                }
            };

            // Read params JSON from WASM memory
            let params_json = if params_len > 0 {
                read_raw_string(&mut caller, params_ptr, params_len).unwrap_or_else(|| "[]".to_string())
            } else {
                "[]".to_string()
            };

            debug!("_db_execute: SQL={}, params={}", sql, params_json);

            // Parse params
            let params: Vec<serde_json::Value> =
                serde_json::from_str(&params_json).unwrap_or_default();

            // Get the database bridge from state
            let db_bridge = match caller.data().db_bridge() {
                Some(db) => db,
                None => {
                    error!("_db_execute: No database configured");
                    return -1;
                }
            };

            // Execute command using tokio runtime
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut bridge = db_bridge.write().await;
                    bridge
                        .call(
                            "execute",
                            json!({
                                "sql": sql,
                                "params": params
                            }),
                        )
                        .await
                })
            });

            // Extract affected rows from result
            match result {
                Ok(v) => {
                    if let Some(ok) = v.get("ok").and_then(|o| o.as_bool()) {
                        if ok {
                            v.get("data")
                                .and_then(|d| d.get("affected_rows"))
                                .and_then(|r| r.as_i64())
                                .unwrap_or(0) as i32
                        } else {
                            error!("_db_execute: Database execute failed: {:?}", v.get("err"));
                            -1
                        }
                    } else {
                        0
                    }
                }
                Err(e) => {
                    error!("_db_execute: Database error: {}", e);
                    -1
                }
            }
        },
    )?;

    // =========================================
    // TRANSACTIONS
    // =========================================

    // _db_begin - Begin a transaction
    // Returns: transaction ID as string pointer, or 0 on error
    linker.func_wrap(
        "env",
        "_db_begin",
        |mut caller: Caller<'_, S>| -> i32 {
            let db_bridge = match caller.data().db_bridge() {
                Some(db) => db,
                None => {
                    error!("_db_begin: No database configured");
                    return 0;
                }
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut bridge = db_bridge.write().await;
                    bridge.call("transaction_begin", json!({})).await
                })
            });

            match result {
                Ok(v) => {
                    if let Some(tx_id) = v
                        .get("data")
                        .and_then(|d| d.get("tx_id"))
                        .and_then(|t| t.as_str())
                    {
                        write_string_to_caller(&mut caller, tx_id)
                    } else {
                        0
                    }
                }
                Err(e) => {
                    error!("_db_begin: Transaction begin error: {}", e);
                    0
                }
            }
        },
    )?;

    // _db_commit - Commit a transaction
    // Args: tx_id_ptr, tx_id_len
    // Returns: 0 on success, -1 on error
    linker.func_wrap(
        "env",
        "_db_commit",
        |mut caller: Caller<'_, S>, tx_id_ptr: i32, tx_id_len: i32| -> i32 {
            let tx_id = match read_raw_string(&mut caller, tx_id_ptr, tx_id_len) {
                Some(s) => s,
                None => return -1,
            };

            let db_bridge = match caller.data().db_bridge() {
                Some(db) => db,
                None => return -1,
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut bridge = db_bridge.write().await;
                    bridge
                        .call("transaction_commit", json!({ "tx_id": tx_id }))
                        .await
                })
            });

            match result {
                Ok(v) => {
                    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                        0
                    } else {
                        -1
                    }
                }
                Err(_) => -1,
            }
        },
    )?;

    // _db_rollback - Rollback a transaction
    // Args: tx_id_ptr, tx_id_len
    // Returns: 0 on success, -1 on error
    linker.func_wrap(
        "env",
        "_db_rollback",
        |mut caller: Caller<'_, S>, tx_id_ptr: i32, tx_id_len: i32| -> i32 {
            let tx_id = match read_raw_string(&mut caller, tx_id_ptr, tx_id_len) {
                Some(s) => s,
                None => return -1,
            };

            let db_bridge = match caller.data().db_bridge() {
                Some(db) => db,
                None => return -1,
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut bridge = db_bridge.write().await;
                    bridge
                        .call("transaction_rollback", json!({ "tx_id": tx_id }))
                        .await
                })
            });

            match result {
                Ok(v) => {
                    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                        0
                    } else {
                        -1
                    }
                }
                Err(_) => -1,
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Database tests require actual database connection
}
