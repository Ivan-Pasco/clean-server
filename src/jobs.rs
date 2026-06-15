//! Background job queue runtime for frame.jobs.
//!
//! Implements an in-memory job queue with:
//! - Configurable retry policies (fixed, exponential backoff)
//! - Per-job attempt limits
//! - Deferred scheduling via `run_at` Unix millisecond timestamps
//! - A cron scheduler with native minute-level parsing
//! - `tokio::task_local!` context so bridge functions can access the current
//!   job ID, args, and attempt number from inside a handler invocation
//!
//! # Architecture
//!
//! ```text
//! _job_register / _job_enqueue
//!        │
//!        ▼
//! ┌──────────────────────────┐
//! │  SharedJobsState         │  in-memory HashMap<id, JobRecord>
//! │  (Arc<Mutex<JobsStore>>) │  + HashMap<name, JobConfig>
//! └──────────┬───────────────┘
//!            │ polled every second
//!            ▼
//! ┌──────────────────────────┐
//! │  worker_loop             │  tokio task
//! │  - pick pending+due jobs │
//! │  - set task-locals       │
//! │  - call WASM handler     │
//! │  - apply retry policy    │
//! └──────────────────────────┘
//!
//! _schedule_cron
//!        │
//!        ▼
//! ┌──────────────────────────┐
//! │  cron_scheduler_loop     │  per-schedule tokio task
//! │  - compute next tick     │
//! │  - sleep until next tick │
//! │  - call WASM handler     │
//! └──────────────────────────┘
//! ```

use crate::wasm::{RequestContext, WasmInstance};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Task-local job execution context
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// The job ID of the job currently being executed. Empty string outside handler.
    pub static JOB_CURRENT_ID: String;

    /// The args JSON of the job currently being executed. Empty string outside handler.
    pub static JOB_CURRENT_ARGS: String;

    /// The attempt number (1-based) of the current job execution. 0 outside handler.
    pub static JOB_CURRENT_ATTEMPT: i32;

    /// When set by `_job_retry_after`, override the computed backoff delay (milliseconds).
    /// -1 means not set (use default backoff formula).
    pub static JOB_RETRY_OVERRIDE_MS: std::cell::Cell<i64>;

    /// Reason string when `_job_fail` was called; None if not called.
    pub static JOB_EXPLICIT_FAIL: std::cell::RefCell<Option<String>>;

    /// Result JSON when `_job_succeed` was called; None if not called.
    pub static JOB_EXPLICIT_SUCCEED: std::cell::RefCell<Option<String>>;
}

/// Retrieve the current job ID from task-local storage.
/// Returns an empty string when called outside a job handler.
pub fn current_job_id() -> String {
    JOB_CURRENT_ID.try_with(|s| s.clone()).unwrap_or_default()
}

/// Retrieve the current job args from task-local storage.
/// Returns an empty string when called outside a job handler.
pub fn current_job_args() -> String {
    JOB_CURRENT_ARGS.try_with(|s| s.clone()).unwrap_or_default()
}

/// Retrieve the current attempt number from task-local storage.
/// Returns 0 when called outside a job handler.
pub fn current_job_attempt() -> i32 {
    JOB_CURRENT_ATTEMPT.try_with(|n| *n).unwrap_or(0)
}

/// Request a retry after the specified delay in milliseconds (called from inside a handler).
/// Overrides the computed backoff for this attempt.
pub fn request_retry_after_ms(delay_ms: i64) {
    JOB_RETRY_OVERRIDE_MS.try_with(|cell| cell.set(delay_ms)).ok();
}

/// Mark the current job as explicitly failed (called from inside a handler via `_job_fail`).
pub fn mark_explicit_fail(reason: String) {
    JOB_EXPLICIT_FAIL
        .try_with(|cell| {
            *cell.borrow_mut() = Some(reason);
        })
        .ok();
}

/// Mark the current job as explicitly succeeded with a result JSON (called via `_job_succeed`).
pub fn mark_explicit_succeed(result: String) {
    JOB_EXPLICIT_SUCCEED
        .try_with(|cell| {
            *cell.borrow_mut() = Some(result);
        })
        .ok();
}

// ---------------------------------------------------------------------------
// Job configuration (registered at startup via _job_register)
// ---------------------------------------------------------------------------

/// Retry backoff strategy for a registered job type.
#[derive(Debug, Clone, PartialEq)]
pub enum BackoffStrategy {
    /// Always use the same fixed delay.
    Fixed,
    /// Delay doubles with each attempt: `delay * 2^(attempt-1)`.
    Exponential,
}

impl BackoffStrategy {
    /// Parse from the string representation used in `_job_register`.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "exponential" => BackoffStrategy::Exponential,
            _ => BackoffStrategy::Fixed,
        }
    }

    /// Compute the next delay in milliseconds given the base delay and attempt number.
    ///
    /// `attempt` is 1-based (first execution = 1, first retry = 2, …).
    pub fn compute_delay(&self, base_ms: u64, attempt: u32) -> u64 {
        match self {
            BackoffStrategy::Fixed => base_ms,
            BackoffStrategy::Exponential => {
                // delay * 2^(attempt-1), capped at 24 hours to prevent overflow
                let exponent = (attempt.saturating_sub(1)).min(30);
                base_ms.saturating_mul(1u64 << exponent).min(86_400_000)
            }
        }
    }
}

/// Configuration for a registered job handler, stored at startup.
#[derive(Debug, Clone)]
pub struct JobConfig {
    /// Job name used in `_job_enqueue` calls.
    pub name: String,
    /// The WASM export name to call when the job runs.
    pub handler: String,
    /// Maximum number of attempts before the job is marked failed.
    pub max_attempts: u32,
    /// Backoff strategy for retries.
    pub backoff: BackoffStrategy,
    /// Base retry delay in milliseconds.
    pub delay_ms: u64,
    /// Maximum handler run time in milliseconds (0 = unlimited).
    pub timeout_ms: u64,
    /// Named queue for prioritised processing.
    pub queue: String,
}

// ---------------------------------------------------------------------------
// Job record (live state stored in memory during server run)
// ---------------------------------------------------------------------------

/// Current lifecycle status of a job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending   => "pending",
            JobStatus::Running   => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed    => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

/// A single job instance tracked in the in-memory store.
#[derive(Debug, Clone)]
pub struct JobRecord {
    /// Unique job identifier (UUID v4 string).
    pub id: String,
    /// Name of the registered job type.
    pub name: String,
    /// Serialised argument payload (JSON string).
    pub args: String,
    /// Current lifecycle status.
    pub status: JobStatus,
    /// Number of attempts that have run so far.
    pub attempt: u32,
    /// Maximum attempts allowed (copied from JobConfig at enqueue time).
    pub max_attempts: u32,
    /// Backoff strategy (copied from JobConfig at enqueue time).
    pub backoff: BackoffStrategy,
    /// Base retry delay in ms (copied from JobConfig at enqueue time).
    pub delay_ms: u64,
    /// Timeout in ms (0 = unlimited).
    pub timeout_ms: u64,
    /// Queue name.
    pub queue: String,
    /// The WASM handler export name.
    pub handler: String,
    /// When the job should next run (Unix ms).
    pub scheduled_at: u64,
    /// When this record was created (Unix ms).
    pub created_at: u64,
    /// When this record was last updated (Unix ms).
    pub updated_at: u64,
    /// Result payload set by `_job_succeed`, None until then.
    pub result: Option<String>,
    /// Error message from the last failed attempt, if any.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Cron schedule registration
// ---------------------------------------------------------------------------

/// A registered cron schedule entry.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    /// User-visible name (key in `_schedule_cancel`).
    pub name: String,
    /// Cron expression string (e.g. `"*/1 * * * *"`).
    pub cron_expr: String,
    /// WASM export name to call on each tick.
    pub handler: String,
    /// Whether this schedule is still active.
    pub active: bool,
}

// ---------------------------------------------------------------------------
// The shared jobs store
// ---------------------------------------------------------------------------

/// All in-memory job state — one instance per server process.
pub struct JobsStore {
    /// Registered job configurations keyed by job name.
    pub configs: HashMap<String, JobConfig>,
    /// All job records keyed by job ID.
    pub records: HashMap<String, JobRecord>,
    /// Registered cron schedules keyed by schedule name.
    pub schedules: HashMap<String, CronSchedule>,
}

impl JobsStore {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            records: HashMap::new(),
            schedules: HashMap::new(),
        }
    }
}

impl Default for JobsStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe handle to the shared jobs state.
pub type SharedJobsState = Arc<Mutex<JobsStore>>;

/// Create a new empty shared jobs state.
pub fn create_shared_jobs_state() -> SharedJobsState {
    Arc::new(Mutex::new(JobsStore::new()))
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Current Unix timestamp in milliseconds.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Public API used by bridge functions
// ---------------------------------------------------------------------------

/// Register a job handler with its retry configuration.
///
/// Silently replaces any existing config for the same name.
#[allow(clippy::too_many_arguments)]
pub async fn register_job(
    state: &SharedJobsState,
    name: String,
    handler: String,
    max_attempts: u32,
    backoff: BackoffStrategy,
    delay_ms: u64,
    timeout_ms: u64,
    queue: String,
) {
    let config = JobConfig {
        name: name.clone(),
        handler,
        max_attempts: max_attempts.max(1),
        backoff,
        delay_ms,
        timeout_ms,
        queue,
    };
    state.lock().await.configs.insert(name.clone(), config);
    debug!("job.register: registered handler for '{}'", name);
}

/// Enqueue a job for immediate execution.
///
/// Returns the generated UUID v4 job ID, or an empty string if the job name
/// is not registered.
pub async fn enqueue_job(state: &SharedJobsState, name: String, args: String) -> String {
    enqueue_job_at(state, name, args, now_ms()).await
}

/// Enqueue a job scheduled to run at a specific future Unix millisecond timestamp.
///
/// Returns the generated UUID v4 job ID, or an empty string if the job name
/// is not registered.
pub async fn enqueue_job_at(
    state: &SharedJobsState,
    name: String,
    args: String,
    run_at_ms: u64,
) -> String {
    let mut store = state.lock().await;

    let config = match store.configs.get(&name) {
        Some(c) => c.clone(),
        None => {
            warn!("job.enqueue: unknown job name '{}' — not registered", name);
            return String::new();
        }
    };

    let id = Uuid::new_v4().to_string();
    let ts = now_ms();

    let record = JobRecord {
        id: id.clone(),
        name: name.clone(),
        args,
        status: JobStatus::Pending,
        attempt: 0,
        max_attempts: config.max_attempts,
        backoff: config.backoff,
        delay_ms: config.delay_ms,
        timeout_ms: config.timeout_ms,
        queue: config.queue,
        handler: config.handler,
        scheduled_at: run_at_ms,
        created_at: ts,
        updated_at: ts,
        result: None,
        error: None,
    };

    store.records.insert(id.clone(), record);
    debug!(
        "job.enqueue: enqueued '{}' as {} (scheduled_at={})",
        name, id, run_at_ms
    );
    id
}

/// Cancel a pending job.
///
/// Returns `true` if the job existed and was in `Pending` state.
/// Running, succeeded, failed, or cancelled jobs cannot be cancelled.
pub async fn cancel_job(state: &SharedJobsState, job_id: &str) -> bool {
    let mut store = state.lock().await;
    if let Some(record) = store.records.get_mut(job_id)
        && record.status == JobStatus::Pending
    {
        record.status = JobStatus::Cancelled;
        record.updated_at = now_ms();
        debug!("job.cancel: {} cancelled", job_id);
        return true;
    }
    false
}

/// Return the current status string for a job ID.
///
/// Returns `"not_found"` if the ID is unknown.
pub async fn job_status(state: &SharedJobsState, job_id: &str) -> String {
    let store = state.lock().await;
    store
        .records
        .get(job_id)
        .map(|r| r.status.as_str().to_string())
        .unwrap_or_else(|| "not_found".to_string())
}

/// Return the result or error string for a job.
///
/// - Succeeded → result JSON (or empty string for implicit success).
/// - Failed     → error message.
/// - Otherwise  → empty string.
pub async fn job_result(state: &SharedJobsState, job_id: &str) -> String {
    let store = state.lock().await;
    match store.records.get(job_id) {
        Some(r) => match r.status {
            JobStatus::Succeeded => r.result.clone().unwrap_or_default(),
            JobStatus::Failed    => r.error.clone().unwrap_or_default(),
            _                    => String::new(),
        },
        None => String::new(),
    }
}

/// Register a named cron schedule.
///
/// Returns `true` on success, `false` if the cron expression is unparseable.
pub async fn schedule_cron(
    state: &SharedJobsState,
    name: String,
    cron_expr: String,
    handler: String,
) -> bool {
    if !is_valid_cron(&cron_expr) {
        warn!(
            "schedule.cron: invalid cron expression '{}' for schedule '{}'",
            cron_expr, name
        );
        return false;
    }

    let schedule = CronSchedule {
        name: name.clone(),
        cron_expr,
        handler,
        active: true,
    };
    state.lock().await.schedules.insert(name.clone(), schedule);
    debug!("schedule.cron: registered '{}'", name);
    true
}

/// Cancel a named cron schedule by marking it inactive.
///
/// Returns `true` if the schedule existed and was active.
pub async fn schedule_cancel(state: &SharedJobsState, name: &str) -> bool {
    let mut store = state.lock().await;
    if let Some(sched) = store.schedules.get_mut(name)
        && sched.active
    {
        sched.active = false;
        debug!("schedule.cancel: '{}' deactivated", name);
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Cron expression parsing and next-tick calculation
// ---------------------------------------------------------------------------

/// Validate that a 5-field cron expression can be parsed.
fn is_valid_cron(expr: &str) -> bool {
    parse_cron_fields(expr).is_some()
}

/// A parsed single cron field.
enum CronField {
    /// `*` — matches every value.
    Star,
    /// `*/n` — matches values divisible by n.
    Step(u32),
    /// Explicit set of values (from comma lists and ranges).
    List(Vec<u32>),
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        match self {
            CronField::Star         => true,
            CronField::Step(n)      => *n > 0 && value.is_multiple_of(*n),
            CronField::List(vals)   => vals.contains(&value),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        if s == "*" {
            return Some(CronField::Star);
        }
        if let Some(step_str) = s.strip_prefix("*/") {
            let n: u32 = step_str.parse().ok()?;
            if n == 0 { return None; }
            return Some(CronField::Step(n));
        }
        // Comma-separated list of values and/or ranges.
        let mut values: Vec<u32> = Vec::new();
        for part in s.split(',') {
            if let Some((start_s, end_s)) = part.split_once('-') {
                let a: u32 = start_s.trim().parse().ok()?;
                let b: u32 = end_s.trim().parse().ok()?;
                if a > b { return None; }
                for v in a..=b {
                    values.push(v);
                }
            } else {
                let v: u32 = part.trim().parse().ok()?;
                values.push(v);
            }
        }
        if values.is_empty() { return None; }
        Some(CronField::List(values))
    }
}

/// Parsed 5-field cron expression.
struct CronFields {
    minute: CronField,   // 0–59
    hour:   CronField,   // 0–23
    dom:    CronField,   // 1–31
    month:  CronField,   // 1–12
    dow:    CronField,   // 0–6 (Sunday=0)
}

fn parse_cron_fields(expr: &str) -> Option<CronFields> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    Some(CronFields {
        minute: CronField::parse(parts[0])?,
        hour:   CronField::parse(parts[1])?,
        dom:    CronField::parse(parts[2])?,
        month:  CronField::parse(parts[3])?,
        dow:    CronField::parse(parts[4])?,
    })
}

/// Compute how long until the next tick of a 5-field cron expression.
///
/// Scans forward minute-by-minute from now.  Returns `None` only if no match
/// is found within one year (guards against impossible expressions like
/// `0 0 31 2 *` — Feb 31st never fires).
pub fn next_cron_tick(expr: &str) -> Option<Duration> {
    use chrono::{Datelike, Timelike, Utc};

    let fields = parse_cron_fields(expr)?;
    let now = Utc::now();

    // Start from the next whole minute.
    let base = now
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))?;
    let mut candidate = base + chrono::Duration::minutes(1);

    for _ in 0..(366 * 24 * 60) {
        let m  = candidate.minute();
        let h  = candidate.hour();
        let d  = candidate.day();
        let mo = candidate.month();
        let wd = candidate.weekday().num_days_from_sunday();

        if fields.minute.matches(m)
            && fields.hour.matches(h)
            && fields.dom.matches(d)
            && fields.month.matches(mo)
            && fields.dow.matches(wd)
        {
            let delta = candidate.signed_duration_since(now);
            let secs = delta.num_seconds().max(0) as u64;
            return Some(Duration::from_secs(secs));
        }

        candidate += chrono::Duration::minutes(1);
    }

    None
}

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

/// Worker poll interval — check for due jobs every second.
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often the worker emits a heartbeat log line when jobs are tracked.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum jobs picked up per poll cycle.
const MAX_JOBS_PER_POLL: usize = 20;

/// Start the background worker loop as a detached tokio task.
///
/// The worker polls the in-memory job store every second, picks up pending
/// jobs whose `scheduled_at` ≤ now, executes their WASM handler with the
/// configured retry policy, and updates the job record based on the outcome.
pub fn start_worker_loop(state: SharedJobsState, wasm: Arc<WasmInstance>) {
    tokio::spawn(async move {
        info!("Job worker loop started");
        let mut last_heartbeat = Instant::now();

        loop {
            tokio::time::sleep(WORKER_POLL_INTERVAL).await;

            // Heartbeat every 60 seconds while there are tracked records.
            if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                let count = state.lock().await.records.len();
                if count > 0 {
                    info!("Job worker heartbeat: {} total job records in store", count);
                }
                last_heartbeat = Instant::now();
            }

            // Collect due pending jobs (up to MAX_JOBS_PER_POLL).
            let due_jobs: Vec<JobRecord> = {
                let store = state.lock().await;
                let now = now_ms();
                store
                    .records
                    .values()
                    .filter(|r| r.status == JobStatus::Pending && r.scheduled_at <= now)
                    .take(MAX_JOBS_PER_POLL)
                    .cloned()
                    .collect()
            };

            for job in due_jobs {
                // Atomically claim the job: Pending → Running.
                // If another worker instance already claimed it, skip.
                let claimed = {
                    let mut store = state.lock().await;
                    if let Some(r) = store.records.get_mut(&job.id) {
                        if r.status == JobStatus::Pending {
                            r.status = JobStatus::Running;
                            r.attempt += 1;
                            r.updated_at = now_ms();
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                if !claimed {
                    debug!("job {}: skipped (already claimed)", job.id);
                    continue;
                }

                let attempt = state
                    .lock()
                    .await
                    .records
                    .get(&job.id)
                    .map(|r| r.attempt)
                    .unwrap_or(1);

                debug!(
                    "job {}: starting attempt {}/{}",
                    job.id, attempt, job.max_attempts
                );

                // Execute the WASM handler in spawn_blocking with task-local context.
                let wasm_clone = wasm.clone();
                let handler_name = job.handler.clone();
                let job_id_clone = job.id.clone();
                let job_args_clone = job.args.clone();
                let attempt_i32 = attempt as i32;

                let call_result = tokio::task::spawn_blocking(move || {
                    JOB_CURRENT_ID.sync_scope(job_id_clone, || {
                        JOB_CURRENT_ARGS.sync_scope(job_args_clone, || {
                            JOB_CURRENT_ATTEMPT.sync_scope(attempt_i32, || {
                                JOB_RETRY_OVERRIDE_MS.sync_scope(
                                    std::cell::Cell::new(-1i64),
                                    || {
                                        JOB_EXPLICIT_FAIL.sync_scope(
                                            std::cell::RefCell::new(None),
                                            || {
                                                JOB_EXPLICIT_SUCCEED.sync_scope(
                                                    std::cell::RefCell::new(None),
                                                    || {
                                                        let req = RequestContext {
                                                            method: "JOB".to_string(),
                                                            path: String::new(),
                                                            headers: Vec::new(),
                                                            body: String::new(),
                                                            params: std::collections::HashMap::new(),
                                                            query: std::collections::HashMap::new(),
                                                        };
                                                        let handler_result = wasm_clone
                                                            .call_handler_job(&handler_name, req, None);

                                                        // Collect task-local outcome signals.
                                                        let retry_override = JOB_RETRY_OVERRIDE_MS
                                                            .try_with(|c| c.get())
                                                            .unwrap_or(-1);
                                                        let explicit_fail = JOB_EXPLICIT_FAIL
                                                            .try_with(|r| r.borrow().clone())
                                                            .unwrap_or(None);
                                                        let explicit_succeed = JOB_EXPLICIT_SUCCEED
                                                            .try_with(|r| r.borrow().clone())
                                                            .unwrap_or(None);

                                                        JobOutcome {
                                                            handler_result,
                                                            retry_override_ms: retry_override,
                                                            explicit_fail,
                                                            explicit_succeed,
                                                        }
                                                    },
                                                )
                                            },
                                        )
                                    },
                                )
                            })
                        })
                    })
                })
                .await;

                match call_result {
                    Err(join_err) => {
                        let err_msg = format!("Job handler task panicked: {}", join_err);
                        error!("job {}: {}", job.id, err_msg);
                        apply_failure(&state, &job, attempt, err_msg).await;
                    }
                    Ok(outcome) => {
                        apply_outcome(&state, &job, attempt, outcome).await;
                    }
                }
            }
        }
    });
}

/// The bundled outcome of a single job handler invocation.
struct JobOutcome {
    /// What the WASM handler returned.
    handler_result: Result<(), crate::error::RuntimeError>,
    /// Milliseconds requested by `_job_retry_after` (-1 = not set).
    retry_override_ms: i64,
    /// Reason if `_job_fail` was called.
    explicit_fail: Option<String>,
    /// Result JSON if `_job_succeed` was called.
    explicit_succeed: Option<String>,
}

/// Apply the outcome of a job handler invocation to the job store.
async fn apply_outcome(
    state: &SharedJobsState,
    job: &JobRecord,
    attempt: u32,
    outcome: JobOutcome,
) {
    // _job_succeed takes highest priority regardless of handler error.
    if let Some(result_json) = outcome.explicit_succeed {
        let mut store = state.lock().await;
        if let Some(r) = store.records.get_mut(&job.id) {
            r.status = JobStatus::Succeeded;
            r.result = Some(result_json);
            r.updated_at = now_ms();
        }
        info!("job {}: succeeded (explicit, attempt {})", job.id, attempt);
        return;
    }

    // _job_fail forces immediate failure (no more retries).
    if let Some(reason) = outcome.explicit_fail {
        let mut store = state.lock().await;
        if let Some(r) = store.records.get_mut(&job.id) {
            r.status = JobStatus::Failed;
            r.error = Some(reason.clone());
            r.updated_at = now_ms();
        }
        warn!(
            "job {}: explicitly failed (attempt {}): {}",
            job.id, attempt, reason
        );
        return;
    }

    match outcome.handler_result {
        Ok(()) => {
            // Handler returned normally without explicit signals.
            let mut store = state.lock().await;
            if let Some(r) = store.records.get_mut(&job.id) {
                r.status = JobStatus::Succeeded;
                r.updated_at = now_ms();
            }
            info!("job {}: succeeded (implicit, attempt {})", job.id, attempt);
        }
        Err(e) => {
            apply_failure_with_retry_override(
                state,
                job,
                attempt,
                e.to_string(),
                outcome.retry_override_ms,
            )
            .await;
        }
    }
}

/// Apply failure logic: retry if attempts remain, otherwise mark permanently failed.
async fn apply_failure(
    state: &SharedJobsState,
    job: &JobRecord,
    attempt: u32,
    err_msg: String,
) {
    apply_failure_with_retry_override(state, job, attempt, err_msg, -1).await;
}

async fn apply_failure_with_retry_override(
    state: &SharedJobsState,
    job: &JobRecord,
    attempt: u32,
    err_msg: String,
    retry_override_ms: i64,
) {
    if attempt < job.max_attempts {
        let next_delay = if retry_override_ms >= 0 {
            retry_override_ms as u64
        } else {
            job.backoff.compute_delay(job.delay_ms, attempt)
        };

        let next_scheduled = now_ms() + next_delay;

        {
            let mut store = state.lock().await;
            if let Some(r) = store.records.get_mut(&job.id) {
                r.status = JobStatus::Pending;
                r.scheduled_at = next_scheduled;
                r.error = Some(err_msg.clone());
                r.updated_at = now_ms();
            }
        }

        warn!(
            "job {}: attempt {} failed, retry in {}ms: {}",
            job.id, attempt, next_delay, err_msg
        );
    } else {
        let mut store = state.lock().await;
        if let Some(r) = store.records.get_mut(&job.id) {
            r.status = JobStatus::Failed;
            r.error = Some(err_msg.clone());
            r.updated_at = now_ms();
        }
        error!(
            "job {}: permanently failed after {} attempt(s): {}",
            job.id, attempt, err_msg
        );
    }
}

// ---------------------------------------------------------------------------
// Cron scheduler
// ---------------------------------------------------------------------------

/// Start the cron scheduler monitor.
///
/// A monitor task polls the schedule registry every 5 seconds.  When it
/// discovers a new active schedule it spawns a dedicated per-schedule task.
/// Each per-schedule task computes the next tick, sleeps until then, fires the
/// WASM handler, then loops.  When a schedule is cancelled (active = false)
/// the task exits cleanly after its next wake-up.
pub fn start_cron_scheduler(state: SharedJobsState, wasm: Arc<WasmInstance>) {
    tokio::spawn(async move {
        info!("Cron scheduler monitor started");

        let mut running: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;

            // Collect active schedules that don't have a running task yet.
            let new_schedules: Vec<CronSchedule> = {
                let store = state.lock().await;
                store
                    .schedules
                    .values()
                    .filter(|s| s.active && !running.contains(&s.name))
                    .cloned()
                    .collect()
            };

            for sched in new_schedules {
                running.insert(sched.name.clone());
                let state_clone = state.clone();
                let wasm_clone = wasm.clone();

                tokio::spawn(async move {
                    info!(
                        "cron '{}': task started (expr: '{}')",
                        sched.name, sched.cron_expr
                    );

                    loop {
                        // Check active flag before sleeping.
                        let still_active = state_clone
                            .lock()
                            .await
                            .schedules
                            .get(&sched.name)
                            .map(|s| s.active)
                            .unwrap_or(false);

                        if !still_active {
                            info!("cron '{}': deactivated, task exiting", sched.name);
                            break;
                        }

                        let wait = match next_cron_tick(&sched.cron_expr) {
                            Some(d) => d,
                            None => {
                                warn!(
                                    "cron '{}': cannot compute next tick for '{}', deactivating",
                                    sched.name, sched.cron_expr
                                );
                                break;
                            }
                        };

                        debug!(
                            "cron '{}': next tick in {:.1}s",
                            sched.name,
                            wait.as_secs_f64()
                        );

                        tokio::time::sleep(wait).await;

                        // Re-check active flag after sleeping.
                        let still_active = state_clone
                            .lock()
                            .await
                            .schedules
                            .get(&sched.name)
                            .map(|s| s.active)
                            .unwrap_or(false);

                        if !still_active {
                            info!("cron '{}': cancelled during sleep, task exiting", sched.name);
                            break;
                        }

                        info!(
                            "cron '{}': firing handler '{}'",
                            sched.name, sched.handler
                        );

                        let wasm_fire = wasm_clone.clone();
                        let h_name = sched.handler.clone();
                        let sched_name = sched.name.clone();

                        let result = tokio::task::spawn_blocking(move || {
                            let req = RequestContext {
                                method: "CRON".to_string(),
                                path: sched_name,
                                headers: Vec::new(),
                                body: String::new(),
                                params: std::collections::HashMap::new(),
                                query: std::collections::HashMap::new(),
                            };
                            wasm_fire.call_handler_job(&h_name, req, None)
                        })
                        .await;

                        match result {
                            Ok(Ok(())) => {
                                debug!("cron '{}': handler completed", sched.name);
                            }
                            Ok(Err(e)) => {
                                warn!("cron '{}': handler error: {}", sched.name, e);
                            }
                            Err(join_err) => {
                                warn!("cron '{}': handler panicked: {}", sched.name, join_err);
                            }
                        }
                    }
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_enqueue() {
        let state = create_shared_jobs_state();

        register_job(
            &state,
            "sendWelcomeEmail".to_string(),
            "sendWelcomeEmail_handler".to_string(),
            3,
            BackoffStrategy::Exponential,
            1000,
            0,
            "default".to_string(),
        )
        .await;

        let id = enqueue_job(
            &state,
            "sendWelcomeEmail".to_string(),
            r#"{"email":"test@example.com"}"#.to_string(),
        )
        .await;

        assert!(!id.is_empty(), "enqueue should return a non-empty UUID job ID");

        let status = job_status(&state, &id).await;
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn test_enqueue_unknown_job_returns_empty() {
        let state = create_shared_jobs_state();
        let id = enqueue_job(&state, "nonexistent".to_string(), "{}".to_string()).await;
        assert!(id.is_empty(), "unknown job type should return empty string");
    }

    #[tokio::test]
    async fn test_cancel_pending_job() {
        let state = create_shared_jobs_state();

        register_job(
            &state,
            "cancelMe".to_string(),
            "cancelMe_handler".to_string(),
            1,
            BackoffStrategy::Fixed,
            0,
            0,
            "default".to_string(),
        )
        .await;

        let id = enqueue_job(&state, "cancelMe".to_string(), "{}".to_string()).await;
        assert!(!id.is_empty());

        let cancelled = cancel_job(&state, &id).await;
        assert!(cancelled, "cancel should succeed for a pending job");

        let status = job_status(&state, &id).await;
        assert_eq!(status, "cancelled");

        // Second cancel should return false.
        let cancelled_again = cancel_job(&state, &id).await;
        assert!(!cancelled_again, "second cancel of same job should fail");
    }

    #[tokio::test]
    async fn test_cancel_nonexistent_returns_false() {
        let state = create_shared_jobs_state();
        let cancelled = cancel_job(&state, "00000000-0000-0000-0000-000000000000").await;
        assert!(!cancelled);
    }

    #[tokio::test]
    async fn test_job_result_pending_is_empty() {
        let state = create_shared_jobs_state();

        register_job(
            &state,
            "resultTest".to_string(),
            "resultTest_handler".to_string(),
            1,
            BackoffStrategy::Fixed,
            0,
            0,
            "default".to_string(),
        )
        .await;

        let id = enqueue_job(&state, "resultTest".to_string(), "{}".to_string()).await;
        let result = job_result(&state, &id).await;
        assert_eq!(result, "", "pending job should return empty result");
    }

    #[tokio::test]
    async fn test_enqueue_at_sets_future_schedule() {
        let state = create_shared_jobs_state();

        register_job(
            &state,
            "futureTask".to_string(),
            "futureTask_handler".to_string(),
            1,
            BackoffStrategy::Fixed,
            0,
            0,
            "default".to_string(),
        )
        .await;

        let future_ms = now_ms() + 10_000;
        let id = enqueue_job_at(
            &state,
            "futureTask".to_string(),
            "{}".to_string(),
            future_ms,
        )
        .await;

        assert!(!id.is_empty());

        // The worker poll should not pick it up yet (scheduled 10s in the future).
        let store = state.lock().await;
        let record = store.records.get(&id).unwrap();
        assert_eq!(record.status, JobStatus::Pending);
        assert!(record.scheduled_at > now_ms());
    }

    #[test]
    fn test_backoff_fixed() {
        let b = BackoffStrategy::Fixed;
        assert_eq!(b.compute_delay(1000, 1), 1000);
        assert_eq!(b.compute_delay(1000, 2), 1000);
        assert_eq!(b.compute_delay(1000, 5), 1000);
    }

    #[test]
    fn test_backoff_exponential() {
        let b = BackoffStrategy::Exponential;
        // delay * 2^(attempt-1)
        assert_eq!(b.compute_delay(1000, 1), 1000);  // 1000 * 2^0
        assert_eq!(b.compute_delay(1000, 2), 2000);  // 1000 * 2^1
        assert_eq!(b.compute_delay(1000, 3), 4000);  // 1000 * 2^2
        assert_eq!(b.compute_delay(1000, 4), 8000);  // 1000 * 2^3
    }

    #[test]
    fn test_backoff_strategy_parse() {
        assert_eq!(BackoffStrategy::parse("exponential"), BackoffStrategy::Exponential);
        assert_eq!(BackoffStrategy::parse("EXPONENTIAL"), BackoffStrategy::Exponential);
        assert_eq!(BackoffStrategy::parse("fixed"), BackoffStrategy::Fixed);
        assert_eq!(BackoffStrategy::parse("linear"), BackoffStrategy::Fixed);
        assert_eq!(BackoffStrategy::parse("unknown"), BackoffStrategy::Fixed);
    }

    #[test]
    fn test_cron_validation_valid() {
        assert!(is_valid_cron("* * * * *"));
        assert!(is_valid_cron("*/5 * * * *"));
        assert!(is_valid_cron("0 0 * * *"));
        assert!(is_valid_cron("*/1 * * * *"));
        assert!(is_valid_cron("0 9 * * 1-5"));
        assert!(is_valid_cron("0,30 * * * *"));
    }

    #[test]
    fn test_cron_validation_invalid() {
        assert!(!is_valid_cron("invalid"));
        assert!(!is_valid_cron("* * * *"));       // too few fields
        assert!(!is_valid_cron("* * * * * *"));   // too many fields
        assert!(!is_valid_cron("*/0 * * * *"));   // zero step
        assert!(!is_valid_cron(""));
    }

    #[tokio::test]
    async fn test_schedule_cron_and_cancel() {
        let state = create_shared_jobs_state();

        let ok = schedule_cron(
            &state,
            "dailyDigest".to_string(),
            "0 0 * * *".to_string(),
            "daily_digest_handler".to_string(),
        )
        .await;
        assert!(ok, "valid cron expression should register");

        let cancelled = schedule_cancel(&state, "dailyDigest").await;
        assert!(cancelled, "should cancel an active schedule");

        let cancelled_again = schedule_cancel(&state, "dailyDigest").await;
        assert!(!cancelled_again, "second cancel should fail");
    }

    #[tokio::test]
    async fn test_schedule_invalid_cron_returns_false() {
        let state = create_shared_jobs_state();
        let ok = schedule_cron(
            &state,
            "bad".to_string(),
            "not a cron".to_string(),
            "handler".to_string(),
        )
        .await;
        assert!(!ok, "invalid cron expression should return false");
    }

    #[test]
    fn test_task_local_defaults_outside_handler() {
        assert_eq!(current_job_id(), "");
        assert_eq!(current_job_args(), "");
        assert_eq!(current_job_attempt(), 0);
    }

    #[test]
    fn test_status_strings() {
        assert_eq!(JobStatus::Pending.as_str(),   "pending");
        assert_eq!(JobStatus::Running.as_str(),   "running");
        assert_eq!(JobStatus::Succeeded.as_str(), "succeeded");
        assert_eq!(JobStatus::Failed.as_str(),    "failed");
        assert_eq!(JobStatus::Cancelled.as_str(), "cancelled");
    }
}
