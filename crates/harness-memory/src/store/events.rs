//! Coordination event log and byte artifacts.
//!
//! Direct implementation of MEMORY_STORE.md §4.8:
//! - Multi-agent coordination event stream (`events/<stream>/<YYYY-MM-DD>.md`)
//! - Agent acknowledgments (`events/<stream>/acks/<agent>.md`)
//! - Polling wait for events (`event_wait`)
//! - Byte-exact artifacts (`artifacts/<id>/payload` + `artifacts/<id>/meta.md`)

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::frontmatter;
use super::ids;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: String,
    pub stream: String,
    pub event_type: String,
    pub from: String,
    pub to: Option<String>,
    pub at: DateTime<Utc>,
    pub status: String,
    pub correlation_id: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactMeta {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
    pub extra: HashMap<String, String>,
}

pub struct EventsStore {
    root: PathBuf,
}

impl EventsStore {
    pub fn new(store_root: PathBuf) -> Self {
        Self { root: store_root }
    }

    pub fn events_root(&self) -> PathBuf {
        self.root.join("events")
    }

    pub fn artifacts_root(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    fn stream_dir(&self, stream: &str) -> PathBuf {
        let slug = ids::slugify(stream);
        let safe_slug = if slug.is_empty() { "main" } else { &slug };
        self.events_root().join(safe_slug)
    }

    /// Append a coordination event to the event stream day file.
    pub fn append_event(
        &self,
        stream: &str,
        event_type: &str,
        from: &str,
        to: Option<&str>,
        body: &str,
        correlation_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Event, HarnessError> {
        let slug = ids::slugify(stream);
        let stream_slug = if slug.is_empty() {
            "main".to_string()
        } else {
            slug
        };

        let now = Utc::now();
        let date_str = now.format("%Y-%m-%d").to_string();
        let dir = self.stream_dir(&stream_slug);
        fs::create_dir_all(&dir)
            .map_err(|e| HarnessError::MemoryError(format!("Failed to create stream dir: {e}")))?;

        let id = format!(
            "evt_{:012x}_{}",
            now.timestamp_millis(),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );

        let event = Event {
            id: id.clone(),
            stream: stream_slug.clone(),
            event_type: event_type.to_string(),
            from: from.to_string(),
            to: to.map(|s| s.to_string()),
            at: now,
            status: status.unwrap_or("open").to_string(),
            correlation_id: correlation_id.map(|s| s.to_string()),
            body: body.trim().to_string(),
        };

        let rendered = render_event(&event);
        let day_file = dir.join(format!("{date_str}.md"));

        if day_file.exists() {
            let mut existing = fs::read_to_string(&day_file).map_err(|e| {
                HarnessError::MemoryError(format!("Failed to read event file: {e}"))
            })?;
            if !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(&format!("\n{rendered}\n"));
            atomic::write_str(&day_file, &existing)
                .map_err(|e| HarnessError::MemoryError(format!("Failed to append event: {e}")))?;
        } else {
            atomic::write_str(&day_file, &format!("{rendered}\n"))
                .map_err(|e| HarnessError::MemoryError(format!("Failed to write event: {e}")))?;
        }

        Ok(event)
    }

    /// List events from a stream, filtered by since, unacked_by, and limited.
    pub fn list_events(
        &self,
        stream: &str,
        since: Option<DateTime<Utc>>,
        limit: Option<usize>,
        unacked_by: Option<&str>,
    ) -> Result<Vec<Event>, HarnessError> {
        let stream_dir = self.stream_dir(stream);
        if !stream_dir.exists() {
            return Ok(Vec::new());
        }

        let acked_set: HashSet<String> = if let Some(agent) = unacked_by {
            let acks_file = stream_dir
                .join("acks")
                .join(format!("{}.md", ids::slugify(agent)));
            if acks_file.exists() {
                fs::read_to_string(&acks_file)
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

        let mut day_files = Vec::new();
        if let Ok(entries) = fs::read_dir(&stream_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().and_then(|s| s.to_str()).unwrap_or("") == "md" {
                    day_files.push(p);
                }
            }
        }

        // Sort chronologically (oldest to newest day)
        day_files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        let mut events = Vec::new();
        for file in day_files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let parsed = parse_events_file(stream, &text);
            for ev in parsed {
                if let Some(since_ts) = since {
                    if ev.at < since_ts {
                        continue;
                    }
                }
                if unacked_by.is_some() && acked_set.contains(&ev.id) {
                    continue;
                }
                events.push(ev);
            }
        }

        if let Some(lim) = limit {
            if events.len() > lim {
                events.truncate(lim);
            }
        }

        Ok(events)
    }

    /// Mark an event as acknowledged by an agent.
    pub fn ack_event(&self, stream: &str, agent: &str, event_id: &str) -> Result<(), HarnessError> {
        let stream_dir = self.stream_dir(stream);
        let acks_dir = stream_dir.join("acks");
        fs::create_dir_all(&acks_dir).map_err(|e| {
            HarnessError::MemoryError(format!("Failed to create acks directory: {e}"))
        })?;

        let agent_slug = ids::slugify(agent);
        let acks_file = acks_dir.join(format!("{agent_slug}.md"));

        let mut existing_acks = HashSet::new();
        if acks_file.exists() {
            if let Ok(txt) = fs::read_to_string(&acks_file) {
                for line in txt.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        existing_acks.insert(trimmed.to_string());
                    }
                }
            }
        }

        if !existing_acks.contains(event_id) {
            let mut content = if acks_file.exists() {
                fs::read_to_string(&acks_file).unwrap_or_default()
            } else {
                String::new()
            };
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(event_id);
            content.push('\n');
            atomic::write_str(&acks_file, &content)
                .map_err(|e| HarnessError::MemoryError(format!("Failed to write ack: {e}")))?;
        }

        Ok(())
    }

    fn matching_event(
        &self,
        stream: &str,
        agent_lower: &str,
    ) -> Result<Option<Event>, HarnessError> {
        let events = self.list_events(stream, None, Some(20), Some(agent_lower))?;
        for ev in events {
            match &ev.to {
                Some(target) if target.to_lowercase() == agent_lower => return Ok(Some(ev)),
                None => return Ok(Some(ev)), // broadcast event
                _ => {}
            }
        }
        Ok(None)
    }

    /// Wait until an unacknowledged event arrives for `agent` or timeout expires.
    pub async fn wait_for_event(
        &self,
        stream: &str,
        agent: &str,
        timeout_ms: u64,
    ) -> Result<Option<Event>, HarnessError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let agent_lower = agent.to_lowercase();

        loop {
            if let Some(ev) = self.matching_event(stream, &agent_lower)? {
                return Ok(Some(ev));
            }
            if start.elapsed() >= timeout {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        Ok(None)
    }

    /// Synchronous wait for callers without a Tokio runtime (the stdio MCP
    /// server). Blocks the calling thread, so async callers should use
    /// [`Self::wait_for_event`].
    pub fn wait_for_event_blocking(
        &self,
        stream: &str,
        agent: &str,
        timeout_ms: u64,
    ) -> Result<Option<Event>, HarnessError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let agent_lower = agent.to_lowercase();

        loop {
            if let Some(ev) = self.matching_event(stream, &agent_lower)? {
                return Ok(Some(ev));
            }
            if start.elapsed() >= timeout {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(None)
    }

    /// Save a byte-exact artifact with metadata.
    pub fn put_artifact(
        &self,
        id_opt: Option<&str>,
        payload: &[u8],
        extra_meta: HashMap<String, String>,
    ) -> Result<String, HarnessError> {
        let id = match id_opt {
            Some(i) => ids::slugify(i),
            None => format!(
                "art_{:012x}_{}",
                Utc::now().timestamp_millis(),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ),
        };

        let dir = self.artifacts_root().join(&id);
        fs::create_dir_all(&dir).map_err(|e| {
            HarnessError::MemoryError(format!("Failed to create artifact dir: {e}"))
        })?;

        let payload_file = dir.join("payload");
        fs::write(&payload_file, payload).map_err(|e| {
            HarnessError::MemoryError(format!("Failed to write artifact payload: {e}"))
        })?;

        let now = Utc::now();
        let mut yaml_lines = Vec::new();
        yaml_lines.push(format!("id: {}", id));
        yaml_lines.push(format!("created_at: {}", now.to_rfc3339()));
        yaml_lines.push(format!("size_bytes: {}", payload.len()));
        for (k, v) in &extra_meta {
            yaml_lines.push(format!(
                "{}: {}",
                k,
                serde_yaml::to_string(v).unwrap_or_default().trim()
            ));
        }
        let yaml = yaml_lines.join("\n");
        let doc_body = format!("# Artifact: {}\n", id);
        let meta_content = frontmatter::render(&yaml, &doc_body);

        let meta_file = dir.join("meta.md");
        atomic::write_str(&meta_file, &meta_content).map_err(|e| {
            HarnessError::MemoryError(format!("Failed to write artifact meta: {e}"))
        })?;

        Ok(id)
    }

    /// Retrieve an artifact's bytes and metadata by id.
    pub fn get_artifact(&self, id: &str) -> Result<Option<(Vec<u8>, ArtifactMeta)>, HarnessError> {
        let dir = self.artifacts_root().join(ids::slugify(id));
        if !dir.exists() {
            return Ok(None);
        }

        let payload_file = dir.join("payload");
        let meta_file = dir.join("meta.md");
        if !payload_file.exists() || !meta_file.exists() {
            return Ok(None);
        }

        let payload = fs::read(&payload_file).map_err(|e| {
            HarnessError::MemoryError(format!("Failed to read artifact payload: {e}"))
        })?;

        let meta_str = fs::read_to_string(&meta_file)
            .map_err(|e| HarnessError::MemoryError(format!("Failed to read artifact meta: {e}")))?;

        let (yaml_part, _) = frontmatter::split(&meta_str).unwrap_or(("", &meta_str));
        let val = frontmatter::parse(yaml_part).unwrap_or(serde_yaml::Value::Null);

        let created_at = frontmatter::string(&val, "created_at")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let size_bytes =
            frontmatter::u64(&val, "size_bytes").unwrap_or(payload.len() as u64) as usize;

        let mut extra = HashMap::new();
        if let serde_yaml::Value::Mapping(map) = val {
            for (k, v) in map {
                if let (Some(k_str), Some(v_str)) = (k.as_str(), v.as_str()) {
                    if !["id", "created_at", "size_bytes"].contains(&k_str) {
                        extra.insert(k_str.to_string(), v_str.to_string());
                    }
                }
            }
        }

        let meta = ArtifactMeta {
            id: id.to_string(),
            created_at,
            size_bytes,
            extra,
        };

        Ok(Some((payload, meta)))
    }
}

fn render_event(ev: &Event) -> String {
    let to_str = ev.to.as_deref().unwrap_or("null");
    let corr_str = ev.correlation_id.as_deref().unwrap_or("null");

    format!(
        "## {}\n```yaml\ntype: {}\nfrom: {}\nto: {}\nat: {}\nstatus: {}\ncorrelation_id: {}\n```\n{}\n",
        ev.id,
        ev.event_type,
        ev.from,
        to_str,
        ev.at.to_rfc3339(),
        ev.status,
        corr_str,
        ev.body
    )
}

fn parse_events_file(stream: &str, text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("## ") {
            let id = line[3..].trim().to_string();
            i += 1;

            // Skip empty lines to find ```yaml
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }

            let mut yaml_lines = Vec::new();
            if i < lines.len() && (lines[i].trim() == "```yaml" || lines[i].trim() == "```yml") {
                i += 1;
                while i < lines.len() && lines[i].trim() != "```" {
                    yaml_lines.push(lines[i]);
                    i += 1;
                }
                if i < lines.len() && lines[i].trim() == "```" {
                    i += 1;
                }
            }

            let mut body_lines = Vec::new();
            while i < lines.len() && !lines[i].trim().starts_with("## ") {
                body_lines.push(lines[i]);
                i += 1;
            }

            let yaml_str = yaml_lines.join("\n");
            let body = body_lines.join("\n").trim().to_string();

            if let Ok(val) = frontmatter::parse(&yaml_str) {
                let event_type =
                    frontmatter::string(&val, "type").unwrap_or_else(|| "event".to_string());
                let from =
                    frontmatter::string(&val, "from").unwrap_or_else(|| "unknown".to_string());
                let to = frontmatter::opt_string(&val, "to").filter(|s| s != "null");
                let status =
                    frontmatter::string(&val, "status").unwrap_or_else(|| "open".to_string());
                let correlation_id =
                    frontmatter::opt_string(&val, "correlation_id").filter(|s| s != "null");
                let at = frontmatter::string(&val, "at")
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                events.push(Event {
                    id,
                    stream: stream.to_string(),
                    event_type,
                    from,
                    to,
                    at,
                    status,
                    correlation_id,
                    body,
                });
            }
        } else {
            i += 1;
        }
    }

    events
}
