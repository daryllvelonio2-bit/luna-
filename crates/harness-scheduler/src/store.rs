//! Persistent SQLite store for scheduled tasks (`~/.luna/schedules.db`).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::model::{
    compute_effective_next_run, is_within_active_window, next_active_window_start,
    validate_active_window, validate_rule, DeliveryReceipt, DeliveryTarget, ExecutionMode,
    ScheduleTask, MAX_DELIVERY_HISTORY,
};

#[derive(Debug, Clone, Default)]
pub struct ScheduleCreateOptions {
    pub id: Option<String>,
    pub name: String,
    pub instruction: String,
    pub rule: String,
    pub delivery_target: DeliveryTarget,
    pub plugin_id: Option<String>,
    pub require_approval: bool,
    pub profile: Option<String>,
    pub execution_mode: ExecutionMode,
    pub active_window: Option<String>,
    pub trigger_command: Option<String>,
    pub trigger_logic: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScheduleUpdateOptions {
    pub name: Option<String>,
    pub instruction: Option<String>,
    pub rule: Option<String>,
    pub delivery_target: Option<DeliveryTarget>,
    pub paused: Option<bool>,
    pub require_approval: Option<bool>,
    pub profile: Option<Option<String>>,
    pub execution_mode: Option<ExecutionMode>,
    pub active_window: Option<Option<String>>,
    pub trigger_command: Option<Option<String>>,
    pub trigger_logic: Option<Option<String>>,
}

pub struct ScheduleStore {
    conn: Mutex<Connection>,
}

impl ScheduleStore {
    pub fn open(path: Option<&Path>) -> Result<Self, String> {
        let conn = match path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                Connection::open(p).map_err(|e| e.to_string())?
            }
            None => Connection::open_in_memory().map_err(|e| e.to_string())?,
        };
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                instruction TEXT NOT NULL,
                rule TEXT NOT NULL,
                delivery_target TEXT NOT NULL,
                plugin_id TEXT,
                require_approval INTEGER NOT NULL DEFAULT 0,
                profile TEXT,
                execution_mode TEXT NOT NULL DEFAULT 'scheduled',
                active_window TEXT,
                trigger_command TEXT,
                trigger_logic TEXT,
                paused INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                next_run_at TEXT,
                last_run_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                history_json TEXT NOT NULL DEFAULT '[]'
            )",
            [],
        )
        .map_err(|e| e.to_string())?;
        ensure_extra_columns(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create(
        &self,
        id: Option<String>,
        name: String,
        instruction: String,
        rule: String,
        delivery_target: DeliveryTarget,
        plugin_id: Option<String>,
    ) -> Result<ScheduleTask, String> {
        self.create_with_approval(
            id,
            name,
            instruction,
            rule,
            delivery_target,
            plugin_id,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_with_approval(
        &self,
        id: Option<String>,
        name: String,
        instruction: String,
        rule: String,
        delivery_target: DeliveryTarget,
        plugin_id: Option<String>,
        require_approval: bool,
    ) -> Result<ScheduleTask, String> {
        self.create_with_options(ScheduleCreateOptions {
            id,
            name,
            instruction,
            rule,
            delivery_target,
            plugin_id,
            require_approval,
            ..Default::default()
        })
    }

    pub fn create_with_options(&self, opts: ScheduleCreateOptions) -> Result<ScheduleTask, String> {
        if opts.name.trim().is_empty() {
            return Err("schedule name cannot be empty".to_string());
        }
        if opts.instruction.trim().is_empty() {
            return Err("schedule instruction cannot be empty".to_string());
        }

        let execution_mode = if opts.rule.trim().eq_ignore_ascii_case("continuous")
            || opts.rule.trim().starts_with("continuous:")
        {
            ExecutionMode::Continuous
        } else {
            opts.execution_mode
        };

        let rule = if opts.rule.trim().is_empty() && execution_mode == ExecutionMode::Continuous {
            "continuous".to_string()
        } else {
            opts.rule.trim().to_string()
        };
        validate_rule(&rule)?;

        let active_window = normalize_opt_string(opts.active_window);
        if let Some(ref win) = active_window {
            validate_active_window(win)?;
        }

        let now = Utc::now();
        let next_run_at = if execution_mode == ExecutionMode::Continuous
            && (rule.eq_ignore_ascii_case("continuous") || rule.starts_with("continuous:"))
        {
            match active_window.as_deref() {
                Some(win) if !is_within_active_window(win, now)? => {
                    Some(next_active_window_start(win, now)?)
                }
                _ => Some(now),
            }
        } else {
            compute_effective_next_run(&rule, active_window.as_deref(), now)?
        };

        let task = ScheduleTask {
            id: opts.id.unwrap_or_else(|| {
                format!("sch_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
            }),
            name: opts.name.trim().to_string(),
            instruction: opts.instruction.trim().to_string(),
            rule,
            delivery_target: opts.delivery_target,
            plugin_id: opts.plugin_id,
            require_approval: opts.require_approval,
            profile: normalize_opt_string(opts.profile),
            execution_mode,
            active_window,
            trigger_command: normalize_opt_string(opts.trigger_command),
            trigger_logic: normalize_opt_string(opts.trigger_logic),
            paused: false,
            completed: next_run_at.is_none(),
            next_run_at,
            last_run_at: None,
            created_at: now,
            updated_at: now,
            history: Vec::new(),
        };
        self.upsert(&task)?;
        Ok(task)
    }

    pub fn upsert(&self, task: &ScheduleTask) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let target_json =
            serde_json::to_string(&task.delivery_target).map_err(|e| e.to_string())?;
        let history_json = serde_json::to_string(&task.history).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO schedules (
                id, name, instruction, rule, delivery_target, plugin_id, require_approval,
                profile, execution_mode, active_window, trigger_command, trigger_logic,
                paused, completed, next_run_at, last_run_at, created_at, updated_at, history_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                instruction = excluded.instruction,
                rule = excluded.rule,
                delivery_target = excluded.delivery_target,
                plugin_id = excluded.plugin_id,
                require_approval = excluded.require_approval,
                profile = excluded.profile,
                execution_mode = excluded.execution_mode,
                active_window = excluded.active_window,
                trigger_command = excluded.trigger_command,
                trigger_logic = excluded.trigger_logic,
                paused = excluded.paused,
                completed = excluded.completed,
                next_run_at = excluded.next_run_at,
                last_run_at = excluded.last_run_at,
                updated_at = excluded.updated_at,
                history_json = excluded.history_json",
            params![
                task.id,
                task.name,
                task.instruction,
                task.rule,
                target_json,
                task.plugin_id,
                i32::from(task.require_approval),
                task.profile,
                task.execution_mode.as_str(),
                task.active_window,
                task.trigger_command,
                task.trigger_logic,
                i32::from(task.paused),
                i32::from(task.completed),
                task.next_run_at.map(|d| d.to_rfc3339()),
                task.last_run_at.map(|d| d.to_rfc3339()),
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
                history_json,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<ScheduleTask>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, instruction, rule, delivery_target, plugin_id, require_approval,
                        profile, execution_mode, active_window, trigger_command, trigger_logic,
                        paused, completed, next_run_at, last_run_at, created_at, updated_at, history_json
                 FROM schedules ORDER BY created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], row_to_task).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn get(&self, id: &str) -> Result<ScheduleTask, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT id, name, instruction, rule, delivery_target, plugin_id, require_approval,
                    profile, execution_mode, active_window, trigger_command, trigger_logic,
                    paused, completed, next_run_at, last_run_at, created_at, updated_at, history_json
             FROM schedules WHERE id = ?1",
            params![id],
            row_to_task,
        )
        .map_err(|e| format!("schedule '{id}' not found: {e}"))
    }

    pub fn update(
        &self,
        id: &str,
        name: Option<String>,
        instruction: Option<String>,
        rule: Option<String>,
        delivery_target: Option<DeliveryTarget>,
        paused: Option<bool>,
    ) -> Result<ScheduleTask, String> {
        self.update_with_approval(id, name, instruction, rule, delivery_target, paused, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_with_approval(
        &self,
        id: &str,
        name: Option<String>,
        instruction: Option<String>,
        rule: Option<String>,
        delivery_target: Option<DeliveryTarget>,
        paused: Option<bool>,
        require_approval: Option<bool>,
    ) -> Result<ScheduleTask, String> {
        self.update_with_options(
            id,
            ScheduleUpdateOptions {
                name,
                instruction,
                rule,
                delivery_target,
                paused,
                require_approval,
                ..Default::default()
            },
        )
    }

    pub fn update_with_options(
        &self,
        id: &str,
        opts: ScheduleUpdateOptions,
    ) -> Result<ScheduleTask, String> {
        let mut task = self.get(id)?;
        let now = Utc::now();
        let mut recompute_next = false;

        if let Some(n) = opts.name {
            if !n.trim().is_empty() {
                task.name = n.trim().to_string();
            }
        }
        if let Some(i) = opts.instruction {
            if !i.trim().is_empty() {
                task.instruction = i.trim().to_string();
            }
        }
        if let Some(prof) = opts.profile {
            task.profile = normalize_opt_string(prof);
        }
        if let Some(mode) = opts.execution_mode {
            task.execution_mode = mode;
            recompute_next = true;
        }
        if let Some(win_opt) = opts.active_window {
            let normalized = normalize_opt_string(win_opt);
            if let Some(ref win) = normalized {
                validate_active_window(win)?;
            }
            task.active_window = normalized;
            recompute_next = true;
        }
        if let Some(cmd_opt) = opts.trigger_command {
            task.trigger_command = normalize_opt_string(cmd_opt);
        }
        if let Some(logic_opt) = opts.trigger_logic {
            task.trigger_logic = normalize_opt_string(logic_opt);
        }
        if let Some(r) = opts.rule {
            let trimmed = r.trim().to_string();
            validate_rule(&trimmed)?;
            if trimmed.eq_ignore_ascii_case("continuous") || trimmed.starts_with("continuous:") {
                task.execution_mode = ExecutionMode::Continuous;
            }
            task.rule = trimmed;
            recompute_next = true;
        }
        if let Some(t) = opts.delivery_target {
            task.delivery_target = t;
        }
        if let Some(p) = opts.paused {
            task.paused = p;
            if !p && !task.completed && task.next_run_at.is_none() {
                recompute_next = true;
            }
        }
        if let Some(req_app) = opts.require_approval {
            task.require_approval = req_app;
        }

        if recompute_next {
            task.next_run_at =
                compute_effective_next_run(&task.rule, task.active_window.as_deref(), now)?;
            task.completed = task.next_run_at.is_none();
        }

        task.updated_at = now;
        self.upsert(&task)?;
        Ok(task)
    }

    pub fn set_next_run_at(
        &self,
        id: &str,
        next_run_at: Option<DateTime<Utc>>,
    ) -> Result<ScheduleTask, String> {
        let mut task = self.get(id)?;
        task.next_run_at = next_run_at;
        task.completed = next_run_at.is_none();
        task.updated_at = Utc::now();
        self.upsert(&task)?;
        Ok(task)
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let rows = conn
            .execute("DELETE FROM schedules WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    /// Return tasks currently due (not paused, not completed, `next_run_at <= now`).
    pub fn due_tasks(&self, now: DateTime<Utc>) -> Result<Vec<ScheduleTask>, String> {
        let all = self.list()?;
        Ok(all
            .into_iter()
            .filter(|t| !t.paused && !t.completed && t.next_run_at.is_some_and(|n| n <= now))
            .collect())
    }

    /// Record a delivery receipt and advance `next_run_at` (or mark one-shot completed).
    pub fn record_delivery(
        &self,
        id: &str,
        receipt: DeliveryReceipt,
    ) -> Result<ScheduleTask, String> {
        let mut task = self.get(id)?;
        let now = receipt.delivered_at;
        if let DeliveryTarget::SingleNewSession { session_id: None } = &task.delivery_target {
            if !receipt.session_id.is_empty() {
                task.delivery_target = DeliveryTarget::SingleNewSession {
                    session_id: Some(receipt.session_id.clone()),
                };
            }
        }
        task.last_run_at = Some(now);
        task.history.push(receipt);
        if task.history.len() > MAX_DELIVERY_HISTORY {
            let drain = task.history.len() - MAX_DELIVERY_HISTORY;
            task.history.drain(0..drain);
        }
        if task.rule.trim().starts_with("once:") && task.execution_mode != ExecutionMode::Continuous
        {
            task.next_run_at = None;
            task.completed = true;
        } else {
            let next = compute_effective_next_run(&task.rule, task.active_window.as_deref(), now)?;
            task.next_run_at = match next {
                Some(dt) => Some(dt),
                None if task.execution_mode == ExecutionMode::Continuous => {
                    compute_effective_next_run("continuous", task.active_window.as_deref(), now)?
                }
                None => None,
            };
            task.completed = task.next_run_at.is_none();
        }
        task.updated_at = now;
        self.upsert(&task)?;
        Ok(task)
    }
}

fn normalize_opt_string(val: Option<String>) -> Option<String> {
    val.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn ensure_extra_columns(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(schedules)")
        .map_err(|e| e.to_string())?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();

    let additions = [
        ("require_approval", "INTEGER NOT NULL DEFAULT 0"),
        ("profile", "TEXT"),
        ("execution_mode", "TEXT NOT NULL DEFAULT 'scheduled'"),
        ("active_window", "TEXT"),
        ("trigger_command", "TEXT"),
        ("trigger_logic", "TEXT"),
    ];
    for (col, ddl) in additions {
        if !cols.iter().any(|c| c == col) {
            conn.execute(&format!("ALTER TABLE schedules ADD COLUMN {col} {ddl}"), [])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleTask> {
    let target_raw: String = row.get(4)?;
    let target: DeliveryTarget = serde_json::from_str(&target_raw).unwrap_or_default();
    let req_app_int: i32 = row.get(6)?;
    let profile: Option<String> = row.get(7)?;
    let mode_raw: String = row.get(8)?;
    let execution_mode = ExecutionMode::from_str_lossy(&mode_raw).unwrap_or_default();
    let active_window: Option<String> = row.get(9)?;
    let trigger_command: Option<String> = row.get(10)?;
    let trigger_logic: Option<String> = row.get(11)?;
    let paused_int: i32 = row.get(12)?;
    let completed_int: i32 = row.get(13)?;
    let next_raw: Option<String> = row.get(14)?;
    let last_raw: Option<String> = row.get(15)?;
    let created_raw: String = row.get(16)?;
    let updated_raw: String = row.get(17)?;
    let history_raw: String = row.get(18)?;

    Ok(ScheduleTask {
        id: row.get(0)?,
        name: row.get(1)?,
        instruction: row.get(2)?,
        rule: row.get(3)?,
        delivery_target: target,
        plugin_id: row.get(5)?,
        require_approval: req_app_int != 0,
        profile,
        execution_mode,
        active_window,
        trigger_command,
        trigger_logic,
        paused: paused_int != 0,
        completed: completed_int != 0,
        next_run_at: next_raw.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        }),
        last_run_at: last_raw.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        }),
        created_at: DateTime::parse_from_rfc3339(&created_raw)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: DateTime::parse_from_rfc3339(&updated_raw)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        history: serde_json::from_str(&history_raw).unwrap_or_default(),
    })
}
