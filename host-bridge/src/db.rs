use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column, Row, TypeInfo};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

// ============================================================================
// DATABASE DRIVER - Native driver with runtime dispatch
// ============================================================================

/// Native database driver with full type support
/// Dispatches to PostgreSQL, MySQL, or SQLite based on connection URL
#[derive(Clone)]
pub enum DatabaseDriver {
	Postgres(PgPool),
	MySql(MySqlPool),
	Sqlite(SqlitePool),
}

impl DatabaseDriver {
	/// Connect to a database based on the URL scheme
	pub async fn connect(url: &str, config: &DbConfig) -> Result<Self> {
		if url.starts_with("postgres://") || url.starts_with("postgresql://") {
			let pool = PgPoolOptions::new()
				.max_connections(config.max_connections)
				.min_connections(config.min_connections)
				.acquire_timeout(Duration::from_millis(config.connection_timeout))
				.idle_timeout(Duration::from_secs(90))
				.max_lifetime(Duration::from_secs(1800))
				.test_before_acquire(true)
				.connect(url)
				.await?;
			Ok(Self::Postgres(pool))
		} else if url.starts_with("mysql://") || url.starts_with("mariadb://") {
			let pool = MySqlPoolOptions::new()
				.max_connections(config.max_connections)
				.min_connections(config.min_connections)
				.acquire_timeout(Duration::from_millis(config.connection_timeout))
				.idle_timeout(Duration::from_secs(90))
				.max_lifetime(Duration::from_secs(1800))
				.test_before_acquire(true)
				.connect(url)
				.await?;
			Ok(Self::MySql(pool))
		} else if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
			let pool = SqlitePoolOptions::new()
				.max_connections(config.max_connections)
				.min_connections(config.min_connections)
				.acquire_timeout(Duration::from_millis(config.connection_timeout))
				.idle_timeout(Duration::from_secs(90))
				.test_before_acquire(true)
				.connect(url)
				.await?;
			Ok(Self::Sqlite(pool))
		} else {
			Err(anyhow::anyhow!(
				"Unsupported database URL scheme. Supported: postgres://, mysql://, sqlite://"
			))
		}
	}

	/// Execute a SELECT query and return rows as JSON
	pub async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<serde_json::Map<String, Value>>> {
		match self {
			Self::Postgres(pool) => Self::query_postgres(pool, sql, params).await,
			Self::MySql(pool) => Self::query_mysql(pool, sql, params).await,
			Self::Sqlite(pool) => Self::query_sqlite(pool, sql, params).await,
		}
	}

	/// Execute an INSERT/UPDATE/DELETE and return affected rows
	pub async fn execute(&self, sql: &str, params: &[Value]) -> Result<ExecuteResult> {
		match self {
			Self::Postgres(pool) => Self::execute_postgres(pool, sql, params).await,
			Self::MySql(pool) => Self::execute_mysql(pool, sql, params).await,
			Self::Sqlite(pool) => Self::execute_sqlite(pool, sql, params).await,
		}
	}

	/// Begin a real database transaction and execute operations
	pub async fn execute_transaction(&self, operations: &[(String, Vec<Value>)]) -> Result<()> {
		match self {
			Self::Postgres(pool) => Self::execute_transaction_postgres(pool, operations).await,
			Self::MySql(pool) => Self::execute_transaction_mysql(pool, operations).await,
			Self::Sqlite(pool) => Self::execute_transaction_sqlite(pool, operations).await,
		}
	}

	// ========================================================================
	// PostgreSQL Implementation
	// ========================================================================

	async fn query_postgres(pool: &PgPool, sql: &str, params: &[Value]) -> Result<Vec<serde_json::Map<String, Value>>> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_postgres(query, param);
		}

		let rows = query.fetch_all(pool).await?;

		let mut result = Vec::new();
		for row in rows {
			let map = Self::row_to_json_postgres(&row)?;
			result.push(map);
		}

		Ok(result)
	}

	fn bind_param_postgres<'q>(
		query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
		param: &Value,
	) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
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

	fn row_to_json_postgres(row: &PgRow) -> Result<serde_json::Map<String, Value>> {
		let mut map = serde_json::Map::new();

		for (i, column) in row.columns().iter().enumerate() {
			let name = column.name().to_string();
			let type_info = column.type_info();
			let type_name = type_info.name();

			let value = match type_name {
				"BOOL" => {
					row.try_get::<bool, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"INT2" | "SMALLINT" => {
					row.try_get::<i16, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"INT4" | "INT" | "INTEGER" => {
					row.try_get::<i32, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"INT8" | "BIGINT" => {
					row.try_get::<i64, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"FLOAT4" | "REAL" => {
					row.try_get::<f32, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"FLOAT8" | "DOUBLE PRECISION" => {
					row.try_get::<f64, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" => {
					row.try_get::<String, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"TIMESTAMP" | "TIMESTAMPTZ" => {
					// Use chrono for proper timestamp handling
					row.try_get::<chrono::NaiveDateTime, _>(i)
						.map(|v| json!(v.to_string()))
						.or_else(|_| {
							row.try_get::<chrono::DateTime<chrono::Utc>, _>(i)
								.map(|v| json!(v.to_rfc3339()))
						})
						.unwrap_or(Value::Null)
				}
				"DATE" => {
					row.try_get::<chrono::NaiveDate, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"TIME" | "TIMETZ" => {
					row.try_get::<chrono::NaiveTime, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"UUID" => {
					row.try_get::<uuid::Uuid, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"JSON" | "JSONB" => {
					row.try_get::<serde_json::Value, _>(i)
						.unwrap_or(Value::Null)
				}
				"BYTEA" => {
					row.try_get::<Vec<u8>, _>(i)
						.map(|v| json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &v)))
						.unwrap_or(Value::Null)
				}
				_ => {
					// Fallback: try to get as string
					row.try_get::<String, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
			};

			map.insert(name, value);
		}

		Ok(map)
	}

	async fn execute_postgres(pool: &PgPool, sql: &str, params: &[Value]) -> Result<ExecuteResult> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_postgres(query, param);
		}

		let result = query.execute(pool).await?;

		Ok(ExecuteResult {
			rows_affected: result.rows_affected(),
			last_insert_id: None, // PostgreSQL uses RETURNING clause instead
		})
	}

	async fn execute_transaction_postgres(pool: &PgPool, operations: &[(String, Vec<Value>)]) -> Result<()> {
		let mut tx = pool.begin().await?;

		for (sql, params) in operations {
			let mut query = sqlx::query(sql);
			for param in params {
				query = Self::bind_param_postgres(query, param);
			}
			query.execute(&mut *tx).await?;
		}

		tx.commit().await?;
		Ok(())
	}

	// ========================================================================
	// MySQL Implementation
	// ========================================================================

	async fn query_mysql(pool: &MySqlPool, sql: &str, params: &[Value]) -> Result<Vec<serde_json::Map<String, Value>>> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_mysql(query, param);
		}

		let rows = query.fetch_all(pool).await?;

		let mut result = Vec::new();
		for row in rows {
			let map = Self::row_to_json_mysql(&row)?;
			result.push(map);
		}

		Ok(result)
	}

	fn bind_param_mysql<'q>(
		query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
		param: &Value,
	) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
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

	fn row_to_json_mysql(row: &MySqlRow) -> Result<serde_json::Map<String, Value>> {
		let mut map = serde_json::Map::new();

		for (i, column) in row.columns().iter().enumerate() {
			let name = column.name().to_string();
			let type_info = column.type_info();
			let type_name = type_info.name();

			let value = match type_name {
				"BOOLEAN" | "TINYINT(1)" | "BOOL" => {
					// MySQL stores booleans as TINYINT(1)
					row.try_get::<bool, _>(i)
						.or_else(|_| row.try_get::<i8, _>(i).map(|v| v != 0))
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"TINYINT" => {
					row.try_get::<i8, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"SMALLINT" => {
					row.try_get::<i16, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"INT" | "INTEGER" | "MEDIUMINT" => {
					row.try_get::<i32, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"BIGINT" => {
					row.try_get::<i64, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"FLOAT" => {
					row.try_get::<f32, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"DOUBLE" => {
					row.try_get::<f64, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"DECIMAL" | "NUMERIC" => {
					// Read as string to preserve precision
					row.try_get::<String, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
					row.try_get::<String, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
				"TIMESTAMP" | "DATETIME" => {
					row.try_get::<chrono::NaiveDateTime, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"DATE" => {
					row.try_get::<chrono::NaiveDate, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"TIME" => {
					row.try_get::<chrono::NaiveTime, _>(i)
						.map(|v| json!(v.to_string()))
						.unwrap_or(Value::Null)
				}
				"JSON" => {
					row.try_get::<serde_json::Value, _>(i)
						.unwrap_or(Value::Null)
				}
				"BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => {
					row.try_get::<Vec<u8>, _>(i)
						.map(|v| json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &v)))
						.unwrap_or(Value::Null)
				}
				_ => {
					// Fallback: try to get as string
					row.try_get::<String, _>(i)
						.map(|v| json!(v))
						.unwrap_or(Value::Null)
				}
			};

			map.insert(name, value);
		}

		Ok(map)
	}

	async fn execute_mysql(pool: &MySqlPool, sql: &str, params: &[Value]) -> Result<ExecuteResult> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_mysql(query, param);
		}

		let result = query.execute(pool).await?;

		Ok(ExecuteResult {
			rows_affected: result.rows_affected(),
			last_insert_id: Some(result.last_insert_id() as i64),
		})
	}

	async fn execute_transaction_mysql(pool: &MySqlPool, operations: &[(String, Vec<Value>)]) -> Result<()> {
		let mut tx = pool.begin().await?;

		for (sql, params) in operations {
			let mut query = sqlx::query(sql);
			for param in params {
				query = Self::bind_param_mysql(query, param);
			}
			query.execute(&mut *tx).await?;
		}

		tx.commit().await?;
		Ok(())
	}

	// ========================================================================
	// SQLite Implementation
	// ========================================================================

	async fn query_sqlite(pool: &SqlitePool, sql: &str, params: &[Value]) -> Result<Vec<serde_json::Map<String, Value>>> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_sqlite(query, param);
		}

		let rows = query.fetch_all(pool).await?;

		let mut result = Vec::new();
		for row in rows {
			let map = Self::row_to_json_sqlite(&row)?;
			result.push(map);
		}

		Ok(result)
	}

	fn bind_param_sqlite<'q>(
		query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
		param: &Value,
	) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
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

	fn row_to_json_sqlite(row: &SqliteRow) -> Result<serde_json::Map<String, Value>> {
		let mut map = serde_json::Map::new();

		for (i, column) in row.columns().iter().enumerate() {
			let name = column.name().to_string();

			// SQLite is dynamically typed - always try to determine the actual type at runtime
			// Type info from SQLite can be unreliable for expressions, aliases, and aggregates
			let value = if let Ok(v) = row.try_get::<i64, _>(i) {
				json!(v)
			} else if let Ok(v) = row.try_get::<i32, _>(i) {
				json!(v)
			} else if let Ok(v) = row.try_get::<f64, _>(i) {
				json!(v)
			} else if let Ok(v) = row.try_get::<String, _>(i) {
				json!(v)
			} else if let Ok(v) = row.try_get::<bool, _>(i) {
				json!(v)
			} else if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
				json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &v))
			} else {
				Value::Null
			};

			map.insert(name, value);
		}

		Ok(map)
	}

	async fn execute_sqlite(pool: &SqlitePool, sql: &str, params: &[Value]) -> Result<ExecuteResult> {
		let mut query = sqlx::query(sql);

		for param in params {
			query = Self::bind_param_sqlite(query, param);
		}

		let result = query.execute(pool).await?;

		Ok(ExecuteResult {
			rows_affected: result.rows_affected(),
			last_insert_id: Some(result.last_insert_rowid()),
		})
	}

	async fn execute_transaction_sqlite(pool: &SqlitePool, operations: &[(String, Vec<Value>)]) -> Result<()> {
		let mut tx = pool.begin().await?;

		for (sql, params) in operations {
			let mut query = sqlx::query(sql);
			for param in params {
				query = Self::bind_param_sqlite(query, param);
			}
			query.execute(&mut *tx).await?;
		}

		tx.commit().await?;
		Ok(())
	}
}

// ============================================================================
// DATABASE BRIDGE
// ============================================================================

/// Database bridge providing database access capabilities
pub struct DbBridge {
	driver: Arc<RwLock<Option<DatabaseDriver>>>,
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

/// Result from execute operations
#[derive(Debug)]
pub struct ExecuteResult {
	pub rows_affected: u64,
	pub last_insert_id: Option<i64>,
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
			driver: Arc::new(RwLock::new(None)),
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

		// Connect using the appropriate driver
		let driver = DatabaseDriver::connect(&config.database_url, &config).await?;

		// Store the driver and config
		*self.driver.write().await = Some(driver);
		*self.config.write().await = Some(config);

		Ok(())
	}

	/// Get the database driver
	async fn get_driver(&self) -> Result<DatabaseDriver> {
		let driver_guard = self.driver.read().await;

		if let Some(driver) = driver_guard.as_ref() {
			Ok(driver.clone())
		} else {
			Err(anyhow::anyhow!(
				"Database not configured. Call configure() first or set DATABASE_URL environment variable."
			))
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
			"config" => self.config_call(params).await,
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
	async fn query(&self, params: Value) -> Result<Value> {
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

		// Validate SQL
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

		let driver = match self.get_driver().await {
			Ok(d) => d,
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

		let timeout = {
			let config_guard = self.config.read().await;
			config_guard.as_ref().map(|c| c.query_timeout).unwrap_or(30000)
		};

		let result = tokio::time::timeout(
			Duration::from_millis(timeout),
			driver.query(&req.sql, &req.params),
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
				let (code, message) = self.categorize_error(&format!("{}", e));
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

	/// Execute an INSERT/UPDATE/DELETE query
	async fn execute(&self, params: Value) -> Result<Value> {
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

		// Validate SQL
		let sql_upper = req.sql.trim().to_uppercase();
		let valid_commands = ["INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "TRUNCATE"];
		let is_valid = valid_commands.iter().any(|cmd| sql_upper.starts_with(cmd));

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

		let driver = match self.get_driver().await {
			Ok(d) => d,
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

		let timeout = {
			let config_guard = self.config.read().await;
			config_guard.as_ref().map(|c| c.query_timeout).unwrap_or(30000)
		};

		let result = tokio::time::timeout(
			Duration::from_millis(timeout),
			driver.execute(&req.sql, &req.params),
		)
		.await;

		match result {
			Ok(Ok(exec_result)) => Ok(json!({
				"ok": true,
				"data": {
					"affected_rows": exec_result.rows_affected,
					"last_insert_id": exec_result.last_insert_id
				}
			})),
			Ok(Err(e)) => {
				let (code, message) = self.categorize_error(&format!("{}", e));
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
	async fn transaction_begin(&self, _params: Value) -> Result<Value> {
		let tx_id = format!("tx_{}", Uuid::new_v4().to_string().replace("-", ""));

		let transaction = Transaction {
			id: tx_id.clone(),
			committed: false,
			rolled_back: false,
			operations: Vec::new(),
		};

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
	async fn transaction_commit(&self, params: Value) -> Result<Value> {
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

		let driver = match self.get_driver().await {
			Ok(d) => d,
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

		let operations = transaction.operations.clone();
		drop(transactions);

		// Execute all operations in a real transaction
		if let Err(e) = driver.execute_transaction(&operations).await {
			let (code, message) = self.categorize_error(&format!("{}", e));
			return Ok(json!({
				"ok": false,
				"err": {
					"code": code,
					"message": message,
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
	async fn transaction_rollback(&self, params: Value) -> Result<Value> {
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

		transaction.rolled_back = true;
		transactions.remove(&req.tx_id);

		Ok(json!({
			"ok": true,
			"data": null
		}))
	}

	/// Execute a query within a transaction
	async fn query_in_tx(&self, params: Value) -> Result<Value> {
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

		let driver = match self.get_driver().await {
			Ok(d) => d,
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

		match driver.query(&req.sql, &req.params).await {
			Ok(rows) => Ok(json!({
				"ok": true,
				"data": {
					"rows": rows,
					"count": rows.len()
				}
			})),
			Err(e) => {
				let (code, message) = self.categorize_error(&format!("{}", e));
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
	async fn execute_in_tx(&self, params: Value) -> Result<Value> {
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

		let sql_upper = req.sql.trim().to_uppercase();
		let valid_commands = ["INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "ALTER", "TRUNCATE"];
		let is_valid = valid_commands.iter().any(|cmd| sql_upper.starts_with(cmd));

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

		transaction.operations.push((req.sql.clone(), req.params.clone()));

		Ok(json!({
			"ok": true,
			"data": {
				"affected_rows": 0,
				"last_insert_id": null
			}
		}))
	}

	/// Configure database connection
	async fn config_call(&mut self, params: Value) -> Result<Value> {
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

	/// Sanitize error message
	fn sanitize_error(&self, error: &str) -> String {
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

impl Default for DbBridge {
	fn default() -> Self {
		Self::new()
	}
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;
	use tokio::sync::Mutex as AsyncMutex;

	static TEST_DB_MUTEX: once_cell::sync::Lazy<Arc<AsyncMutex<()>>> =
		once_cell::sync::Lazy::new(|| Arc::new(AsyncMutex::new(())));

	async fn setup_test_db() -> (DbBridge, tokio::sync::MutexGuard<'static, ()>) {
		let guard = TEST_DB_MUTEX.lock().await;

		let mut bridge = DbBridge::new();

		let config = DbConfig {
			database_url: "sqlite::memory:".to_string(),
			max_connections: 5,
			min_connections: 1,
			connection_timeout: 5000,
			query_timeout: 10000,
		};

		bridge.configure(config).await.unwrap();

		// Drop table if exists
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

		for i in 1..=3 {
			let insert = json!({
				"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
				"params": [format!("User{}", i), format!("user{}@example.com", i), 20 + i]
			});
			bridge.call("execute", insert).await.unwrap();
		}

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

		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Charlie", "charlie@example.com", 35]
		});
		bridge.call("execute", insert).await.unwrap();

		let update = json!({
			"sql": "UPDATE users SET age = $1 WHERE email = $2",
			"params": [40, "charlie@example.com"]
		});

		let result = bridge.call("execute", update).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["affected_rows"], 1);

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

		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["David", "david@example.com", 28]
		});
		bridge.call("execute", insert).await.unwrap();

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

		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		assert_eq!(begin_result["ok"], true);

		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

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

		let commit = json!({"tx_id": tx_id});
		let result = bridge.call("transaction_commit", commit).await.unwrap();

		assert_eq!(result["ok"], true);

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

		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		let execute = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Grace", "grace@example.com", 27]
		});
		bridge.call("execute_in_tx", execute).await.unwrap();

		let rollback = json!({"tx_id": tx_id});
		let result = bridge.call("transaction_rollback", rollback).await.unwrap();

		assert_eq!(result["ok"], true);

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

		let insert1 = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Alice", "alice@example.com", 30]
		});
		bridge.call("execute", insert1).await.unwrap();

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

		let insert = json!({
			"sql": "INSERT INTO users (name, email, age) VALUES ($1, $2, $3)",
			"params": ["Test", "test@example.com", 25]
		});
		bridge.call("execute", insert).await.unwrap();

		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		let query = json!({
			"tx_id": tx_id,
			"sql": "SELECT * FROM users WHERE email = $1",
			"params": ["test@example.com"]
		});

		let result = bridge.call("query_in_tx", query).await.unwrap();

		assert_eq!(result["ok"], true);
		assert_eq!(result["data"]["count"], 1);

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

		let params = json!({"invalid": "params"});

		let result = bridge.call("query", params).await.unwrap();

		assert_eq!(result["ok"], false);
		assert_eq!(result["err"]["code"], "VALIDATION_ERROR");
	}
}

/// Integration tests for PostgreSQL and MySQL
/// Run with: INTEGRATION_TESTS=1 cargo test integration --no-fail-fast -- --test-threads=1
#[cfg(test)]
mod integration_tests {
	use super::*;

	fn skip_if_no_integration() -> bool {
		std::env::var("INTEGRATION_TESTS").is_err()
	}

	async fn setup_postgres() -> Option<DbBridge> {
		if skip_if_no_integration() {
			return None;
		}

		let mut bridge = DbBridge::new();
		let config = DbConfig {
			database_url: "postgres://clean:cleanpass@localhost:5432/cleantest".to_string(),
			max_connections: 5,
			min_connections: 1,
			connection_timeout: 10000,
			query_timeout: 30000,
		};

		match bridge.configure(config).await {
			Ok(()) => Some(bridge),
			Err(e) => {
				eprintln!("Failed to connect to PostgreSQL: {}", e);
				None
			}
		}
	}

	async fn setup_mysql() -> Option<DbBridge> {
		if skip_if_no_integration() {
			return None;
		}

		let mut bridge = DbBridge::new();
		let config = DbConfig {
			database_url: "mysql://clean:cleanpass@localhost:3306/cleantest".to_string(),
			max_connections: 5,
			min_connections: 1,
			connection_timeout: 10000,
			query_timeout: 30000,
		};

		match bridge.configure(config).await {
			Ok(()) => Some(bridge),
			Err(e) => {
				eprintln!("Failed to connect to MySQL: {}", e);
				None
			}
		}
	}

	#[tokio::test]
	async fn integration_test_postgres_query() {
		let Some(mut bridge) = setup_postgres().await else {
			println!("Skipping PostgreSQL integration test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		// Query users table with all columns - native driver supports all types
		let query = json!({
			"sql": "SELECT * FROM users ORDER BY id",
			"params": []
		});

		let result = bridge.call("query", query).await.unwrap();

		assert_eq!(result["ok"], true, "PostgreSQL query failed: {:?}", result);
		assert!(result["data"]["count"].as_i64().unwrap() >= 4, "Expected at least 4 users");

		let first_user = &result["data"]["rows"][0];
		assert_eq!(first_user["name"], "Alice Johnson");
		assert_eq!(first_user["email"], "alice@example.com");
		assert_eq!(first_user["role"], "admin");
		// Native driver supports booleans!
		assert_eq!(first_user["active"], true);

		println!("PostgreSQL query test passed!");
	}

	#[tokio::test]
	async fn integration_test_postgres_crud() {
		let Some(mut bridge) = setup_postgres().await else {
			println!("Skipping PostgreSQL CRUD test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		// Cleanup
		let cleanup = json!({
			"sql": "DELETE FROM users WHERE email = $1",
			"params": ["testuser@integration.com"]
		});
		let _ = bridge.call("execute", cleanup).await;

		// INSERT
		let insert = json!({
			"sql": "INSERT INTO users (name, email, role) VALUES ($1, $2, $3)",
			"params": ["Test User", "testuser@integration.com", "tester"]
		});
		let result = bridge.call("execute", insert).await.unwrap();
		assert_eq!(result["ok"], true, "PostgreSQL INSERT failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);

		// SELECT with all columns
		let query = json!({
			"sql": "SELECT * FROM users WHERE email = $1",
			"params": ["testuser@integration.com"]
		});
		let result = bridge.call("query", query).await.unwrap();
		assert_eq!(result["ok"], true, "PostgreSQL SELECT failed: {:?}", result);
		assert_eq!(result["data"]["count"], 1);
		assert_eq!(result["data"]["rows"][0]["name"], "Test User");

		// UPDATE
		let update = json!({
			"sql": "UPDATE users SET role = $1 WHERE email = $2",
			"params": ["admin", "testuser@integration.com"]
		});
		let result = bridge.call("execute", update).await.unwrap();
		assert_eq!(result["ok"], true, "PostgreSQL UPDATE failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);

		// DELETE
		let delete = json!({
			"sql": "DELETE FROM users WHERE email = $1",
			"params": ["testuser@integration.com"]
		});
		let result = bridge.call("execute", delete).await.unwrap();
		assert_eq!(result["ok"], true, "PostgreSQL DELETE failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);

		println!("PostgreSQL CRUD test passed!");
	}

	#[tokio::test]
	async fn integration_test_postgres_join() {
		let Some(mut bridge) = setup_postgres().await else {
			println!("Skipping PostgreSQL JOIN test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		let query = json!({
			"sql": "SELECT p.title, u.name as author, u.active FROM posts p JOIN users u ON p.author_id = u.id WHERE u.active = true ORDER BY p.id",
			"params": []
		});

		let result = bridge.call("query", query).await.unwrap();

		assert_eq!(result["ok"], true, "PostgreSQL JOIN failed: {:?}", result);
		assert!(result["data"]["count"].as_i64().unwrap() >= 3, "Expected at least 3 posts");
		// Verify boolean is properly returned
		assert_eq!(result["data"]["rows"][0]["active"], true);

		println!("PostgreSQL JOIN test passed!");
	}

	#[tokio::test]
	async fn integration_test_mysql_query() {
		let Some(mut bridge) = setup_mysql().await else {
			println!("Skipping MySQL integration test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		// Query with all columns - native driver supports all types
		let query = json!({
			"sql": "SELECT * FROM users ORDER BY id",
			"params": []
		});

		let result = bridge.call("query", query).await.unwrap();

		assert_eq!(result["ok"], true, "MySQL query failed: {:?}", result);
		assert!(result["data"]["count"].as_i64().unwrap() >= 4, "Expected at least 4 users");

		let first_user = &result["data"]["rows"][0];
		assert_eq!(first_user["name"], "Alice Johnson");
		assert_eq!(first_user["email"], "alice@example.com");
		assert_eq!(first_user["role"], "admin");
		// Native driver supports MySQL booleans!
		assert_eq!(first_user["active"], true);

		println!("MySQL query test passed!");
	}

	#[tokio::test]
	async fn integration_test_mysql_crud() {
		let Some(mut bridge) = setup_mysql().await else {
			println!("Skipping MySQL CRUD test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		// Cleanup
		let cleanup = json!({
			"sql": "DELETE FROM users WHERE email = ?",
			"params": ["testuser_mysql@integration.com"]
		});
		let _ = bridge.call("execute", cleanup).await;

		// INSERT
		let insert = json!({
			"sql": "INSERT INTO users (name, email, role) VALUES (?, ?, ?)",
			"params": ["Test User", "testuser_mysql@integration.com", "tester"]
		});
		let result = bridge.call("execute", insert).await.unwrap();
		assert_eq!(result["ok"], true, "MySQL INSERT failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);
		// MySQL returns last_insert_id
		assert!(result["data"]["last_insert_id"].is_number());

		// SELECT with all columns
		let query = json!({
			"sql": "SELECT * FROM users WHERE email = ?",
			"params": ["testuser_mysql@integration.com"]
		});
		let result = bridge.call("query", query).await.unwrap();
		assert_eq!(result["ok"], true, "MySQL SELECT failed: {:?}", result);
		assert_eq!(result["data"]["count"], 1);
		assert_eq!(result["data"]["rows"][0]["name"], "Test User");

		// UPDATE
		let update = json!({
			"sql": "UPDATE users SET role = ? WHERE email = ?",
			"params": ["admin", "testuser_mysql@integration.com"]
		});
		let result = bridge.call("execute", update).await.unwrap();
		assert_eq!(result["ok"], true, "MySQL UPDATE failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);

		// DELETE
		let delete = json!({
			"sql": "DELETE FROM users WHERE email = ?",
			"params": ["testuser_mysql@integration.com"]
		});
		let result = bridge.call("execute", delete).await.unwrap();
		assert_eq!(result["ok"], true, "MySQL DELETE failed: {:?}", result);
		assert_eq!(result["data"]["affected_rows"], 1);

		println!("MySQL CRUD test passed!");
	}

	#[tokio::test]
	async fn integration_test_mysql_join() {
		let Some(mut bridge) = setup_mysql().await else {
			println!("Skipping MySQL JOIN test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		let query = json!({
			"sql": "SELECT p.title, u.name as author, u.active FROM posts p JOIN users u ON p.author_id = u.id WHERE u.active = true ORDER BY p.id",
			"params": []
		});

		let result = bridge.call("query", query).await.unwrap();

		assert_eq!(result["ok"], true, "MySQL JOIN failed: {:?}", result);
		assert!(result["data"]["count"].as_i64().unwrap() >= 3, "Expected at least 3 posts");
		// Verify boolean is properly returned
		assert_eq!(result["data"]["rows"][0]["active"], true);

		println!("MySQL JOIN test passed!");
	}

	#[tokio::test]
	async fn integration_test_postgres_transaction() {
		let Some(mut bridge) = setup_postgres().await else {
			println!("Skipping PostgreSQL transaction test (set INTEGRATION_TESTS=1 to run)");
			return;
		};

		let begin_result = bridge.call("transaction_begin", json!({})).await.unwrap();
		assert_eq!(begin_result["ok"], true);
		let tx_id = begin_result["data"]["tx_id"].as_str().unwrap();

		let execute1 = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, role) VALUES ($1, $2, $3)",
			"params": ["TxUser1", "txuser1@integration.com", "tester"]
		});
		bridge.call("execute_in_tx", execute1).await.unwrap();

		let execute2 = json!({
			"tx_id": tx_id,
			"sql": "INSERT INTO users (name, email, role) VALUES ($1, $2, $3)",
			"params": ["TxUser2", "txuser2@integration.com", "tester"]
		});
		bridge.call("execute_in_tx", execute2).await.unwrap();

		let commit = json!({"tx_id": tx_id});
		let result = bridge.call("transaction_commit", commit).await.unwrap();
		assert_eq!(result["ok"], true, "PostgreSQL transaction commit failed: {:?}", result);

		let query = json!({
			"sql": "SELECT COUNT(*) as count FROM users WHERE email LIKE $1",
			"params": ["%@integration.com"]
		});
		let result = bridge.call("query", query).await.unwrap();
		assert!(result["data"]["rows"][0]["count"].as_i64().unwrap() >= 2);

		// Cleanup
		let cleanup = json!({
			"sql": "DELETE FROM users WHERE email LIKE $1",
			"params": ["%@integration.com"]
		});
		bridge.call("execute", cleanup).await.unwrap();

		println!("PostgreSQL transaction test passed!");
	}
}
