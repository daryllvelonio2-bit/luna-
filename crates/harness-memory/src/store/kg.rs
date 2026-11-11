//! Knowledge Graph: temporal entity-relationship graph with canonical Markdown persistence.
//!
//! Stores canonical files per entity (`graph/entities/<entity_id>.md`) and
//! per subject (`graph/triples/<subject_id>.md`).
//!
//! Provides point-in-time temporal querying with valid-time bounds, atomic supersede,
//! deduplication, entity candidate matching, and chronological timelines.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::frontmatter;
use super::ids::{canonical_entity_name, entity_id_for_name, make_triple_id, safe_id};

pub const MIN_ENTITY_CANDIDATE_LEN: usize = 3;

/// Format an error into a HarnessError::MemoryError.
fn kg_err(msg: impl Into<String>) -> HarnessError {
    HarnessError::MemoryError(msg.into())
}

/// Check if a temporal string is in date-only format (YYYY-MM-DD).
pub fn is_date_only_temporal(val: &str) -> bool {
    let s = val.trim();
    if s.len() != 10 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
}

/// Comparable instant for a valid_from / as_of value.
/// Date-only values expand to T00:00:00Z.
pub fn temporal_start_key(value: Option<&str>) -> Option<String> {
    let val = value?.trim();
    if val.is_empty() {
        return None;
    }
    if is_date_only_temporal(val) {
        Some(format!("{val}T00:00:00Z"))
    } else {
        Some(val.to_string())
    }
}

/// Comparable instant for a valid_to value.
/// Date-only values expand to T23:59:59Z for backwards-compatible full-day coverage.
pub fn temporal_end_key(value: Option<&str>) -> Option<String> {
    let val = value?.trim();
    if val.is_empty() {
        return None;
    }
    if is_date_only_temporal(val) {
        Some(format!("{val}T23:59:59Z"))
    } else {
        Some(val.to_string())
    }
}

/// Validate that a temporal interval is not inverted.
/// Inverted intervals (valid_to < valid_from) are rejected at write time.
pub fn validate_temporal_interval(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<(), HarnessError> {
    if let (Some(from), Some(to)) = (valid_from, valid_to) {
        if let (Some(sk), Some(ek)) = (temporal_start_key(Some(from)), temporal_end_key(Some(to))) {
            if ek < sk {
                return Err(kg_err(format!(
                    "valid_to={to:?} is before valid_from={from:?}; an inverted interval would be invisible to every KG query"
                )));
            }
        }
    }
    Ok(())
}

/// One entity attribute with temporal bounds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityAttribute {
    pub key: String,
    pub value: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Canonical entity node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub aliases: Vec<String>,
    pub properties: BTreeMap<String, String>,
    pub attributes: Vec<EntityAttribute>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Entity {
    pub fn new(name: &str, entity_type: Option<&str>) -> Self {
        let now = Utc::now();
        Self {
            id: entity_id_for_name(name),
            name: name.trim().to_string(),
            entity_type: entity_type.unwrap_or("unknown").to_string(),
            aliases: Vec::new(),
            properties: BTreeMap::new(),
            attributes: Vec::new(),
            notes: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn to_document(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("entity: {}", self.name));
        lines.push(format!("type: {}", self.entity_type));
        if !self.aliases.is_empty() {
            let aliases_json = serde_json::to_string(&self.aliases).unwrap_or_default();
            lines.push(format!("aliases: {aliases_json}"));
        }
        lines.push(format!("created_at: {}", self.created_at.to_rfc3339()));
        lines.push(format!("updated_at: {}", self.updated_at.to_rfc3339()));
        let yaml = lines.join("\n");

        let mut body = String::new();
        body.push_str("## Attributes\n");
        for attr in &self.attributes {
            body.push_str(&format!(
                "- key: {} | value: {} | from: {} | to: {}\n",
                escape_pipe(&attr.key),
                escape_pipe(&attr.value),
                attr.from.as_deref().unwrap_or("null"),
                attr.to.as_deref().unwrap_or("null")
            ));
        }
        if !self.notes.trim().is_empty() {
            body.push_str("## Notes\n");
            body.push_str(self.notes.trim());
            body.push('\n');
        }

        frontmatter::render(&yaml, &body)
    }

    pub fn from_document(id: &str, text: &str) -> Result<Self, HarnessError> {
        let (yaml, body) = frontmatter::split(text)
            .ok_or_else(|| kg_err("missing front matter in entity file"))?;
        let val = frontmatter::parse(yaml).map_err(|e| kg_err(e.to_string()))?;

        let name = frontmatter::string(&val, "entity").unwrap_or_else(|| id.to_string());
        let entity_type =
            frontmatter::string(&val, "type").unwrap_or_else(|| "unknown".to_string());
        let aliases = frontmatter::list(&val, "aliases");
        let created_at = frontmatter::string(&val, "created_at")
            .and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .unwrap_or_else(Utc::now);
        let updated_at = frontmatter::string(&val, "updated_at")
            .and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .unwrap_or(created_at);

        let mut attributes = Vec::new();
        let mut notes = String::new();

        let mut in_attributes = false;
        let mut in_notes = false;

        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## Attributes") {
                in_attributes = true;
                in_notes = false;
                continue;
            } else if trimmed.starts_with("## Notes") {
                in_attributes = false;
                in_notes = true;
                continue;
            }

            if in_attributes && trimmed.starts_with("- ") {
                let content = &trimmed[2..];
                let mut key = String::new();
                let mut value = String::new();
                let mut from = None;
                let mut to = None;

                for part in content.split('|') {
                    let p = part.trim();
                    if let Some(v) = p.strip_prefix("key:") {
                        key = unescape_pipe(v.trim());
                    } else if let Some(v) = p.strip_prefix("value:") {
                        value = unescape_pipe(v.trim());
                    } else if let Some(v) = p.strip_prefix("from:") {
                        let t = v.trim();
                        if t != "null" && !t.is_empty() {
                            from = Some(t.to_string());
                        }
                    } else if let Some(v) = p.strip_prefix("to:") {
                        let t = v.trim();
                        if t != "null" && !t.is_empty() {
                            to = Some(t.to_string());
                        }
                    }
                }
                if !key.is_empty() {
                    attributes.push(EntityAttribute {
                        key,
                        value,
                        from,
                        to,
                    });
                }
            } else if in_notes {
                if !notes.is_empty() {
                    notes.push('\n');
                }
                notes.push_str(line);
            }
        }

        Ok(Self {
            id: entity_id_for_name(&name),
            name,
            entity_type,
            aliases,
            properties: BTreeMap::new(),
            attributes,
            notes: notes.trim().to_string(),
            created_at,
            updated_at,
        })
    }
}

/// One relationship triple between subject and object with temporal validity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Triple {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub confidence: f32,
    pub source_record: Option<String>,
    pub source_file: Option<String>,
    pub source_record_id: Option<String>,
    pub adapter_name: Option<String>,
    pub extracted_at: DateTime<Utc>,
}

impl Triple {
    pub fn is_current(&self) -> bool {
        self.valid_to.is_none()
    }

    pub fn render_line(&self) -> String {
        let mut parts = Vec::new();
        parts.push(format!("id: {}", self.id));
        parts.push(format!("p: {}", escape_pipe(&self.predicate)));
        parts.push(format!("o: {}", escape_pipe(&self.object)));
        parts.push(format!(
            "from: {}",
            self.valid_from.as_deref().unwrap_or("null")
        ));
        parts.push(format!(
            "to: {}",
            self.valid_to.as_deref().unwrap_or("null")
        ));
        parts.push(format!("conf: {:.4}", self.confidence));
        if let Some(src) = &self.source_record {
            parts.push(format!("src: {}", escape_pipe(src)));
        }
        if let Some(file) = &self.source_file {
            parts.push(format!("file: {}", escape_pipe(file)));
        }
        if let Some(record) = &self.source_record_id {
            parts.push(format!("record: {}", escape_pipe(record)));
        }
        if let Some(adapter) = &self.adapter_name {
            parts.push(format!("adapter: {}", escape_pipe(adapter)));
        }
        format!("- {}", parts.join(" | "))
    }

    pub fn parse_line(subject: &str, line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let content = trimmed.strip_prefix("- ")?;

        let mut id = String::new();
        let mut predicate = String::new();
        let mut object = String::new();
        let mut valid_from = None;
        let mut valid_to = None;
        let mut confidence = 1.0f32;
        let mut source_record = None;
        let mut source_file = None;
        let mut source_record_id = None;
        let mut adapter_name = None;

        for part in content.split('|') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix("id:") {
                id = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("p:") {
                predicate = unescape_pipe(v.trim());
            } else if let Some(v) = p.strip_prefix("o:") {
                object = unescape_pipe(v.trim());
            } else if let Some(v) = p.strip_prefix("from:") {
                let s = v.trim();
                if s != "null" && !s.is_empty() {
                    valid_from = Some(s.to_string());
                }
            } else if let Some(v) = p.strip_prefix("to:") {
                let s = v.trim();
                if s != "null" && !s.is_empty() {
                    valid_to = Some(s.to_string());
                }
            } else if let Some(v) = p.strip_prefix("conf:") {
                if let Ok(c) = v.trim().parse::<f32>() {
                    confidence = c;
                }
            } else if let Some(v) = p.strip_prefix("src:") {
                let s = unescape_pipe(v.trim());
                if !s.is_empty() {
                    source_record = Some(s);
                }
            } else if let Some(v) = p.strip_prefix("file:") {
                let s = unescape_pipe(v.trim());
                if !s.is_empty() {
                    source_file = Some(s);
                }
            } else if let Some(v) = p.strip_prefix("record:") {
                let s = unescape_pipe(v.trim());
                if !s.is_empty() {
                    source_record_id = Some(s);
                }
            } else if let Some(v) = p.strip_prefix("adapter:") {
                let s = unescape_pipe(v.trim());
                if !s.is_empty() {
                    adapter_name = Some(s);
                }
            }
        }

        if predicate.is_empty() || object.is_empty() {
            return None;
        }

        if id.is_empty() {
            id = make_triple_id(
                &entity_id_for_name(subject),
                &predicate,
                &entity_id_for_name(&object),
                valid_from.as_deref(),
                "legacy",
            );
        }

        Some(Self {
            id,
            subject: subject.to_string(),
            predicate,
            object,
            valid_from,
            valid_to,
            confidence,
            source_record,
            source_file,
            source_record_id,
            adapter_name,
            extracted_at: Utc::now(),
        })
    }
}

fn escape_pipe(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', "\\n")
}

fn unescape_pipe(s: &str) -> String {
    s.replace("\\|", "|")
        .replace("\\n", "\n")
        .replace("\\\\", "\\")
}

/// Query result for an entity relationships lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityQueryResult {
    pub entity: String,
    pub as_of: Option<String>,
    pub active_facts: Vec<Triple>,
    pub historical_facts: Vec<Triple>,
    pub future_facts: Vec<Triple>,
    pub facts: Vec<Triple>,
    pub count: usize,
    pub resolved_from: Option<String>,
    pub candidates: Vec<EntityCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityCandidate {
    pub id: String,
    pub name: String,
}

/// Statistics for the entire knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGraphStats {
    pub entities: usize,
    pub triples: usize,
    pub current_facts: usize,
    pub expired_facts: usize,
    pub relationship_types: Vec<String>,
}

/// The Knowledge Graph manager handling entity and triple files under `graph/`.
pub struct KnowledgeGraph {
    pub root: PathBuf,
}

impl KnowledgeGraph {
    pub fn new(store_root: PathBuf) -> Self {
        Self { root: store_root }
    }

    pub fn entities_dir(&self) -> PathBuf {
        self.root.join("graph").join("entities")
    }

    pub fn triples_dir(&self) -> PathBuf {
        self.root.join("graph").join("triples")
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join("search_index").join("kg.jsonl")
    }

    pub fn ensure_dirs(&self) -> Result<(), HarnessError> {
        fs::create_dir_all(self.entities_dir()).map_err(|e| kg_err(e.to_string()))?;
        fs::create_dir_all(self.triples_dir()).map_err(|e| kg_err(e.to_string()))?;
        fs::create_dir_all(self.root.join("search_index")).map_err(|e| kg_err(e.to_string()))?;
        Ok(())
    }

    // ── Entity Operations ──────────────────────────────────────────────────

    pub fn entity_file_path(&self, entity_id: &str) -> PathBuf {
        self.entities_dir()
            .join(format!("{}.md", safe_id(entity_id)))
    }

    pub fn get_entity(&self, name_or_id: &str) -> Result<Option<Entity>, HarnessError> {
        let eid = entity_id_for_name(name_or_id);
        let path = self.entity_file_path(&eid);
        if path.is_file() {
            let text = fs::read_to_string(&path).map_err(|e| kg_err(e.to_string()))?;
            return Ok(Some(Entity::from_document(&eid, &text)?));
        }
        // Try fallback scan over all entities by name match
        for ent in self.list_entities()? {
            if ent.id == eid || ent.name.eq_ignore_ascii_case(name_or_id.trim()) {
                return Ok(Some(ent));
            }
        }
        Ok(None)
    }

    pub fn list_entities(&self) -> Result<Vec<Entity>, HarnessError> {
        let dir = self.entities_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut list = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| kg_err(e.to_string()))? {
            let entry = entry.map_err(|e| kg_err(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Ok(text) = fs::read_to_string(&path) {
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    if let Ok(ent) = Entity::from_document(id, &text) {
                        list.push(ent);
                    }
                }
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(list)
    }

    pub fn add_entity(
        &self,
        name: &str,
        entity_type: Option<&str>,
        properties: Option<BTreeMap<String, String>>,
        notes: Option<&str>,
    ) -> Result<String, HarnessError> {
        self.ensure_dirs()?;
        let eid = entity_id_for_name(name);
        let mut entity = self
            .get_entity(name)?
            .unwrap_or_else(|| Entity::new(name, entity_type));

        if let Some(t) = entity_type {
            if t != "unknown" {
                entity.entity_type = t.to_string();
            }
        }
        if let Some(props) = properties {
            entity.properties.extend(props);
        }
        if let Some(n) = notes {
            if !n.trim().is_empty() {
                entity.notes = n.trim().to_string();
            }
        }
        entity.updated_at = Utc::now();

        let path = self.entity_file_path(&eid);
        atomic::write_str(&path, &entity.to_document()).map_err(|e| kg_err(e.to_string()))?;
        Ok(eid)
    }

    // ── Triple Operations ──────────────────────────────────────────────────

    pub fn triple_file_path(&self, subject_id: &str) -> PathBuf {
        self.triples_dir()
            .join(format!("{}.md", safe_id(subject_id)))
    }

    pub fn load_subject_triples(
        &self,
        subject_id: &str,
    ) -> Result<(String, Vec<Triple>), HarnessError> {
        let path = self.triple_file_path(subject_id);
        if !path.is_file() {
            return Ok((String::new(), Vec::new()));
        }
        let text = fs::read_to_string(&path).map_err(|e| kg_err(e.to_string()))?;
        let (yaml, body) = frontmatter::split(&text)
            .ok_or_else(|| kg_err("missing front matter in triples file"))?;
        let val = frontmatter::parse(yaml).map_err(|e| kg_err(e.to_string()))?;
        let subject =
            frontmatter::string(&val, "subject").unwrap_or_else(|| subject_id.to_string());

        let mut triples = Vec::new();
        for line in body.lines() {
            if let Some(t) = Triple::parse_line(&subject, line) {
                triples.push(t);
            }
        }
        Ok((subject, triples))
    }

    pub fn save_subject_triples(
        &self,
        subject: &str,
        triples: &[Triple],
    ) -> Result<(), HarnessError> {
        self.ensure_dirs()?;
        let sub_id = entity_id_for_name(subject);
        let path = self.triple_file_path(&sub_id);

        let yaml = format!(
            "subject: {}\nupdated_at: {}",
            subject,
            Utc::now().to_rfc3339()
        );
        let mut body = String::new();
        for t in triples {
            body.push_str(&t.render_line());
            body.push('\n');
        }

        let doc = frontmatter::render(&yaml, &body);
        atomic::write_str(&path, &doc).map_err(|e| kg_err(e.to_string()))?;
        Ok(())
    }

    /// Add a relationship triple: subject → predicate → object.
    /// Deduplicates open triples and identical closed triples (replays).
    #[allow(clippy::too_many_arguments)]
    pub fn add_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: Option<&str>,
        valid_to: Option<&str>,
        confidence: Option<f32>,
        source_record: Option<&str>,
        source_file: Option<&str>,
        source_record_id: Option<&str>,
        adapter_name: Option<&str>,
    ) -> Result<String, HarnessError> {
        self.ensure_dirs()?;
        validate_temporal_interval(valid_from, valid_to)?;

        let pred = predicate.trim().to_lowercase().replace(' ', "_");
        let sub_id = entity_id_for_name(subject);
        let obj_id = entity_id_for_name(object);

        // Auto-create entity files if missing
        if self.get_entity(subject)?.is_none() {
            self.add_entity(subject, None, None, None)?;
        }
        if self.get_entity(object)?.is_none() {
            self.add_entity(object, None, None, None)?;
        }

        let (canonical_subject, mut triples) = self.load_subject_triples(&sub_id)?;
        let subject_to_use = if canonical_subject.is_empty() {
            subject
        } else {
            &canonical_subject
        };

        // 1. Check for existing identical open triple
        for existing in &triples {
            if existing.predicate == pred
                && entity_id_for_name(&existing.object) == obj_id
                && existing.valid_to.is_none()
            {
                return Ok(existing.id.clone());
            }
        }

        // 2. Check for identical closed assertion replay
        if valid_to.is_some() {
            for existing in &triples {
                if existing.predicate == pred
                    && entity_id_for_name(&existing.object) == obj_id
                    && existing.valid_from.as_deref() == valid_from
                    && existing.valid_to.as_deref() == valid_to
                    && confidence.is_none_or(|value| existing.confidence == value)
                    && existing.source_record.as_deref() == source_record
                    && existing.source_file.as_deref() == source_file
                {
                    return Ok(existing.id.clone());
                }
            }
        }

        let now_iso = Utc::now().to_rfc3339();
        let triple_id = make_triple_id(&sub_id, &pred, &obj_id, valid_from, &now_iso);

        let new_triple = Triple {
            id: triple_id.clone(),
            subject: subject_to_use.to_string(),
            predicate: pred,
            object: object.trim().to_string(),
            valid_from: valid_from.map(str::to_string),
            valid_to: valid_to.map(str::to_string),
            confidence: confidence.unwrap_or(1.0),
            source_record: source_record.map(str::to_string),
            source_file: source_file.map(str::to_string),
            source_record_id: source_record_id.map(str::to_string),
            adapter_name: adapter_name.map(str::to_string),
            extracted_at: Utc::now(),
        };

        triples.push(new_triple);
        self.save_subject_triples(subject_to_use, &triples)?;
        self.rebuild_index()?;

        Ok(triple_id)
    }

    /// Mark a relationship as no longer valid (set valid_to date/time).
    pub fn invalidate(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        ended: Option<&str>,
    ) -> Result<usize, HarnessError> {
        let sub_id = entity_id_for_name(subject);
        let obj_id = entity_id_for_name(object);
        let pred = predicate.trim().to_lowercase().replace(' ', "_");

        let ended_val = ended
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());

        let (canonical_subject, mut triples) = self.load_subject_triples(&sub_id)?;
        let mut modified = 0;

        for triple in &mut triples {
            if triple.predicate == pred
                && entity_id_for_name(&triple.object) == obj_id
                && triple.valid_to.is_none()
            {
                if let Some(from) = &triple.valid_from {
                    if let (Some(ek), Some(sk)) = (
                        temporal_end_key(Some(&ended_val)),
                        temporal_start_key(Some(from)),
                    ) {
                        if ek < sk {
                            return Err(kg_err(format!(
                                "valid_to={ended_val:?} is before valid_from={from:?}; an inverted interval would be invisible to every KG query"
                            )));
                        }
                    }
                }
                triple.valid_to = Some(ended_val.clone());
                modified += 1;
            }
        }

        if modified > 0 {
            self.save_subject_triples(&canonical_subject, &triples)?;
            self.rebuild_index()?;
        }

        Ok(modified)
    }

    /// Atomically replace one fact with another at a single shared boundary.
    /// Direct port of the reference design's `supersede`.
    #[allow(clippy::too_many_arguments)]
    pub fn supersede(
        &self,
        subject: &str,
        predicate: &str,
        old_obj: &str,
        new_obj: &str,
        at: Option<&str>,
        confidence: Option<f32>,
        source_record: Option<&str>,
        source_file: Option<&str>,
        source_record_id: Option<&str>,
        adapter_name: Option<&str>,
    ) -> Result<String, HarnessError> {
        let sub_id = entity_id_for_name(subject);
        let old_id = entity_id_for_name(old_obj);
        let new_id = entity_id_for_name(new_obj);
        let pred = predicate.trim().to_lowercase().replace(' ', "_");

        let boundary = match at {
            None => Utc::now().to_rfc3339(),
            Some(instant) => {
                if is_date_only_temporal(instant) {
                    format!("{instant}T00:00:00Z")
                } else {
                    instant.trim().to_string()
                }
            }
        };

        // Ensure entities exist
        if self.get_entity(subject)?.is_none() {
            self.add_entity(subject, None, None, None)?;
        }
        if self.get_entity(new_obj)?.is_none() {
            self.add_entity(new_obj, None, None, None)?;
        }

        let (canonical_subject, mut triples) = self.load_subject_triples(&sub_id)?;
        let subject_to_use = if canonical_subject.is_empty() {
            subject
        } else {
            &canonical_subject
        };

        // Verify boundary is not before old fact's valid_from
        for t in &triples {
            if t.predicate == pred
                && entity_id_for_name(&t.object) == old_id
                && t.valid_to.is_none()
            {
                if let Some(from) = &t.valid_from {
                    if let (Some(ek), Some(sk)) = (
                        temporal_end_key(Some(&boundary)),
                        temporal_start_key(Some(from)),
                    ) {
                        if ek < sk {
                            return Err(kg_err(format!(
                                "at={boundary:?} is before valid_from={from:?}; an inverted interval would be invisible to every KG query"
                            )));
                        }
                    }
                }
            }
        }

        // Close open old fact
        for t in &mut triples {
            if t.predicate == pred
                && entity_id_for_name(&t.object) == old_id
                && t.valid_to.is_none()
            {
                t.valid_to = Some(boundary.clone());
            }
        }

        // Check if successor is already open
        for t in &triples {
            if t.predicate == pred
                && entity_id_for_name(&t.object) == new_id
                && t.valid_to.is_none()
            {
                self.save_subject_triples(subject_to_use, &triples)?;
                self.rebuild_index()?;
                return Ok(t.id.clone());
            }
        }

        // Open successor at same instant
        let triple_id = make_triple_id(
            &sub_id,
            &pred,
            &new_id,
            Some(&boundary),
            &Utc::now().to_rfc3339(),
        );
        triples.push(Triple {
            id: triple_id.clone(),
            subject: subject_to_use.to_string(),
            predicate: pred,
            object: new_obj.trim().to_string(),
            valid_from: Some(boundary),
            valid_to: None,
            confidence: confidence.unwrap_or(1.0),
            source_record: source_record.map(str::to_string),
            source_file: source_file.map(str::to_string),
            source_record_id: source_record_id.map(str::to_string),
            adapter_name: adapter_name.map(str::to_string),
            extracted_at: Utc::now(),
        });

        self.save_subject_triples(subject_to_use, &triples)?;
        self.rebuild_index()?;

        Ok(triple_id)
    }

    /// Load all triples across all subjects.
    pub fn all_triples(&self) -> Result<Vec<Triple>, HarnessError> {
        let dir = self.triples_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| kg_err(e.to_string()))? {
            let entry = entry.map_err(|e| kg_err(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let (_, triples) = self.load_subject_triples(id)?;
                all.extend(triples);
            }
        }
        Ok(all)
    }

    /// Rebuild the derived index `search_index/kg.jsonl`.
    pub fn rebuild_index(&self) -> Result<(), HarnessError> {
        self.ensure_dirs()?;
        let all = self.all_triples()?;
        let mut lines = Vec::new();
        for t in all {
            if let Ok(json) = serde_json::to_string(&t) {
                lines.push(json);
            }
        }
        let content = lines.join("\n");
        let idx_path = self.index_path();
        atomic::write_str(&idx_path, &content).map_err(|e| kg_err(e.to_string()))?;
        Ok(())
    }

    /// Query the knowledge graph for an entity's relationships.
    /// direction: "outgoing", "incoming", "both".
    /// as_of: optional point-in-time ISO filter.
    pub fn query_entity(
        &self,
        name: &str,
        as_of: Option<&str>,
        direction: Option<&str>,
    ) -> Result<EntityQueryResult, HarnessError> {
        let dir = direction.unwrap_or("both");
        let eid = entity_id_for_name(name);
        let all = self.all_triples()?;

        let mut matches = Vec::new();
        for t in &all {
            let sub_match = entity_id_for_name(&t.subject) == eid;
            let obj_match = entity_id_for_name(&t.object) == eid;

            let in_direction = match dir {
                "outgoing" => sub_match,
                "incoming" => obj_match,
                _ => sub_match || obj_match,
            };

            if in_direction && matches_as_of(t, as_of) {
                matches.push(t.clone());
            }
        }

        let mut resolved_from = None;
        let mut candidates = Vec::new();

        if matches.is_empty() {
            // Check if exact entity exists
            let exact_exists = self.get_entity(name)?.is_some();
            if !exact_exists {
                candidates = self.find_entity_candidates(name)?;
                if candidates.len() == 1 {
                    let cand = &candidates[0];
                    let cand_eid = &cand.id;
                    for t in &all {
                        let sub_match = entity_id_for_name(&t.subject) == *cand_eid;
                        let obj_match = entity_id_for_name(&t.object) == *cand_eid;
                        let in_direction = match dir {
                            "outgoing" => sub_match,
                            "incoming" => obj_match,
                            _ => sub_match || obj_match,
                        };
                        if in_direction && matches_as_of(t, as_of) {
                            matches.push(t.clone());
                        }
                    }
                    if !matches.is_empty() {
                        resolved_from = Some(name.trim().to_string());
                    }
                }
            }
        }

        let mut active = Vec::new();
        let mut historical = Vec::new();
        let mut future = Vec::new();

        if as_of.is_none() {
            let now_key = Utc::now().to_rfc3339();
            for t in &matches {
                let start_key = temporal_start_key(t.valid_from.as_deref());
                let end_key = temporal_end_key(t.valid_to.as_deref());

                if let Some(sk) = start_key {
                    if sk > now_key {
                        future.push(t.clone());
                        continue;
                    }
                }
                if let Some(ek) = end_key {
                    if ek < now_key {
                        historical.push(t.clone());
                        continue;
                    }
                }
                active.push(t.clone());
            }
        } else {
            active = matches.clone();
        }

        let count = matches.len();
        let final_entity = if resolved_from.is_some() && !candidates.is_empty() {
            candidates[0].name.clone()
        } else {
            name.trim().to_string()
        };

        Ok(EntityQueryResult {
            entity: final_entity,
            as_of: as_of.map(str::to_string),
            active_facts: active,
            historical_facts: historical,
            future_facts: future,
            facts: matches,
            count,
            resolved_from,
            candidates,
        })
    }

    /// Look up entity candidates for disambiguation (prefix / token matching, never arbitrary substring).
    pub fn find_entity_candidates(&self, name: &str) -> Result<Vec<EntityCandidate>, HarnessError> {
        let needle = canonical_entity_name(name);
        if needle.len() < MIN_ENTITY_CANDIDATE_LEN {
            return Ok(Vec::new());
        }
        let entities = self.list_entities()?;
        let mut out = Vec::new();

        for ent in entities {
            let canon = canonical_entity_name(&ent.name);
            let eid = entity_id_for_name(&ent.name);
            // Matches: starts with needle + " ", or ends with " " + needle, or contains " " + needle + " ",
            // or prefix match on tokens
            let matched = canon.starts_with(&format!("{needle} "))
                || canon.ends_with(&format!(" {needle}"))
                || canon.contains(&format!(" {needle} "))
                || eid.starts_with(&format!("{needle}_"))
                || eid.ends_with(&format!("_{needle}"))
                || eid.contains(&format!("_{needle}_"))
                || canon.starts_with(&needle);

            if matched && canon != needle {
                out.push(EntityCandidate {
                    id: ent.id,
                    name: ent.name,
                });
            }
            if out.len() >= 5 {
                break;
            }
        }
        Ok(out)
    }

    /// Query all triples matching a relationship predicate.
    pub fn query_relationship(
        &self,
        predicate: &str,
        as_of: Option<&str>,
    ) -> Result<Vec<Triple>, HarnessError> {
        let pred = predicate.trim().to_lowercase().replace(' ', "_");
        let all = self.all_triples()?;
        Ok(all
            .into_iter()
            .filter(|t| t.predicate == pred && matches_as_of(t, as_of))
            .collect())
    }

    /// Get facts in chronological order, optionally filtered by entity.
    pub fn timeline(
        &self,
        entity_name: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Triple>, HarnessError> {
        let eid = entity_name.map(entity_id_for_name);
        let mut all = self.all_triples()?;

        if let Some(target_id) = eid {
            all.retain(|t| {
                entity_id_for_name(&t.subject) == target_id
                    || entity_id_for_name(&t.object) == target_id
            });
        }

        // Order by valid_from ASC NULLS LAST, id ASC (matches the reference design)
        all.sort_by(|a, b| {
            let ka = temporal_start_key(a.valid_from.as_deref());
            let kb = temporal_start_key(b.valid_from.as_deref());
            match (&ka, &kb) {
                (Some(va), Some(vb)) => va.cmp(vb).then_with(|| a.id.cmp(&b.id)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.id.cmp(&b.id),
            }
        });

        let paged = all.into_iter().skip(offset).take(limit).collect();
        Ok(paged)
    }

    /// Knowledge graph overview stats.
    pub fn stats(&self) -> Result<KnowledgeGraphStats, HarnessError> {
        let entities = self.list_entities()?.len();
        let triples = self.all_triples()?;
        let total_triples = triples.len();
        let current_facts = triples.iter().filter(|t| t.valid_to.is_none()).count();
        let expired_facts = total_triples.saturating_sub(current_facts);

        let mut preds = BTreeSet::new();
        for t in &triples {
            preds.insert(t.predicate.clone());
        }

        Ok(KnowledgeGraphStats {
            entities,
            triples: total_triples,
            current_facts,
            expired_facts,
            relationship_types: preds.into_iter().collect(),
        })
    }
}

/// Helper function to check if a triple satisfies the point-in-time `as_of` instant.
/// Upper bound is strict (valid_to > as_of) for half-open intervals [valid_from, valid_to).
fn matches_as_of(t: &Triple, as_of: Option<&str>) -> bool {
    let Some(query_instant) = as_of else {
        return true;
    };
    let as_of_key = match temporal_start_key(Some(query_instant)) {
        Some(k) => k,
        None => return true,
    };

    if let Some(from) = &t.valid_from {
        if let Some(sk) = temporal_start_key(Some(from)) {
            if sk > as_of_key {
                return false;
            }
        }
    }

    if let Some(to) = &t.valid_to {
        if let Some(ek) = temporal_end_key(Some(to)) {
            if ek <= as_of_key {
                return false;
            }
        }
    }

    true
}
