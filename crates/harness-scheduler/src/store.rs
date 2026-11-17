//! Persistent SQLite store for scheduled tasks (`~/.luna/schedules.db`).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::model::{
    compute_next_run, validate_rule, DeliveryReceipt, DeliveryTarget, ScheduleTask,
    MAX_DELIVERY_HISTORY,
};

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
        if name.trim().is_empty() {
            return Err("schedule name cannot be empty".to_string());
        }
        if instruction.trim().is_empty() {
            return Err("schedule instruction cannot be empty".to_string());
        }
        validate_rule(&rule)?;
        let now = Utc::now();
        let next_run_at = compute_next_run(&rule, now)?;
        let task = ScheduleTask {
            id: id.unwrap_or_else(|| format!("sch_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])),
            name: name.trim().to_string(),
            instruction: instruction.trim().to_string(),
            rule,
            delivery_target,
            plugin_id,
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
        let target_json = serde_json::to_string(&task.delivery_target).map_err(|e| e.to_string())?;
        let history_json = serde_json::to_string(&task.history).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO schedules (
                id, name, instruction, rule, delivery_target, plugin_id, paused, completed,
                next_run_at, last_run_at, created_at, updated_at, history_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                instruction = excluded.instruction,
                rule = excluded.rule,
                delivery_target = excluded.delivery_target,
                plugin_id = excluded.plugin_id,
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
                "SELECT id, name, instruction, rule, delivery_target, plugin_id, paused, completed,
                        next_run_at, last_run_at, created_at, updated_at, history_json
                 FROM schedules ORDER BY created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], row_to_task)
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn get(&self, id: &str) -> Result<ScheduleTask, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT id, name, instruction, rule, delivery_target, plugin_id, paused, completed,
                    next_run_at, last_run_at, created_at, updated_at, history_json
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
        let mut task = self.get(id)?;
        let now = Utc::now();
        if let Some(n) = name {
            if !n.trim().is_empty() {
                task.name = n.trim().to_string();
            }
        }
        if let Some(i) = instruction {
            if !i.trim().is_empty() {
                task.instruction = i.trim().to_string();
            }
        }
        if let Some(r) = rule {
            validate_rule(&r)?;
            task.rule = r;
            task.next_run_at = compute_next_run(&task.rule, now)?;
            task.completed = task.next_run_at.is_none();
        }
        if let Some(t) = delivery_target {
            task.delivery_target = t;
        }
        if let Some(p) = paused {
            task.paused = p;
            if !p && !task.completed && task.next_run_at.is_none() {
                task.next_run_at = compute_next_run(&task.rule, now)?;
            }
        }
        task.updated_at = now;
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
    pub fn record_delivery(&self, id: &str, receipt: DeliveryReceipt) -> Result<ScheduleTask, String> {
        let mut task = self.get(id)?;
        let now = receipt.delivered_at;
        if let DeliveryTarget::SingleNewSession { session_id: None } = &task.delivery_target {
            task.delivery_target = DeliveryTarget::SingleNewSession {
                session_id: Some(receipt.session_id.clone()),
            };
        }
        task.last_run_at = Some(now);
        task.history.push(receipt);
        if task.history.len() > MAX_DELIVERY_HISTORY {
            let drain = task.history.len() - MAX_DELIVERY_HISTORY;
            task.history.drain(0..drain);
        }
        if task.rule.trim().starts_with("once:") {
            task.next_run_at = None;
            task.completed = true;
        } else {
            task.next_run_at = compute_next_run(&task.rule, now)?;
            task.completed = task.next_run_at.is_none();
        }
        task.updated_at = now;
        self.upsert(&task)?;
        Ok(task)
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleTask> {
    let target_raw: String = row.get(4)?;
    let target: DeliveryTarget = serde_json::from_str(&target_raw).unwrap_or_default();
    let paused_int: i32 = row.get(6)?;
    let completed_int: i32 = row.get(7)?;
    let next_raw: Option<String> = row.get(8)?;
    let last_raw: Option<String> = row.get(9)?;
    let created_raw: String = row.get(10)?;
    let updated_raw: String = row.get(11)?;
    let history_raw: String = row.get(12)?;

    Ok(ScheduleTask {
        id: row.get(0)?,
        name: row.get(1)?,
        instruction: row.get(2)?,
        rule: row.get(3)?,
        delivery_target: target,
        plugin_id: row.get(5)?,
        paused: paused_int != 0,
        completed: completed_int != 0,
        next_run_at: next_raw.and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|d| d.with_timezone(&Utc))),
        last_run_at: last_raw.and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|d| d.with_timezone(&Utc))),
        created_at: DateTime::parse_from_rfc3339(&created_raw)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        updated_at: DateTime::parse_from_rfc3339(&updated_raw)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        history: serde_json::from_str(&history_raw).unwrap_or_default(),
    })
}
