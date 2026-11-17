//! Persistent per-plugin SQLite KV storage and JSON settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Persistent SQLite KV store scoped to one plugin (`~/.luna/plugins/<id>/data.db`).
pub struct PluginStorage {
    conn: Mutex<Connection>,
}

impl PluginStorage {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn get(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT value FROM kv WHERE key = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query(params![key]).map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let raw: String = row.get(0).map_err(|e| e.to_string())?;
            let val = serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw));
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    pub fn set(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let raw = serde_json::to_string(value).map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, raw, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete(&self, key: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let n = conn
            .execute("DELETE FROM kv WHERE key = ?1", params![key])
            .map_err(|e| e.to_string())?;
        Ok(n > 0)
    }

    pub fn list(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT key FROM kv ORDER BY key ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut keys = Vec::new();
        for r in rows {
            keys.push(r.map_err(|e| e.to_string())?);
        }
        Ok(keys)
    }
}

pub fn load_settings(plugin_dir: &Path) -> BTreeMap<String, serde_json::Value> {
    let path = settings_path(plugin_dir);
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_settings(
    plugin_dir: &Path,
    settings: &BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    let path = settings_path(plugin_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

fn settings_path(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("settings.json")
}
