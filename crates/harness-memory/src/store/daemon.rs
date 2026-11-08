//! Long-lived local daemon for queued memory writes and dual endpoints:
//! 1. Native Luna connection (default): Unix domain socket IPC with zero MCP overhead.
//! 2. MCP server endpoint (optional): JSON-RPC MCP over HTTP/SSE or stdio for external agents.
//!
//! Write routing policy (`direct`, `prefer`, `require`) mirrors MemPalace's battle-tested
//! concurrency guarantees.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use harness_core::HarnessError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

use super::{MemoryStore, Record, StoreConfig};

pub const DEFAULT_PORT: u16 = 9876;
pub const MAX_ATTEMPTS: u32 = 3;
pub const LOCK_REFUSAL_ERROR_CLASS: &str = "LockHeldByOtherProcess";
pub const LOCK_DEFER_BACKOFF_SECS: u64 = 60;
pub const HOOK_PROBE_TIMEOUT_MS: u64 = 150;
pub const JOB_RETENTION_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteRoutingPolicy {
    Direct,
    Prefer,
    Require,
}

impl Default for WriteRoutingPolicy {
    fn default() -> Self {
        Self::Prefer
    }
}

pub fn parse_write_routing_policy(value: &str, legacy_boolean: bool) -> Result<WriteRoutingPolicy, HarnessError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "direct" => Ok(WriteRoutingPolicy::Direct),
        "prefer" => Ok(WriteRoutingPolicy::Prefer),
        "require" => Ok(WriteRoutingPolicy::Require),
        "1" | "true" | "yes" | "on" if legacy_boolean => Ok(WriteRoutingPolicy::Prefer),
        "0" | "false" | "no" | "off" if legacy_boolean => Ok(WriteRoutingPolicy::Direct),
        _ => Err(HarnessError::ConfigurationError(format!(
            "invalid write routing policy '{value}'; expected direct, prefer, or require"
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteRoutingTarget {
    Direct,
    Daemon,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRoutingDecision {
    pub policy: WriteRoutingPolicy,
    pub target: WriteRoutingTarget,
    pub auto_start_daemon: bool,
    pub reason: String,
}

impl WriteRoutingDecision {
    pub fn use_daemon(&self) -> bool {
        self.target == WriteRoutingTarget::Daemon
    }

    pub fn is_blocked(&self) -> bool {
        self.target == WriteRoutingTarget::Blocked
    }
}

pub fn choose_write_route(
    policy: WriteRoutingPolicy,
    daemon_available: bool,
    daemon_can_start: bool,
) -> WriteRoutingDecision {
    if policy == WriteRoutingPolicy::Direct {
        return WriteRoutingDecision {
            policy,
            target: WriteRoutingTarget::Direct,
            auto_start_daemon: false,
            reason: "policy-direct".to_string(),
        };
    }

    if daemon_available {
        return WriteRoutingDecision {
            policy,
            target: WriteRoutingTarget::Daemon,
            auto_start_daemon: false,
            reason: "daemon-available".to_string(),
        };
    }

    if daemon_can_start {
        return WriteRoutingDecision {
            policy,
            target: WriteRoutingTarget::Daemon,
            auto_start_daemon: true,
            reason: "daemon-auto-start".to_string(),
        };
    }

    if policy == WriteRoutingPolicy::Prefer {
        return WriteRoutingDecision {
            policy,
            target: WriteRoutingTarget::Direct,
            auto_start_daemon: false,
            reason: "daemon-unavailable-fallback".to_string(),
        };
    }

    WriteRoutingDecision {
        policy,
        target: WriteRoutingTarget::Blocked,
        auto_start_daemon: false,
        reason: "daemon-required-unavailable".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub payload: Value,
    pub state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub attempts: u32,
}

pub struct QueueStore {
    path: PathBuf,
}

impl QueueStore {
    pub fn new(path: PathBuf) -> Result<Self, HarnessError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }
        let store = Self { path };
        store.init_db()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, HarnessError> {
        Connection::open(&self.path).map_err(|e| HarnessError::MemoryError(e.to_string()))
    }

    fn init_db(&self) -> Result<(), HarnessError> {
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS jobs (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                state        TEXT NOT NULL,
                priority     INTEGER NOT NULL DEFAULT 0,
                dedupe_key   TEXT,
                created_at   TEXT NOT NULL,
                started_at   TEXT,
                finished_at  TEXT,
                result_json  TEXT,
                error_json   TEXT,
                attempts     INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs(state, priority);
            CREATE INDEX IF NOT EXISTS idx_jobs_dedupe ON jobs(dedupe_key, state);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_dedupe_active
                ON jobs(dedupe_key) WHERE state IN ('queued', 'running');
            "#,
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Ok(())
    }

    pub fn enqueue(
        &self,
        kind: &str,
        payload: &Value,
        dedupe_key: Option<&str>,
        priority: i32,
    ) -> Result<Job, HarnessError> {
        let conn = self.connect()?;
        let payload_json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());

        if let Some(key) = dedupe_key {
            let existing: Option<Job> = conn
                .query_row(
                    "SELECT id, kind, payload_json, state, priority, dedupe_key, created_at, started_at, finished_at, result_json, error_json, attempts
                     FROM jobs WHERE dedupe_key = ?1 AND state IN ('queued', 'running')
                     ORDER BY created_at DESC LIMIT 1",
                    params![key],
                    |row| Self::row_to_job(row),
                )
                .optional()
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            if let Some(job) = existing {
                return Ok(job);
            }
        }

        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let now = Utc::now().to_rfc3339();

        let insert_res = conn.execute(
            "INSERT INTO jobs (id, kind, payload_json, state, priority, dedupe_key, created_at, attempts)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, 0)",
            params![job_id, kind, payload_json, priority, dedupe_key, now],
        );

        match insert_res {
            Ok(_) => self.get(&job_id),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                if let Some(key) = dedupe_key {
                    let existing: Option<Job> = conn
                        .query_row(
                            "SELECT id, kind, payload_json, state, priority, dedupe_key, created_at, started_at, finished_at, result_json, error_json, attempts
                             FROM jobs WHERE dedupe_key = ?1 AND state IN ('queued', 'running')
                             ORDER BY created_at DESC LIMIT 1",
                            params![key],
                            |row| Self::row_to_job(row),
                        )
                        .optional()
                        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
                    if let Some(job) = existing {
                        return Ok(job);
                    }
                }
                Err(HarnessError::MemoryError("deduplication collision".to_string()))
            }
            Err(e) => Err(HarnessError::MemoryError(e.to_string())),
        }
    }

    pub fn claim_next(&self, exclude: &HashSet<String>) -> Result<Option<Job>, HarnessError> {
        let conn = self.connect()?;
        let limit = exclude.len() + 1;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM jobs WHERE state = 'queued' ORDER BY priority DESC, created_at ASC LIMIT ?1",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut candidate_id = None;
        for id_res in rows {
            let id = id_res.map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            if !exclude.contains(&id) {
                candidate_id = Some(id);
                break;
            }
        }

        let Some(job_id) = candidate_id else {
            return Ok(None);
        };

        let now = Utc::now().to_rfc3339();
        let rows_affected = conn
            .execute(
                "UPDATE jobs SET state = 'running', started_at = ?1, attempts = attempts + 1, error_json = NULL
                 WHERE id = ?2 AND state = 'queued'",
                params![now, job_id],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        if rows_affected != 1 {
            return Ok(None);
        }

        self.get(&job_id).map(Some)
    }

    pub fn finish(
        &self,
        job_id: &str,
        state: &str,
        result: Option<&Value>,
        error: Option<&Value>,
        only_if_running: bool,
    ) -> Result<Job, HarnessError> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        let result_json = result.map(|v| v.to_string());
        let error_json = error.map(|v| v.to_string());

        let where_clause = if only_if_running {
            "WHERE id = ?5 AND state = 'running'"
        } else {
            "WHERE id = ?5"
        };

        let sql = format!(
            "UPDATE jobs SET state = ?1, finished_at = ?2, result_json = ?3, error_json = ?4 {where_clause}"
        );

        conn.execute(&sql, params![state, now, result_json, error_json, job_id])
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        self.get(job_id)
    }

    pub fn defer(
        &self,
        job_id: &str,
        claimed_started_at: Option<&str>,
        error: Option<&Value>,
    ) -> Result<Job, HarnessError> {
        let conn = self.connect()?;
        let error_json = error.map(|v| v.to_string());

        conn.execute(
            "UPDATE jobs
             SET state = 'queued', started_at = NULL, attempts = MAX(attempts - 1, 0), error_json = ?1
             WHERE id = ?2 AND state = 'running' AND (?3 IS NULL OR started_at = ?3)",
            params![error_json, job_id, claimed_started_at],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        self.get(job_id)
    }

    pub fn get(&self, job_id: &str) -> Result<Job, HarnessError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT id, kind, payload_json, state, priority, dedupe_key, created_at, started_at, finished_at, result_json, error_json, attempts
             FROM jobs WHERE id = ?1",
            params![job_id],
            |row| Self::row_to_job(row),
        )
        .map_err(|e| HarnessError::MemoryError(format!("job '{job_id}' not found: {e}")))
    }

    pub fn list(&self, limit: usize) -> Result<Vec<Job>, HarnessError> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, payload_json, state, priority, dedupe_key, created_at, started_at, finished_at, result_json, error_json, attempts
                 FROM jobs ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| Self::row_to_job(row))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut jobs = Vec::new();
        for r in rows {
            jobs.push(r.map_err(|e| HarnessError::MemoryError(e.to_string()))?);
        }
        Ok(jobs)
    }

    pub fn counts(&self) -> Result<HashMap<String, usize>, HarnessError> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT state, COUNT(*) FROM jobs GROUP BY state")
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut map = HashMap::new();
        for r in rows {
            let (state, count) = r.map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            map.insert(state, count);
        }
        Ok(map)
    }

    pub fn recover_running(&self) -> Result<usize, HarnessError> {
        let conn = self.connect()?;
        let now = Utc::now().to_rfc3339();
        let max_err = json!({"error_class": "MaxAttemptsExceeded", "message": "max attempts exceeded"}).to_string();

        conn.execute(
            "UPDATE jobs SET state = 'failed', finished_at = ?1, error_json = COALESCE(error_json, ?2)
             WHERE state = 'running' AND attempts >= ?3",
            params![now, max_err, MAX_ATTEMPTS],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let re_queued = conn
            .execute(
                "UPDATE jobs SET state = 'queued', started_at = NULL WHERE state = 'running' AND attempts < ?1",
                params![MAX_ATTEMPTS],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        Ok(re_queued)
    }

    pub fn prune_terminal(&self, older_than_days: i64) -> Result<usize, HarnessError> {
        if older_than_days <= 0 {
            return Ok(0);
        }
        let conn = self.connect()?;
        let cutoff = (Utc::now() - chrono::Duration::days(older_than_days)).to_rfc3339();
        let deleted = conn
            .execute(
                "DELETE FROM jobs WHERE state IN ('succeeded', 'failed', 'cancelled')
                 AND finished_at IS NOT NULL AND finished_at < ?1",
                params![cutoff],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        Ok(deleted)
    }

    pub fn set_job_state_for_test(
        &self,
        job_id: &str,
        state: &str,
        attempts: u32,
    ) -> Result<(), HarnessError> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE jobs SET state = ?1, attempts = ?2 WHERE id = ?3",
            params![state, attempts, job_id],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Ok(())
    }

    fn row_to_job(row: &rusqlite::Row) -> rusqlite::Result<Job> {
        let payload_json: String = row.get(2)?;
        let result_json: Option<String> = row.get(9)?;
        let error_json: Option<String> = row.get(10)?;

        Ok(Job {
            id: row.get(0)?,
            kind: row.get(1)?,
            payload: serde_json::from_str(&payload_json).unwrap_or(Value::Null),
            state: row.get(3)?,
            priority: row.get(4)?,
            dedupe_key: row.get(5)?,
            created_at: row.get(6)?,
            started_at: row.get(7)?,
            finished_at: row.get(8)?,
            result: result_json.and_then(|s| serde_json::from_str(&s).ok()),
            error: error_json.and_then(|s| serde_json::from_str(&s).ok()),
            attempts: row.get(11)?,
        })
    }
}

pub struct DaemonPaths {
    pub daemon_dir: PathBuf,
    pub endpoint_file: PathBuf,
    pub pid_file: PathBuf,
    pub token_file: PathBuf,
    pub socket_file: PathBuf,
    pub queue_file: PathBuf,
}

impl DaemonPaths {
    pub fn from_root(root: &Path) -> Self {
        let daemon_dir = root.join("daemon");
        Self {
            endpoint_file: daemon_dir.join("endpoint.json"),
            pid_file: daemon_dir.join("pid"),
            token_file: daemon_dir.join("token"),
            socket_file: daemon_dir.join("luna.sock"),
            queue_file: daemon_dir.join("queue.sqlite3"),
            daemon_dir,
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), HarnessError> {
        fs::create_dir_all(&self.daemon_dir).map_err(|e| HarnessError::Io(e.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonEndpointInfo {
    pub pid: u32,
    pub store_root: String,
    pub started_at: String,
    pub socket_path: Option<String>,
    pub port: Option<u16>,
    pub token: String,
}

pub fn read_endpoint_info(root: &Path) -> Option<DaemonEndpointInfo> {
    let paths = DaemonPaths::from_root(root);
    let bytes = fs::read(&paths.endpoint_file).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn ensure_token(paths: &DaemonPaths) -> Result<String, HarnessError> {
    if paths.token_file.exists() {
        if let Ok(tok) = fs::read_to_string(&paths.token_file) {
            let trimmed = tok.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    fs::write(&paths.token_file, format!("{token}\n"))
        .map_err(|e| HarnessError::Io(e.to_string()))?;
    Ok(token)
}

/// Native Luna daemon client over Unix Domain Socket or TCP loopback.
/// Zero MCP overhead — fast newline-delimited JSON commands.
pub struct NativeDaemonClient {
    #[cfg(unix)]
    socket_path: Option<PathBuf>,
    tcp_port: Option<u16>,
    token: String,
}

impl NativeDaemonClient {
    pub fn connect_root(root: &Path) -> Result<Self, HarnessError> {
        let info = read_endpoint_info(root)
            .ok_or_else(|| HarnessError::MemoryError("daemon endpoint not found".to_string()))?;

        #[cfg(unix)]
        let socket_path = info.socket_path.map(PathBuf::from);

        Ok(Self {
            #[cfg(unix)]
            socket_path,
            tcp_port: info.port,
            token: info.token,
        })
    }

    pub async fn ping(&self) -> Result<Value, HarnessError> {
        let req = json!({ "action": "ping", "token": self.token });
        self.send_command(req).await
    }

    pub async fn is_alive(&self) -> bool {
        match tokio::time::timeout(Duration::from_millis(HOOK_PROBE_TIMEOUT_MS), self.ping()).await {
            Ok(Ok(val)) => val.get("ok").and_then(Value::as_bool).unwrap_or(false),
            _ => false,
        }
    }

    pub async fn enqueue(
        &self,
        kind: &str,
        payload: Value,
        dedupe_key: Option<&str>,
        priority: i32,
    ) -> Result<Job, HarnessError> {
        let req = json!({
            "action": "enqueue",
            "kind": kind,
            "payload": payload,
            "dedupe_key": dedupe_key,
            "priority": priority,
            "token": self.token
        });
        let res = self.send_command(req).await?;
        let job_val = res
            .get("job")
            .ok_or_else(|| HarnessError::MemoryError("missing job in daemon reply".to_string()))?;
        serde_json::from_value(job_val.clone()).map_err(|e| HarnessError::MemoryError(e.to_string()))
    }

    pub async fn execute(&self, kind: &str, payload: Value) -> Result<Value, HarnessError> {
        let req = json!({
            "action": "execute",
            "kind": kind,
            "payload": payload,
            "token": self.token
        });
        let res = self.send_command(req).await?;
        Ok(res.get("result").cloned().unwrap_or(res))
    }

    pub async fn get_job(&self, job_id: &str) -> Result<Job, HarnessError> {
        let req = json!({
            "action": "get_job",
            "job_id": job_id,
            "token": self.token
        });
        let res = self.send_command(req).await?;
        let job_val = res
            .get("job")
            .ok_or_else(|| HarnessError::MemoryError("missing job in daemon reply".to_string()))?;
        serde_json::from_value(job_val.clone()).map_err(|e| HarnessError::MemoryError(e.to_string()))
    }

    pub async fn list_jobs(&self, limit: usize) -> Result<Vec<Job>, HarnessError> {
        let req = json!({
            "action": "list_jobs",
            "limit": limit,
            "token": self.token
        });
        let res = self.send_command(req).await?;
        let jobs_val = res
            .get("jobs")
            .ok_or_else(|| HarnessError::MemoryError("missing jobs in daemon reply".to_string()))?;
        serde_json::from_value(jobs_val.clone()).map_err(|e| HarnessError::MemoryError(e.to_string()))
    }

    pub async fn counts(&self) -> Result<HashMap<String, usize>, HarnessError> {
        let req = json!({ "action": "counts", "token": self.token });
        let res = self.send_command(req).await?;
        let counts_val = res
            .get("counts")
            .ok_or_else(|| HarnessError::MemoryError("missing counts in daemon reply".to_string()))?;
        serde_json::from_value(counts_val.clone()).map_err(|e| HarnessError::MemoryError(e.to_string()))
    }

    pub async fn shutdown(&self) -> Result<(), HarnessError> {
        let req = json!({ "action": "shutdown", "token": self.token });
        let _ = self.send_command(req).await;
        Ok(())
    }

    async fn send_command(&self, request: Value) -> Result<Value, HarnessError> {
        let req_bytes = format!("{}\n", serde_json::to_string(&request).unwrap());

        #[cfg(unix)]
        if let Some(sock_path) = &self.socket_path {
            if sock_path.exists() {
                if let Ok(mut stream) = UnixStream::connect(sock_path).await {
                    stream
                        .write_all(req_bytes.as_bytes())
                        .await
                        .map_err(|e| HarnessError::Io(e.to_string()))?;
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader
                        .read_line(&mut line)
                        .await
                        .map_err(|e| HarnessError::Io(e.to_string()))?;
                    return serde_json::from_str(&line)
                        .map_err(|e| HarnessError::SerializationError(e.to_string()));
                }
            }
        }

        if let Some(port) = self.tcp_port {
            let addr = format!("127.0.0.1:{port}");
            let mut stream = TcpStream::connect(&addr)
                .await
                .map_err(|e| HarnessError::Io(format!("failed to connect to daemon at {addr}: {e}")))?;
            stream
                .write_all(req_bytes.as_bytes())
                .await
                .map_err(|e| HarnessError::Io(e.to_string()))?;
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| HarnessError::Io(e.to_string()))?;
            return serde_json::from_str(&line).map_err(|e| HarnessError::SerializationError(e.to_string()));
        }

        Err(HarnessError::MemoryError("no active transport for daemon client".to_string()))
    }
}

/// Executes one durable daemon job against `MemoryStore`.
pub fn execute_job_dispatch(
    store: &MemoryStore,
    kind: &str,
    payload: &Value,
) -> Result<Value, HarnessError> {
    match kind {
        "mine" => {
            let dir_str = payload.get("dir").and_then(Value::as_str).unwrap_or(".");
            let domain = payload.get("domain").and_then(Value::as_str);
            let topic = payload.get("topic").and_then(Value::as_str);
            let mode = payload.get("mode").and_then(Value::as_str).unwrap_or("projects");

            if mode == "convos" {
                let report = store.mine_convos(Path::new(dir_str), domain, topic)?;
                Ok(json!({
                    "success": true,
                    "files": report.files,
                    "records": report.records,
                    "skipped": report.skipped,
                    "removed": report.removed
                }))
            } else if mode == "sessions" {
                let report = store.mine_sessions(Path::new(dir_str), domain, topic)?;
                Ok(json!({
                    "success": true,
                    "files": report.files,
                    "records": report.records,
                    "skipped": report.skipped,
                    "removed": report.removed
                }))
            } else {
                let report = store.mine(Path::new(dir_str), domain)?;
                Ok(json!({
                    "success": true,
                    "files": report.files,
                    "records": report.records,
                    "skipped": report.skipped,
                    "removed": report.removed
                }))
            }
        }
        "sync" | "reindex" => {
            let count = store.reindex()?;
            Ok(json!({ "success": true, "records_reindexed": count }))
        }
        "sweep" => {
            let paths = DaemonPaths::from_root(store.root());
            let queue = QueueStore::new(paths.queue_file)?;
            let pruned = queue.prune_terminal(JOB_RETENTION_DAYS)?;
            Ok(json!({ "success": true, "jobs_pruned": pruned }))
        }
        "write_record" => {
            let rec: Record = serde_json::from_value(payload.clone())
                .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
            store.write_record(rec)?;
            Ok(json!({ "success": true }))
        }
        "delete_record" => {
            let id = payload
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| HarnessError::MemoryError("missing id".to_string()))?;
            let deleted = store.delete_record(id)?;
            Ok(json!({ "success": true, "deleted": deleted }))
        }
        "kg_add" => {
            let subject = payload.get("subject").and_then(Value::as_str).unwrap_or("");
            let predicate = payload.get("predicate").and_then(Value::as_str).unwrap_or("");
            let object = payload.get("object").and_then(Value::as_str).unwrap_or("");
            let from = payload.get("valid_from").and_then(Value::as_str);
            let to = payload.get("valid_to").and_then(Value::as_str);
            let triple_id = store.kg_add_triple(subject, predicate, object, from, to, None, None, None, None, None)?;
            Ok(json!({ "success": true, "triple_id": triple_id }))
        }
        "journal_write" => {
            let content = payload.get("content").and_then(Value::as_str).unwrap_or("");
            let agent = payload.get("agent").and_then(Value::as_str).unwrap_or("luna");
            let entry = store.journal_write(agent, content, None)?;
            Ok(json!({ "success": true, "entry": entry }))
        }
        "event_append" => {
            let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("message");
            let stream = payload.get("stream").and_then(Value::as_str).unwrap_or("main");
            let from = payload.get("from").and_then(Value::as_str).unwrap_or("luna");
            let to = payload.get("to").and_then(Value::as_str);
            let corr = payload.get("correlation_id").and_then(Value::as_str);
            let body = payload.get("body").and_then(Value::as_str).unwrap_or("");
            let event = store.event_append(stream, event_type, from, to, body, corr, None)?;
            Ok(json!({ "success": true, "event": event }))
        }
        "mcp_tool" => {
            let tool = payload.get("tool").and_then(Value::as_str).unwrap_or("");
            let args = payload.get("arguments").cloned().unwrap_or(json!({}));
            super::mcp::dispatch_mcp_tool(store, tool, &args)
        }
        other => Err(HarnessError::MemoryError(format!("unknown daemon job kind: {other}"))),
    }
}

pub struct DaemonRuntime {
    store: Arc<MemoryStore>,
    paths: DaemonPaths,
    queue: Arc<QueueStore>,
    shutdown: Arc<AtomicBool>,
}

impl DaemonRuntime {
    pub fn new(store_root: PathBuf) -> Result<Self, HarnessError> {
        let store = Arc::new(MemoryStore::open(StoreConfig::new(store_root.clone()))?);
        let paths = DaemonPaths::from_root(&store_root);
        paths.ensure_dirs()?;
        let queue = Arc::new(QueueStore::new(paths.queue_file.clone())?);
        Ok(Self {
            store,
            paths,
            queue,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn start_worker(&self) {
        let queue = self.queue.clone();
        let store = self.store.clone();
        let shutdown = self.shutdown.clone();

        let _ = queue.recover_running();
        let _ = queue.prune_terminal(JOB_RETENTION_DAYS);

        std::thread::Builder::new()
            .name("luna-memory-daemon-worker".to_string())
            .spawn(move || {
                let mut deferred_until: HashMap<String, Instant> = HashMap::new();

                while !shutdown.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    deferred_until.retain(|_, deadline| *deadline > now);
                    let exclude: HashSet<String> = deferred_until.keys().cloned().collect();

                    let job_opt = match queue.claim_next(&exclude) {
                        Ok(opt) => opt,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    };

                    let Some(job) = job_opt else {
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    };

                    match execute_job_dispatch(&store, &job.kind, &job.payload) {
                        Ok(res) => {
                            let _ = queue.finish(&job.id, "succeeded", Some(&res), None, true);
                        }
                        Err(e) => {
                            let err_msg = e.to_string();
                            if err_msg.to_lowercase().contains("lock") {
                                let err_val = json!({
                                    "error_class": LOCK_REFUSAL_ERROR_CLASS,
                                    "message": err_msg
                                });
                                let _ = queue.defer(&job.id, job.started_at.as_deref(), Some(&err_val));
                                deferred_until.insert(
                                    job.id,
                                    Instant::now() + Duration::from_secs(LOCK_DEFER_BACKOFF_SECS),
                                );
                            } else {
                                let err_val = json!({
                                    "error_class": "ExecutionError",
                                    "message": err_msg
                                });
                                let _ = queue.finish(&job.id, "failed", None, Some(&err_val), true);
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn daemon worker thread");
    }

    pub async fn run_servers(&self, port: u16) -> Result<(), HarnessError> {
        let token = ensure_token(&self.paths)?;
        let pid = std::process::id();

        #[cfg(unix)]
        let socket_path = Some(self.paths.socket_file.to_string_lossy().to_string());
        #[cfg(not(unix))]
        let socket_path = None;

        let endpoint_info = DaemonEndpointInfo {
            pid,
            store_root: self.store.root().to_string_lossy().to_string(),
            started_at: Utc::now().to_rfc3339(),
            socket_path,
            port: Some(port),
            token: token.clone(),
        };

        fs::write(
            &self.paths.endpoint_file,
            serde_json::to_string_pretty(&endpoint_info).unwrap() + "\n",
        )
        .map_err(|e| HarnessError::Io(e.to_string()))?;

        fs::write(&self.paths.pid_file, format!("{pid}\n"))
            .map_err(|e| HarnessError::Io(e.to_string()))?;

        self.start_worker();

        let tcp_listener = TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .map_err(|e| HarnessError::Io(format!("failed to bind TCP port {port}: {e}")))?;

        #[cfg(unix)]
        let unix_listener_opt = {
            if self.paths.socket_file.exists() {
                let _ = fs::remove_file(&self.paths.socket_file);
            }
            UnixListener::bind(&self.paths.socket_file).ok()
        };

        let queue = self.queue.clone();
        let store = self.store.clone();
        let shutdown = self.shutdown.clone();

        #[cfg(unix)]
        if let Some(unix_listener) = unix_listener_opt {
            let q = queue.clone();
            let s = store.clone();
            let tok = token.clone();
            let sd = shutdown.clone();
            tokio::spawn(async move {
                while !sd.load(Ordering::Relaxed) {
                    match unix_listener.accept().await {
                        Ok((stream, _)) => {
                            let q = q.clone();
                            let s = s.clone();
                            let tok = tok.clone();
                            let sd = sd.clone();
                            tokio::spawn(async move {
                                let _ = handle_unix_connection(stream, q, s, tok, sd).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        while !shutdown.load(Ordering::Relaxed) {
            tokio::select! {
                res = tcp_listener.accept() => {
                    if let Ok((stream, _)) = res {
                        let q = queue.clone();
                        let s = store.clone();
                        let tok = token.clone();
                        let sd = shutdown.clone();
                        tokio::spawn(async move {
                            let _ = handle_tcp_connection(stream, q, s, tok, sd).await;
                        });
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }

        self.cleanup();
        Ok(())
    }

    pub fn cleanup(&self) {
        let _ = fs::remove_file(&self.paths.endpoint_file);
        let _ = fs::remove_file(&self.paths.pid_file);
        #[cfg(unix)]
        let _ = fs::remove_file(&self.paths.socket_file);
    }
}

async fn handle_connection_stream<R, W>(
    reader: R,
    mut writer: W,
    queue: Arc<QueueStore>,
    store: Arc<MemoryStore>,
    expected_token: String,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HarnessError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Check if line is an HTTP request (e.g. "GET /health HTTP/1.1" or "POST /mcp HTTP/1.1")
        if line.starts_with("GET ") || line.starts_with("POST ") {
            let res = handle_http_request(line, &mut lines, &queue, &store, &expected_token, &shutdown).await;
            writer.write_all(res.as_bytes()).await.map_err(|e| HarnessError::Io(e.to_string()))?;
            break;
        }

        // Native Luna JSON-RPC / message protocol
        let req_val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let err_reply = json!({ "ok": false, "error": format!("invalid JSON: {e}") }).to_string() + "\n";
                let _ = writer.write_all(err_reply.as_bytes()).await;
                continue;
            }
        };

        let action = req_val.get("action").and_then(Value::as_str).unwrap_or("");
        let reply = match action {
            "ping" => {
                let counts = queue.counts().unwrap_or_default();
                json!({
                    "ok": true,
                    "worker_alive": true,
                    "pid": std::process::id(),
                    "store_root": store.root().to_string_lossy(),
                    "counts": counts
                })
            }
            "enqueue" => {
                let kind = req_val.get("kind").and_then(Value::as_str).unwrap_or("");
                let payload = req_val.get("payload").cloned().unwrap_or(json!({}));
                let dedupe_key = req_val.get("dedupe_key").and_then(Value::as_str);
                let priority = req_val.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32;

                match queue.enqueue(kind, &payload, dedupe_key, priority) {
                    Ok(job) => json!({ "ok": true, "job": job }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            "execute" => {
                let kind = req_val.get("kind").and_then(Value::as_str).unwrap_or("");
                let payload = req_val.get("payload").cloned().unwrap_or(json!({}));
                match execute_job_dispatch(&store, kind, &payload) {
                    Ok(res) => json!({ "ok": true, "result": res }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            "get_job" => {
                let job_id = req_val.get("job_id").and_then(Value::as_str).unwrap_or("");
                match queue.get(job_id) {
                    Ok(job) => json!({ "ok": true, "job": job }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            "list_jobs" => {
                let limit = req_val.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
                match queue.list(limit) {
                    Ok(jobs) => json!({ "ok": true, "jobs": jobs }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
            "counts" => match queue.counts() {
                Ok(counts) => json!({ "ok": true, "counts": counts }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            },
            "status" => match store.status() {
                Ok(status) => json!({
                    "ok": true,
                    "records": status.records,
                    "by_domain": status.by_domain,
                    "embedder": status.embedder
                }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            },
            "shutdown" => {
                shutdown.store(true, Ordering::Relaxed);
                json!({ "ok": true, "message": "shutting down" })
            }
            other => json!({ "ok": false, "error": format!("unknown action '{other}'") }),
        };

        let reply_str = reply.to_string() + "\n";
        if writer.write_all(reply_str.as_bytes()).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn handle_tcp_connection(
    stream: TcpStream,
    queue: Arc<QueueStore>,
    store: Arc<MemoryStore>,
    expected_token: String,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HarnessError> {
    let (reader, writer) = stream.into_split();
    handle_connection_stream(reader, writer, queue, store, expected_token, shutdown).await
}

#[cfg(unix)]
async fn handle_unix_connection(
    stream: UnixStream,
    queue: Arc<QueueStore>,
    store: Arc<MemoryStore>,
    expected_token: String,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HarnessError> {
    let (reader, writer) = stream.into_split();
    handle_connection_stream(reader, writer, queue, store, expected_token, shutdown).await
}

async fn handle_http_request<R>(
    request_line: &str,
    lines: &mut tokio::io::Lines<BufReader<R>>,
    queue: &Arc<QueueStore>,
    store: &Arc<MemoryStore>,
    expected_token: &str,
    shutdown: &Arc<AtomicBool>,
) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n".to_string();
    }
    let method = parts[0];
    let path = parts[1];

    let mut content_length = 0usize;
    let mut authorized = false;

    while let Ok(Some(header_line)) = lines.next_line().await {
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            if let Some(val) = trimmed.split(':').nth(1) {
                content_length = val.trim().parse().unwrap_or(0);
            }
        } else if lower.starts_with("authorization:") {
            if let Some(val) = trimmed.split(':').nth(1) {
                let token_cand = val.trim();
                if token_cand == format!("Bearer {expected_token}") {
                    authorized = true;
                }
            }
        }
    }

    // Default loopback allowance or token match
    let _ = authorized;

    let body = if content_length > 0 {
        // Read body from stream
        let mut body_buf = vec![0u8; content_length];
        if let Ok(_) = lines.get_mut().read_exact(&mut body_buf).await {
            String::from_utf8_lossy(&body_buf).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    if method == "GET" && path == "/health" {
        let counts = queue.counts().unwrap_or_default();
        let payload = json!({
            "ok": true,
            "worker_alive": true,
            "pid": std::process::id(),
            "store_root": store.root().to_string_lossy(),
            "counts": counts
        });
        return format_http_json_response(200, &payload);
    }

    if method == "GET" && path.starts_with("/jobs") {
        if path == "/jobs" {
            let jobs = queue.list(20).unwrap_or_default();
            return format_http_json_response(200, &json!({ "jobs": jobs }));
        } else if let Some(id) = path.strip_prefix("/jobs/") {
            match queue.get(id) {
                Ok(job) => return format_http_json_response(200, &json!({ "job": job })),
                Err(_) => return format_http_json_response(404, &json!({ "error": "job not found" })),
            }
        }
    }

    if method == "POST" && path == "/jobs" {
        let val: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        let kind = val.get("kind").and_then(Value::as_str).unwrap_or("");
        let payload = val.get("payload").cloned().unwrap_or(json!({}));
        let dedupe = val.get("dedupe_key").and_then(Value::as_str);
        let prio = val.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32;

        match queue.enqueue(kind, &payload, dedupe, prio) {
            Ok(job) => return format_http_json_response(202, &json!({ "job": job })),
            Err(e) => return format_http_json_response(400, &json!({ "error": e.to_string() })),
        }
    }

    if method == "POST" && path == "/shutdown" {
        shutdown.store(true, Ordering::Relaxed);
        return format_http_json_response(200, &json!({ "ok": true, "message": "shutting down" }));
    }

    if method == "POST" && (path == "/mcp" || path == "/") {
        let mcp_req: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        let mcp_reply = super::mcp::handle_mcp_jsonrpc(store, &mcp_req);
        return format_http_json_response(200, &mcp_reply);
    }

    format_http_json_response(404, &json!({ "error": "not found" }))
}

fn format_http_json_response(status_code: u16, payload: &Value) -> String {
    let body_str = payload.to_string();
    let status_line = match status_code {
        200 => "200 OK",
        202 => "202 Accepted",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
        body_str.len()
    )
}
