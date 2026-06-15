//! SQLite persistence tests for the background job queue runtime.
//!
//! Each test uses an in-memory SQLite database (`:memory:`) to exercise the
//! persistence layer in isolation without touching the filesystem.

use clean_server::jobs::{
    BackoffStrategy, JobStatus,
    create_shared_jobs_state, ensure_jobs_table, enqueue_job, init_persistence,
    job_result, job_status, now_ms, recover_from_disk_with_pool, register_job,
};
use sqlx::SqlitePool;

/// Build an in-memory SQLite pool for test isolation.
async fn make_test_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite should always connect");
    ensure_jobs_table(&pool)
        .await
        .expect("table creation should succeed on in-memory DB");
    pool
}

// ---------------------------------------------------------------------------
// Test 1: enqueue_persists_to_sqlite
// ---------------------------------------------------------------------------

/// Verify that enqueueing a job writes a row to `__clean_jobs` with
/// status = 'pending'.
#[tokio::test]
async fn enqueue_persists_to_sqlite() {
    let pool = make_test_pool().await;
    let state = create_shared_jobs_state();
    state.lock().await.set_sqlite_pool(pool.clone());

    register_job(
        &state,
        "sendEmail".to_string(),
        "sendEmail_handler".to_string(),
        3,
        BackoffStrategy::Exponential,
        1000,
        0,
        "default".to_string(),
    )
    .await;

    let job_id = enqueue_job(
        &state,
        "sendEmail".to_string(),
        r#"{"to":"user@example.com"}"#.to_string(),
    )
    .await;

    assert!(!job_id.is_empty(), "enqueue must return a non-empty job ID");

    // Query the DB directly to confirm the row exists with the expected values.
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, status, args_json FROM __clean_jobs WHERE id = ?",
    )
    .bind(&job_id)
    .fetch_optional(&pool)
    .await
    .expect("DB query should not fail");

    let (db_id, db_status, db_args) = row.expect("row must exist in __clean_jobs after enqueue");
    assert_eq!(db_id, job_id);
    assert_eq!(db_status, "pending", "newly enqueued job must have status=pending");
    assert!(
        db_args.contains("user@example.com"),
        "args_json must contain the enqueued payload"
    );
}

// ---------------------------------------------------------------------------
// Test 2: recover_resets_running_to_pending
// ---------------------------------------------------------------------------

/// Verify that a `running` row (simulating an orphaned job from a previous
/// hard-kill) is reset to `pending` in the in-memory cache and in the DB
/// after `recover_from_disk_with_pool` is called.
#[tokio::test]
async fn recover_resets_running_to_pending() {
    let pool = make_test_pool().await;

    let job_id = uuid::Uuid::new_v4().to_string();
    let ts = now_ms() as i64;

    // Insert a row in `running` state directly — as if a previous server
    // process was killed mid-execution.
    sqlx::query(
        "INSERT INTO __clean_jobs
            (id, name, args_json, status, attempt, scheduled_at_ms, created_at_ms, queue)
         VALUES (?, 'processPayment', '{}', 'running', 1, ?, ?, 'default')",
    )
    .bind(&job_id)
    .bind(ts)
    .bind(ts)
    .execute(&pool)
    .await
    .expect("INSERT should succeed");

    let state = create_shared_jobs_state();

    // Run recovery against the pool (without attaching the pool to the state first
    // — we're testing the standalone recovery function used during startup).
    recover_from_disk_with_pool(&state, &pool).await;

    // The record must be in memory with status = Pending.
    let store = state.lock().await;
    let record = store
        .records
        .get(&job_id)
        .expect("recovered record must be present in the in-memory cache");

    assert_eq!(
        record.status,
        JobStatus::Pending,
        "running rows must be reset to pending during recovery"
    );
    drop(store);

    // Confirm the DB row was also updated to `pending`.
    let (db_status,): (String,) =
        sqlx::query_as("SELECT status FROM __clean_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .expect("DB query should succeed");

    assert_eq!(db_status, "pending", "DB row must be updated to pending during recovery");
}

// ---------------------------------------------------------------------------
// Test 3: cleanup_deletes_old_finished_jobs
// ---------------------------------------------------------------------------

/// Verify that the startup cleanup sweep removes finished rows whose
/// `finished_at_ms` is older than the configured retention window.
#[tokio::test]
async fn cleanup_deletes_old_finished_jobs() {
    let pool = make_test_pool().await;

    let retention_days = 7u64;
    // Place the finished_at_ms 1 ms before the cutoff so the row is stale.
    let cutoff_ms = now_ms() - retention_days * 86_400_000;
    let old_finished_ms = (cutoff_ms - 1) as i64;
    let ts = old_finished_ms;

    let job_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO __clean_jobs
            (id, name, args_json, status, attempt, scheduled_at_ms, created_at_ms,
             finished_at_ms, queue)
         VALUES (?, 'cleanupJob', '{}', 'succeeded', 1, ?, ?, ?, 'default')",
    )
    .bind(&job_id)
    .bind(ts)
    .bind(ts)
    .bind(old_finished_ms)
    .execute(&pool)
    .await
    .expect("INSERT should succeed");

    // Wire the pool into the state and run the full startup sequence
    // (schema guard + cleanup + recovery).
    let state = create_shared_jobs_state();
    state.lock().await.set_sqlite_pool(pool.clone());

    init_persistence(&state, retention_days).await;

    // The stale row must have been deleted.
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM __clean_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_optional(&pool)
            .await
            .expect("DB query should succeed");

    assert!(
        row.is_none(),
        "stale finished job must be deleted by the cleanup sweep"
    );
}

// ---------------------------------------------------------------------------
// Test 4: status_query_falls_back_to_db
// ---------------------------------------------------------------------------

/// Verify that `job_status` and `job_result` return correct values for a
/// record that exists in the DB but has been evicted from the in-memory cache
/// (i.e. it is a finished job not loaded during recovery).
#[tokio::test]
async fn status_query_falls_back_to_db() {
    let pool = make_test_pool().await;

    let job_id = uuid::Uuid::new_v4().to_string();
    let ts = now_ms() as i64;

    // Insert a `succeeded` row directly — recovery only loads pending+running
    // rows, so this row intentionally stays out of the in-memory cache.
    sqlx::query(
        "INSERT INTO __clean_jobs
            (id, name, args_json, status, attempt, scheduled_at_ms, created_at_ms,
             finished_at_ms, result_json, queue)
         VALUES (?, 'analyticsJob', '{}', 'succeeded', 1, ?, ?, ?, '{\"ok\":true}', 'default')",
    )
    .bind(&job_id)
    .bind(ts)
    .bind(ts)
    .bind(ts)
    .execute(&pool)
    .await
    .expect("INSERT should succeed");

    // State has the pool but the record is NOT in the in-memory cache.
    let state = create_shared_jobs_state();
    state.lock().await.set_sqlite_pool(pool.clone());

    assert!(
        !state.lock().await.records.contains_key(&job_id),
        "record must not be in the in-memory cache before the DB fallback test"
    );

    // job_status falls back to the DB when the id is absent from memory.
    let status = job_status(&state, &job_id).await;
    assert_eq!(
        status, "succeeded",
        "job_status must return 'succeeded' via DB fallback"
    );

    // job_result also uses DB fallback for terminal records not in memory.
    let result = job_result(&state, &job_id).await;
    assert_eq!(
        result, r#"{"ok":true}"#,
        "job_result must return the result_json from the DB fallback"
    );
}
