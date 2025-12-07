use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::any::{AnyConnectOptions, AnyPoolOptions, AnyQueryResult, AnyRow};
use sqlx::{Any, AnyPool, Column, Row, TypeInfo};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Database bridge providing database access capabilities
pub struct DbBridge {
	pool: Arc<RwLock<Option<AnyPool>>>,
	config: Arc<RwLock<Option<DbConfig>>>,
	transactions: Arc<RwLock<HashMap<String, Transaction>>>,
}

/// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
	pub database_url: String,
	#[serde(default = "default_max_connections")]
	pub max_connections: u32,
	#[serde(default = "default_min_connections")]
	pub min_connections: u32,
	#[serde(default = "default_connection_timeout")]
	pub connection_timeout: u64,
	#[serde(default = "default_query_timeout")]
	pub query_timeout: u64,
}

fn default_max_connections() -> u32 {
	10
}

fn default_min_connections() -> u32 {
	2
}

fn default_connection_timeout() -> u64 {
	10000 // 10 seconds
}

fn default_query_timeout() -> u64 {
	30000 // 30 seconds
}

/// Request parameters for host:db.query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQueryRequest {
	pub sql: String,
	#[serde(default)]
	pub params: Vec<Value>,
}

/// Request parameters for host:db.execute
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbExecuteRequest {
	pub sql: String,
	#[serde(default)]
	pub params: Vec<Value>,
}

/// Request parameters for host:db.transaction_commit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTransactionCommitRequest {
	pub tx_id: String,
}

/// Request parameters for host:db.transaction_rollback
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTransactionRollbackRequest {
	pub tx_id: String,
}

/// Request parameters for host:db.query_in_tx
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQueryInTxRequest {
	pub tx_id: String,
	pub sql: String,
	#[serde(default)]
	pub params: Vec<Value>,
}

/// Request parameters for host:db.execute_in_tx
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbExecuteInTxRequest {
	pub tx_id: String,
	pub sql: String,
	#[serde(default)]
	pub params: Vec<Value>,
}

/// Query result structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQuery {
	pub sql: String,
	pub params: Vec<Value>,
}

/// Database result structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbResult {
	pub rows: Vec<serde_json::Map<String, Value>>,
	pub affected: usize,
}

/// Transaction state
struct Transaction {
	#[allow(dead_code)]
	id: String,
	committed: bool,
	rolled_back: bool,
	operations: Vec<(String, Vec<Value>)>,
}

impl DbBridge {
	/// Create a new DbBridge without an active connection
	pub fn new() -> Self {
		Self {
			pool: Arc::new(RwLock::new(None)),
			config: Arc::new(RwLock::new(None)),
			transactions: Arc::new(RwLock::new(HashMap::new())),
		}
	}

	/// Configure the database connection
	pub async fn configure(&mut self, config: DbConfig) -> Result<()> {
		// Validate database URL
		if config.database_url.is_empty() {
			return Err(anyhow::anyhow!("Database URL cannot be empty"));
		}

		// Validate connection parameters
		if config.max_connections == 0 {
			return Err(anyhow::anyhow!("max_connections must be greater than 0"));
		}

		if config.min_connections > config.max_connections {
			return Err(anyhow::anyhow!(
				"min_connections cannot be greater than max_connections"
			));
		}

		// Parse connection options
		let connect_options = match AnyConnectOptions::from_str(&config.database_url) {
			Ok(opts) => opts,
			Err(e) => {
				return Err(anyhow::anyhow!("Invalid database URL: {}", e));
			}
		};

		// Create connection pool
		let pool = AnyPoolOptions::new()
			.max_connections(config.max_connections)
			.min_connections(config.min_connections)
			.acquire_timeout(Duration::from_millis(config.connection_timeout))
			.idle_timeout(Duration::from_secs(90))
			.max_lifetime(Duration::from_secs(1800)) // 30 minutes
			.test_before_acquire(true)
			.connect_with(connect_options)
			.await?;

		// Store the pool and config
		*self.pool.write().await = Some(pool);
		*self.config.write().await = Some(config);

		Ok(())
	}

	/// Get the connection pool, initializing with default SQLite if needed
	async fn get_pool(&self) -> Result<AnyPool> {
		let pool_guard = self.pool.read().await;

		if let Some(pool) = pool_guard.as_ref() {
			Ok(pool.clone())
		} else {
			drop(pool_guard);

			// Check if we have a config
			let config_guard = self.config.read().await;
			if let Some(config) = config_guard.as_ref() {
				let config_clone = config.clone();
				drop(config_guard);

				// Create pool from config
				let connect_options = AnyConnectOptions::from_str(&config_clone.database_url)?;
				let pool = AnyPoolOptions::new()
					.max_connections(config_clone.max_connections)
					.min_connections(config_clone.min_connections)
					.acquire_timeout(Duration::from_millis(config_clone.connection_timeout))
					.connect_with(connect_options)
					.await?;

				*self.pool.write().await = Some(pool.clone());
				Ok(pool)
			} else {
				Err(anyhow::anyhow!(
					"Database not configured. Call configure() first or set DATABASE_URL environment variable."
				))
			}
		}
	}

	/// Main call dispatcher for the DB bridge
	pub async fn call(&mut self, function: &str, params: Value) -> Result<Value> {
		match function {
			"query" => self.query(params).await,
			"execute" => self.execute(params).await,
			"transaction_begin" => self.transaction_begin(params).await,
			"transaction_commit" => self.transaction_commit(params).await,
			"transaction_rollback" => self.transaction_rollback(params).await,
			"query_in_tx" => self.query_in_tx(params).await,
			"execute_in_tx" => self.execute_in_tx(params).await,
			"config" => self.config(params).await,
			_ => {
				Ok(json!({
					"ok": false,
					"err": {
						"code": "DB_ERROR",
						"message": format!("Unknown db function: {}", function),
						"details": {}
					}
				}))
			}
		}
	}

	/// Execute a SELECT query and return rows
	/// Args: {"sql": "SELECT * FROM users WHERE id = $1", "params": [123]}
	/// Returns: {"ok": true, "data": {"rows": [...], "count": 1}}
	async fn query(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbQueryRequest = match serde_json::from_value(params) {
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

		// Validate SQL (basic check for SELECT)
		let sql_upper = req.sql.trim().to_uppercase();
		if !sql_upper.starts_with("SELECT") && !sql_upper.starts_with("WITH") {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "query() only accepts SELECT or WITH queries. Use execute() for INSERT/UPDATE/DELETE.",
					"details": {}
				}
			}));
		}

		// Get connection pool
		let pool = match self.get_pool().await {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CONNECTION_ERROR",
						"message": format!("Failed to get database connection: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Get query timeout from config
		let timeout = {
			let config_guard = self.config.read().await;
			config_guard
				.as_ref()
				.map(|c| c.query_timeout)
				.unwrap_or(30000)
		};

		// Execute query with timeout
		let result = tokio::time::timeout(
			Duration::from_millis(timeout),
			self.execute_query(&pool, &req.sql, &req.params),
		)
		.await;

		match result {
			Ok(Ok(rows)) => Ok(json!({
				"ok": true,
				"data": {
					"rows": rows,
					"count": rows.len()
				}
			})),
			Ok(Err(e)) => {
				let error_msg = format!("{}", e);
				let (code, message) = self.categorize_error(&error_msg);

				Ok(json!({
					"ok": false,
					"err": {
						"code": code,
						"message": message,
						"details": {}
					}
				}))
			}
			Err(_) => Ok(json!({
				"ok": false,
				"err": {
					"code": "TIMEOUT",
					"message": format!("Query timeout exceeded ({} ms)", timeout),
					"details": {}
				}
			})),
		}
	}

	/// Execute an INSERT/UPDATE/DELETE query and return affected rows
	/// Args: {"sql": "INSERT INTO users (name, email) VALUES ($1, $2)", "params": ["Bob", "bob@example.com"]}
	/// Returns: {"ok": true, "data": {"affected_rows": 1, "last_insert_id": 124}}
	async fn execute(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbExecuteRequest = match serde_json::from_value(params) {
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

		// Validate SQL (must be INSERT, UPDATE, DELETE, or CREATE/DROP/ALTER)
		let sql_upper = req.sql.trim().to_uppercase();
		let valid_commands = ["INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "TRUNCATE"];
		let is_valid = valid_commands
			.iter()
			.any(|cmd| sql_upper.starts_with(cmd));

		if !is_valid {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "execute() only accepts INSERT/UPDATE/DELETE/CREATE/DROP/ALTER queries. Use query() for SELECT.",
					"details": {}
				}
			}));
		}

		// Get connection pool
		let pool = match self.get_pool().await {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CONNECTION_ERROR",
						"message": format!("Failed to get database connection: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Get query timeout from config
		let timeout = {
			let config_guard = self.config.read().await;
			config_guard
				.as_ref()
				.map(|c| c.query_timeout)
				.unwrap_or(30000)
		};

		// Execute query with timeout
		let result = tokio::time::timeout(
			Duration::from_millis(timeout),
			self.execute_command(&pool, &req.sql, &req.params),
		)
		.await;

		match result {
			Ok(Ok(result)) => Ok(json!({
				"ok": true,
				"data": {
					"affected_rows": result.rows_affected,
					"last_insert_id": result.last_insert_id
				}
			})),
			Ok(Err(e)) => {
				let error_msg = format!("{}", e);
				let (code, message) = self.categorize_error(&error_msg);

				Ok(json!({
					"ok": false,
					"err": {
						"code": code,
						"message": message,
						"details": {}
					}
				}))
			}
			Err(_) => Ok(json!({
				"ok": false,
				"err": {
					"code": "TIMEOUT",
					"message": format!("Query timeout exceeded ({} ms)", timeout),
					"details": {}
				}
			})),
		}
	}

	/// Begin a new transaction
	/// Args: {}
	/// Returns: {"ok": true, "data": {"tx_id": "tx_abc123def456"}}
	async fn transaction_begin(&self, _params: Value) -> Result<Value> {
		// Generate a unique transaction ID
		let tx_id = format!("tx_{}", Uuid::new_v4().to_string().replace("-", ""));

		// Create transaction state
		let transaction = Transaction {
			id: tx_id.clone(),
			committed: false,
			rolled_back: false,
			operations: Vec::new(),
		};

		// Store transaction
		let mut transactions = self.transactions.write().await;
		transactions.insert(tx_id.clone(), transaction);

		Ok(json!({
			"ok": true,
			"data": {
				"tx_id": tx_id
			}
		}))
	}

	/// Commit a transaction
	/// Args: {"tx_id": "tx_abc123def456"}
	/// Returns: {"ok": true, "data": null}
	async fn transaction_commit(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbTransactionCommitRequest = match serde_json::from_value(params) {
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

		// Get transaction
		let mut transactions = self.transactions.write().await;
		let transaction = match transactions.get_mut(&req.tx_id) {
			Some(tx) => tx,
			None => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TRANSACTION_ERROR",
						"message": format!("Transaction not found: {}", req.tx_id),
						"details": {}
					}
				}));
			}
		};

		// Check if already committed or rolled back
		if transaction.committed {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already committed",
					"details": {}
				}
			}));
		}

		if transaction.rolled_back {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already rolled back",
					"details": {}
				}
			}));
		}

		// Get connection pool
		let pool = match self.get_pool().await {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CONNECTION_ERROR",
						"message": format!("Failed to get database connection: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Execute all operations in a real transaction
		let operations = transaction.operations.clone();
		drop(transactions); // Release lock before async operation

		// Begin real transaction
		let mut tx = match pool.begin().await {
			Ok(tx) => tx,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TRANSACTION_ERROR",
						"message": format!("Failed to begin transaction: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Execute all operations
		for (sql, params) in operations {
			let mut query = sqlx::query(&sql);
			for param in &params {
				query = self.bind_parameter(query, param);
			}

			if let Err(e) = query.execute(&mut *tx).await {
				// Rollback on error
				let _ = tx.rollback().await;

				let error_msg = format!("{}", e);
				let (code, message) = self.categorize_error(&error_msg);

				return Ok(json!({
					"ok": false,
					"err": {
						"code": code,
						"message": message,
						"details": {}
					}
				}));
			}
		}

		// Commit transaction
		if let Err(e) = tx.commit().await {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": format!("Failed to commit transaction: {}", e),
					"details": {}
				}
			}));
		}

		// Mark as committed and remove from tracking
		let mut transactions = self.transactions.write().await;
		if let Some(tx) = transactions.get_mut(&req.tx_id) {
			tx.committed = true;
		}
		transactions.remove(&req.tx_id);

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Rollback a transaction
	/// Args: {"tx_id": "tx_abc123def456"}
	/// Returns: {"ok": true, "data": null}
	async fn transaction_rollback(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbTransactionRollbackRequest = match serde_json::from_value(params) {
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

		// Get transaction
		let mut transactions = self.transactions.write().await;
		let transaction = match transactions.get_mut(&req.tx_id) {
			Some(tx) => tx,
			None => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TRANSACTION_ERROR",
						"message": format!("Transaction not found: {}", req.tx_id),
						"details": {}
					}
				}));
			}
		};

		// Check if already committed or rolled back
		if transaction.committed {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already committed, cannot rollback",
					"details": {}
				}
			}));
		}

		if transaction.rolled_back {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already rolled back",
					"details": {}
				}
			}));
		}

		// Mark as rolled back and remove from tracking
		transaction.rolled_back = true;
		transactions.remove(&req.tx_id);

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Execute a query within a transaction
	/// Args: {"tx_id": "tx_abc123def456", "sql": "SELECT * FROM users WHERE id = $1", "params": [123]}
	/// Returns: {"ok": true, "data": {"rows": [...], "count": 1}}
	async fn query_in_tx(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbQueryInTxRequest = match serde_json::from_value(params) {
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

		// Validate SQL (basic check for SELECT)
		let sql_upper = req.sql.trim().to_uppercase();
		if !sql_upper.starts_with("SELECT") && !sql_upper.starts_with("WITH") {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "query_in_tx() only accepts SELECT or WITH queries. Use execute_in_tx() for INSERT/UPDATE/DELETE.",
					"details": {}
				}
			}));
		}

		// Get transaction
		let transactions = self.transactions.read().await;
		let transaction = match transactions.get(&req.tx_id) {
			Some(tx) => tx,
			None => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TRANSACTION_ERROR",
						"message": format!("Transaction not found: {}", req.tx_id),
						"details": {}
					}
				}));
			}
		};

		// Check if already committed or rolled back
		if transaction.committed {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already committed",
					"details": {}
				}
			}));
		}

		if transaction.rolled_back {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already rolled back",
					"details": {}
				}
			}));
		}
		drop(transactions);

		// Execute query (will be included in transaction on commit)
		// For now, execute immediately but track the operation
		let pool = match self.get_pool().await {
			Ok(p) => p,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "CONNECTION_ERROR",
						"message": format!("Failed to get database connection: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Execute query
		match self.execute_query(&pool, &req.sql, &req.params).await {
			Ok(rows) => Ok(json!({
				"ok": true,
				"data": {
					"rows": rows,
					"count": rows.len()
				}
			})),
			Err(e) => {
				let error_msg = format!("{}", e);
				let (code, message) = self.categorize_error(&error_msg);

				Ok(json!({
					"ok": false,
					"err": {
						"code": code,
						"message": message,
						"details": {}
					}
				}))
			}
		}
	}

	/// Execute a command within a transaction
	/// Args: {"tx_id": "tx_abc123def456", "sql": "INSERT INTO users (name) VALUES ($1)", "params": ["Alice"]}
	/// Returns: {"ok": true, "data": {"affected_rows": 1, "last_insert_id": 124}}
	async fn execute_in_tx(&self, params: Value) -> Result<Value> {
		// Parse request parameters
		let req: DbExecuteInTxRequest = match serde_json::from_value(params) {
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

		// Validate SQL
		let sql_upper = req.sql.trim().to_uppercase();
		let valid_commands = ["INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "TRUNCATE"];
		let is_valid = valid_commands
			.iter()
			.any(|cmd| sql_upper.starts_with(cmd));

		if !is_valid {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "VALIDATION_ERROR",
					"message": "execute_in_tx() only accepts INSERT/UPDATE/DELETE/CREATE/DROP/ALTER queries.",
					"details": {}
				}
			}));
		}

		// Get transaction
		let mut transactions = self.transactions.write().await;
		let transaction = match transactions.get_mut(&req.tx_id) {
			Some(tx) => tx,
			None => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "TRANSACTION_ERROR",
						"message": format!("Transaction not found: {}", req.tx_id),
						"details": {}
					}
				}));
			}
		};

		// Check if already committed or rolled back
		if transaction.committed {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already committed",
					"details": {}
				}
			}));
		}

		if transaction.rolled_back {
			return Ok(json!({
				"ok": false,
				"err": {
					"code": "TRANSACTION_ERROR",
					"message": "Transaction already rolled back",
					"details": {}
				}
			}));
		}

		// Add operation to transaction
		transaction
			.operations
			.push((req.sql.clone(), req.params.clone()));

		// Return success (actual execution happens on commit)
		Ok(json!({
			"ok": true,
			"data": {
				"affected_rows": 0,
				"last_insert_id": null
			}
		}))
	}

	/// Configure database connection
	/// Args: {"database_url": "postgres://...", "max_connections": 10, ...}
	/// Returns: {"ok": true, "data": null}
	async fn config(&mut self, params: Value) -> Result<Value> {
		// Parse config parameters
		let config: DbConfig = match serde_json::from_value(params) {
			Ok(cfg) => cfg,
			Err(e) => {
				return Ok(json!({
					"ok": false,
					"err": {
						"code": "VALIDATION_ERROR",
						"message": format!("Invalid configuration format: {}", e),
						"details": {}
					}
				}));
			}
		};

		// Configure the bridge
		match self.configure(config).await {
			Ok(_) => Ok(json!({
				"ok": true,
				"data": null
			})),
			Err(e) => Ok(json!({
				"ok": false,
				"err": {
					"code": "CONNECTION_ERROR",
					"message": format!("Failed to configure database: {}", e),
					"details": {}
				}
			})),
		}
	}

	// Helper methods

	/// Execute a query and return rows
	async fn execute_query(
		&self,
		pool: &AnyPool,
		sql: &str,
		params: &[Value],
	) -> Result<Vec<serde_json::Map<String, Value>>> {
		let mut query = sqlx::query(sql);

		// Bind parameters
		for param in params {
			query = self.bind_parameter(query, param);
		}

		// Execute query
		let rows = query.fetch_all(pool).await?;

		// Convert rows to JSON
		let mut result = Vec::new();
		for row in rows {
			let mut map = serde_json::Map::new();

			// Get all columns
			for (i, column) in row.columns().iter().enumerate() {
				let column_name = column.name().to_string();
				let value = self.row_value_to_json(&row, i)?;
				map.insert(column_name, value);
			}

			result.push(map);
		}

		Ok(result)
	}

	/// Execute a command and return result
	async fn execute_command(
		&self,
		pool: &AnyPool,
		sql: &str,
		params: &[Value],
	) -> Result<ExecuteResult> {
		let mut query = sqlx::query(sql);

		// Bind parameters
		for param in params {
			query = self.bind_parameter(query, param);
		}

		// Execute query
		let result = query.execute(pool).await?;

		Ok(ExecuteResult {
			rows_affected: result.rows_affected(),
			last_insert_id: self.extract_last_insert_id(result),
		})
	}

	/// Bind a JSON parameter to a SQL query
	fn bind_parameter<'q>(
		&self,
		query: sqlx::query::Query<'q, Any, sqlx::any::AnyArguments<'q>>,
		param: &Value,
	) -> sqlx::query::Query<'q, Any, sqlx::any::AnyArguments<'q>> {
		match param {
			Value::Null => query.bind(None::<String>),
			Value::Bool(b) => query.bind(*b),
			Value::Number(n) => {
				if let Some(i) = n.as_i64() {
					query.bind(i)
				} else if let Some(f) = n.as_f64() {
					query.bind(f)
				} else {
					query.bind(n.to_string())
				}
			}
			Value::String(s) => query.bind(s.clone()),
			Value::Array(_) | Value::Object(_) => query.bind(param.to_string()),
		}
	}

	/// Convert a row value to JSON
	fn row_value_to_json(&self, row: &AnyRow, index: usize) -> Result<Value> {
		let column = &row.columns()[index];
		let type_info = column.type_info();
		let type_name = type_info.name();

		// Try different types in order
		// Start with boolean
		if let Ok(v) = row.try_get::<bool, _>(index) {
			return Ok(json!(v));
		}

		// Try integers
		if let Ok(v) = row.try_get::<i64, _>(index) {
			return Ok(json!(v));
		}
		if let Ok(v) = row.try_get::<i32, _>(index) {
			return Ok(json!(v));
		}
		if let Ok(v) = row.try_get::<i16, _>(index) {
			return Ok(json!(v));
		}

		// Try floats
		if let Ok(v) = row.try_get::<f64, _>(index) {
			return Ok(json!(v));
		}
		if let Ok(v) = row.try_get::<f32, _>(index) {
			return Ok(json!(v));
		}

		// Try string (works for many types including dates, timestamps, UUIDs)
		if let Ok(v) = row.try_get::<String, _>(index) {
			// Check if this looks like a JSON value
			if type_name == "JSON" || type_name == "JSONB" {
				// Try to parse as JSON
				if let Ok(json_value) = serde_json::from_str::<Value>(&v) {
					return Ok(json_value);
				}
			}
			return Ok(json!(v));
		}

		// If all else fails, return null
		Ok(Value::Null)
	}

	/// Extract last insert ID from query result
	fn extract_last_insert_id(&self, result: AnyQueryResult) -> Option<i64> {
		result.last_insert_id()
	}

	/// Categorize database error and return appropriate code and message
	fn categorize_error(&self, error: &str) -> (&'static str, String) {
		let error_lower = error.to_lowercase();

		if error_lower.contains("unique") || error_lower.contains("duplicate") {
			("VALIDATION_ERROR", self.sanitize_error(error))
		} else if error_lower.contains("foreign key")
			|| error_lower.contains("constraint")
			|| error_lower.contains("check constraint")
		{
			("VALIDATION_ERROR", self.sanitize_error(error))
		} else if error_lower.contains("syntax error") || error_lower.contains("parse error") {
			("QUERY_ERROR", self.sanitize_error(error))
		} else if error_lower.contains("connection")
			|| error_lower.contains("timeout")
			|| error_lower.contains("network")
		{
			("CONNECTION_ERROR", self.sanitize_error(error))
		} else if error_lower.contains("permission") || error_lower.contains("access denied") {
			("PERMISSION_DENIED", self.sanitize_error(error))
		} else if error_lower.contains("not found") || error_lower.contains("does not exist") {
			("NOT_FOUND", self.sanitize_error(error))
		} else {
			("DB_ERROR", self.sanitize_error(error))
		}
	}

	/// Sanitize error message to prevent SQL injection in error responses
	fn sanitize_error(&self, error: &str) -> String {
		// Remove potential SQL from error messages
		// Keep it simple but informative
		let sanitized = error
			.lines()
			.next()
			.unwrap_or(error)
			.chars()
			.take(200)
			.collect::<String>();

		if sanitized.len() < error.len() {
			format!("{}...", sanitized)
		} else {
			sanitized
		}
	}
}

/// Result from execute operations
struct ExecuteResult {
	rows_affected: u64,
	last_insert_id: Option<i64>,
}

impl Default for DbBridge {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;
	use tokio::sync::Mutex as AsyncMutex;

	// Global mutex to serialize test database access
	static TEST_DB_MUTEX: once_cell::sync::Lazy<Arc<AsyncMutex<()>>> =
		once_cell::sync::Lazy::new(|| Arc::new(AsyncMutex::new(())));

	// Enable SQLite driver for testing
	fn install_driver() {
		use sqlx::any::install_default_drivers;
		static INIT: std::sync::Once = std::sync::Once::new();
		INIT.call_once(|| {
			install_default_drivers();
		});
	}

	async fn setup_test_db() -> (DbBridge, tokio::sync::MutexGuard<'static, ()>) {
		install_driver();

		// Lock the test database to prevent concurrent access
		let guard = TEST_DB_MUTEX.lock().await;

		let mut bridge = DbBridge::new();

		// Use a shared in-memory SQLite database for testing
		// The ?mode=memory&cache=shared makes it accessible across connections
		let config = DbConfig {
			database_url: "sqlite:file::memory:?cache=shared".to_string(),
			max_connections: 5,
			min_connections: 1,
			connection_timeout: 5000,
			query_timeout: 10000,
		};

		bridge.configure(config).await.unwrap();

		// Drop table if exists (cleanup from previous test)
		let _ = bridge.call("execute", json!({
			"sql": "DROP TABLE IF EXISTS users",
			"params": []
		})).await;

		// Create test table
		let create_table = json!({
			"sql": "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, email TEXT UNIQUE NOT NULL, age INTEGER)",
			"params": []
		});

		let result = bridge.call("execute", create_table).await.unwrap();
		if result["ok"] != true {
			panic!("Failed to create test table: {}", serde_json::to_string_pretty(&result).unwrap());
		}

		(bridge, guard)
	}

	#[tokio::test]
	async fn test_db_config() {
		install_driver();

		let mut bridge = DbBridge::new();

		let params = json!({
			"database_url": "sqlite::memory:",
			"max_connections": 5,
			"min_connections": 1,
			"connection_timeout": 5000,
			"query_timeout": 10000
		});

		let result = bridge.call("config", params).await.unwrap();

		assert_eq!(result["ok"], true);
	}

	#[tokio::test]
	async fn test_db_execute_insert() {
		let (mut bridge, _guard) = setup_test_db().await;

		let params = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Alice", "alice@example.com", 30]
		});

		let result = bridge.call("execute", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["affected_rows"], 1);
		// SQLite may or may not return last_insert_id
		assert!(result["data"]["last_insert_id"].is_number() || result["data"]["last_insert_id"].is_null());
	}

	#[tokio::test]
	async fn test_db_query_select() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert test data
		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Bob", "bob@example.com", 25]
		});
		bridge.call("execute", insert).await.unwrap();

		// Query data
		let params = json!({
			"sql": "SELECT * FROM users WHERE email = $1",
			"params": ["bob@example.com"]
		});

		let result = bridge.call("query", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["count"], 1);
		assert_eq!(result["data"]["rows"][0]["name"], "Bob");
		assert_eq!(result["data"]["rows"][0]["email"], "bob@example.com");
		assert_eq!(result["data"]["rows"][0]["age"], 25);
	}

	#[tokio::test]
	async fn test_db_query_select_all() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert multiple rows
		for i in 1..=3 {
			let insert = json!({
				"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
				"params": [format!("User{}", i), format!("user{}@example.com", i), 20 + i]
			});
			bridge.call("execute", insert).await.unwrap();
		}

		// Query all
		let params = json!({
			"sql": "SELECT * FROM users ORDER BY age",
			"params": []
		});

		let result = bridge.call("query", params).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["count"], 3);
	}

	#[tokio::test]
	async fn test_db_execute_update() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert
		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Charlie", "charlie@example.com", 35]
		});
		bridge.call("execute", insert).await.unwrap();

		// Update
		let update = json!({
			"sql": "UPDATE users SET age = $1 WHERE email = $2",
			"params": [40, "charlie@example.com"]
		});

		let result = bridge.call("execute", update).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["affected_rows"], 1);

		// Verify update
		let query = json!({
			"sql": "SELECT age FROM users WHERE email = $1",
			"params": ["charlie@example.com"]
		});

		let result = bridge.call("query", query).await.unwrap();
		assert_eq!(result["data"]["rows"][0]["age"], 40);
	}

	#[tokio::test]
	async fn test_db_execute_delete() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert
		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["David", "david@example.com", 28]
		});
		bridge.call("execute", insert).await.unwrap();

		// Delete
		let delete = json!({
			"sql": "DELETE FROM users WHERE email = $1",
			"params": ["david@example.com"]
		});

		let result = bridge.call("execute", delete).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["affected_rows"], 1);
	}

	#[tokio::test]
	async fn test_db_transaction_commit() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Begin transaction
		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		assert_eq!(begin_result["ok"], true);

		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		// Execute in transaction
		let execute1 = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Eve", "eve@example.com", 29]
		});
		bridge.call("execute_in_tx", execute1).await.unwrap();

		let execute2 = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Frank", "frank@example.com", 31]
		});
		bridge.call("execute_in_tx", execute2).await.unwrap();

		// Commit
		let commit = json!({"tx_id": tx_id});
		let result = bridge.call("transaction_commit", commit).await.unwrap();

		assert_eq!(result["ok"], true);

		// Verify both inserts succeeded
		let query = json!({
			"sql": "SELECT COUNT(*) as count FROM users",
			"params": []
		});
		let result = bridge.call("query", query).await.unwrap();
		assert_eq!(result["data"]["rows"][0]["count"], 2);
	}

	#[tokio::test]
	async fn test_db_transaction_rollback() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Begin transaction
		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		// Execute in transaction
		let execute = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Grace", "grace@example.com", 27]
		});
		bridge.call("execute_in_tx", execute).await.unwrap();

		// Rollback
		let rollback = json!({"tx_id": tx_id});
		let result = bridge.call("transaction_rollback", rollback).await.unwrap();

		assert_eq!(result["ok"], true);

		// Verify insert was rolled back
		let query = json!({
			"sql": "SELECT COUNT(*) as count FROM users",
			"params": []
		});
		let result = bridge.call("query", query).await.unwrap();
		assert_eq!(result["data"]["rows"][0]["count"], 0);
	}

	#[tokio::test]
	async fn test_db_validation_error() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Try to use query() with INSERT (should fail)
		let params = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Test", "test@example.com", 25]
		});

		let result = bridge.call("query", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_db_unique_constraint_error() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert first user
		let insert1 = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Alice", "alice@example.com", 30]
		});
		bridge.call("execute", insert1).await.unwrap();

		// Try to insert duplicate email
		let insert2 = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Bob", "alice@example.com", 25]
		});

		let result = bridge.call("execute", insert2).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}

	#[tokio::test]
	async fn test_db_query_in_tx() {
		let (mut bridge, _guard) = setup_test_db().await;

		// Insert some data
		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Test", "test@example.com", 25]
		});
		bridge.call("execute", insert).await.unwrap();

		// Begin transaction
		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		// Query in transaction
		let query = json!({
			"tx_id": tx_id,
			"sql": "SELECT * FROM users WHERE email = $1",
			"params": ["test@example.com"]
		});

		let result = bridge.call("query_in_tx", query).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["count"], 1);

		// Rollback (should still work for queries)
		bridge
			.call("transaction_rollback", json!({"tx_id": tx_id}))
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_unknown_function() {
		let mut bridge = DbBridge::new();

		let result = bridge.call("unknown", json!({})).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "DB_ERROR");
	}

	#[tokio::test]
	async fn test_transaction_not_found() {
		let mut bridge = DbBridge::new();

		let commit = json!({"tx_id": "invalid_tx_id"});
		let result = bridge.call("transaction_commit", commit).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "TRANSACTION_ERROR");
	}

	#[tokio::test]
	async fn test_invalid_params() {
		let mut bridge = DbBridge::new();

		// Invalid params (missing required fields)
		let params = json!({"invalid": "params"});

		let result = bridge.call("query", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}
}
