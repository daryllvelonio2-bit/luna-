use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use harness_core::{MemoryCategory, MemoryEngine, MemoryRecord, Note, NotesEngine};
use harness_memory::build_memory_engine;
use harness_note_graph::{graph_notes, Confidence, NodeKind, Relation};
use harness_notes::projects::project_from_tags;
use harness_notes::FileNotesEngine;

use crate::state::WebState;

pub fn knowledge_router() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/memories", get(search_memories))
        .route("/api/notes", get(search_notes))
        .route("/api/notes/{note_id}", get(get_note))
        .route("/api/knowledge-graph", get(get_knowledge_graph))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeNodeMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeNodeDto {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub subtitle: String,
    pub category: String,
    pub cluster: usize,
    pub degree: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub preview: String,
    pub tags: Vec<String>,
    pub meta: KnowledgeNodeMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeEdgeDto {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: String,
    pub weight: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeClusterDto {
    pub id: usize,
    pub label: String,
    pub node_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeStatsDto {
    pub total_memories: usize,
    pub total_notes: usize,
    pub total_hubs: usize,
    pub total_tags: usize,
    pub total_concepts: usize,
    pub total_nodes: usize,
    pub total_edges: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeGraphResponse {
    pub nodes: Vec<KnowledgeNodeDto>,
    pub edges: Vec<KnowledgeEdgeDto>,
    pub clusters: Vec<KnowledgeClusterDto>,
    pub stats: KnowledgeStatsDto,
    pub memories: Vec<serde_json::Value>,
    pub notes: Vec<serde_json::Value>,
}

fn memory_to_json(r: &MemoryRecord) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "subject": r.subject,
        "predicate": r.predicate,
        "object": r.object,
        "name": r.name,
        "path": r.path,
        "category": r.category,
        "confidence": r.confidence,
        "text": r.raw_content,
        "created_at": r.created_at.to_rfc3339(),
        "updated_at": r.updated_at.to_rfc3339(),
    })
}

fn note_to_json(n: &Note, include_body: bool) -> serde_json::Value {
    let project = project_from_tags(&n.tags);
    let visible_tags: Vec<String> = n
        .tags
        .iter()
        .filter(|t| !t.starts_with("project:"))
        .cloned()
        .collect();
    serde_json::json!({
        "id": n.id,
        "title": n.title,
        "body": if include_body { n.content.clone() } else { n.summary_pointer.clone() },
        "summary_pointer": n.summary_pointer,
        "tags": visible_tags,
        "project": project,
        "created_at": n.created_at.to_rfc3339(),
        "updated_at": n.updated_at.to_rfc3339(),
    })
}

async fn search_memories(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let q = query.q.unwrap_or_default();
    let limit = if q.trim().is_empty() { 120 } else { 80 };
    let cfg = state.config.read().await.clone();
    let engine = build_memory_engine(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let records = engine
        .retrieve(&q, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let out = records
        .iter()
        .filter(|r| r.category != MemoryCategory::Transcript)
        .map(memory_to_json)
        .collect();
    Ok(Json(out))
}

async fn search_notes(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let q = query.q.unwrap_or_default();
    let (notes_dir, notes_db) = {
        let cfg = state.config.read().await;
        (cfg.paths.notes_dir.clone(), cfg.paths.notes_db.clone())
    };
    let engine = FileNotesEngine::new(notes_dir, Some(notes_db))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let notes = if q.trim().is_empty() {
        engine
            .list_notes(120, 0)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        engine
            .search_notes(&q, 80)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    let out = notes.iter().map(|n| note_to_json(n, true)).collect();
    Ok(Json(out))
}

async fn get_note(
    State(state): State<Arc<WebState>>,
    Path(note_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (notes_dir, notes_db) = {
        let cfg = state.config.read().await;
        (cfg.paths.notes_dir.clone(), cfg.paths.notes_db.clone())
    };
    let engine = FileNotesEngine::new(notes_dir, Some(notes_db))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let note = engine
        .read_note(&note_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(note_to_json(&note, true)))
}

async fn get_knowledge_graph(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<KnowledgeGraphResponse>, (StatusCode, String)> {
    let q = query.q.unwrap_or_default();
    let cfg = state.config.read().await.clone();
    let engine = build_memory_engine(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let notes_engine = FileNotesEngine::new(cfg.paths.notes_dir.clone(), Some(cfg.paths.notes_db))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mem_limit = if q.trim().is_empty() { 150 } else { 100 };
    let memories: Vec<MemoryRecord> = engine
        .retrieve(&q, mem_limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .filter(|r| r.category != MemoryCategory::Transcript)
        .collect();

    let notes = if q.trim().is_empty() {
        notes_engine
            .list_notes(120, 0)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        notes_engine
            .search_notes(&q, 80)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    Ok(Json(build_unified_knowledge_graph(&memories, &notes)))
}

fn normalize_slug(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .replace(['_', ' '], "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn title_case_slug(raw: &str) -> String {
    let cleaned = raw.replace(['.', '_', '-'], " ");
    cleaned
        .split_whitespace()
        .map(|w| {
            let lower = w.to_ascii_lowercase();
            match lower.as_str() {
                "llm" | "ai" | "ui" | "api" | "mcp" | "tts" | "stt" | "cli" => {
                    lower.to_ascii_uppercase()
                }
                "dotnet" => ".NET".to_string(),
                "youtube" => "YouTube".to_string(),
                "dsh" => "dsh".to_string(),
                _ => {
                    let mut chars = lower.chars();
                    match chars.next() {
                        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                        None => String::new(),
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_memory_hub(rec: &MemoryRecord) -> (String, String, String) {
    let subj = rec.subject.trim();
    let parts: Vec<&str> = subj.split('.').filter(|s| !s.is_empty()).collect();
    if parts.first().copied() == Some("project") && parts.len() >= 2 {
        let slug = normalize_slug(parts[1]);
        let label = rec
            .name
            .as_deref()
            .filter(|s| !s.trim().is_empty() && parts.len() == 2)
            .map(str::to_string)
            .unwrap_or_else(|| title_case_slug(parts[1]));
        return (format!("hub:project:{slug}"), label, "Project".to_string());
    }
    if parts.first().copied() == Some("user") {
        if parts.get(1).copied() == Some("automation") {
            let topic = parts.get(2).copied().unwrap_or("automations");
            let slug = normalize_slug(topic);
            return (
                format!("hub:project:{slug}"),
                title_case_slug(topic),
                "Automation".to_string(),
            );
        }
        return (
            "hub:domain:user".to_string(),
            "User & Preferences".to_string(),
            "Domain".to_string(),
        );
    }
    if parts.first().copied() == Some("tool") || parts.first().copied() == Some("environment") {
        return (
            "hub:domain:environment".to_string(),
            "Tools & Environment".to_string(),
            "Domain".to_string(),
        );
    }
    let ns = parts.first().copied().unwrap_or("general");
    let slug = normalize_slug(ns);
    (
        format!("hub:domain:{slug}"),
        title_case_slug(ns),
        "Domain".to_string(),
    )
}

fn format_memory_label(rec: &MemoryRecord) -> String {
    let parts: Vec<&str> = rec.subject.split('.').filter(|s| !s.is_empty()).collect();
    let scope = if parts.first().copied() == Some("project") && parts.len() >= 3 {
        title_case_slug(&parts[2..].join(" "))
    } else if parts.len() >= 2 && parts[0] == "user" {
        title_case_slug(&parts[1..].join(" "))
    } else if let Some(name) = rec.name.as_deref().filter(|s| !s.trim().is_empty()) {
        name.to_string()
    } else {
        title_case_slug(&rec.subject)
    };
    let pred = title_case_slug(&rec.predicate);
    if pred.is_empty() || scope.eq_ignore_ascii_case(&pred) {
        scope
    } else {
        format!("{scope} · {pred}")
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{head}…")
}

/// Build a unified Neuron Knowledge Graph combining active memories, workspace notes,
/// project/domain hubs, tags, concepts, and cross-synaptic connections.
pub fn build_unified_knowledge_graph(
    memories: &[MemoryRecord],
    notes: &[Note],
) -> KnowledgeGraphResponse {
    let mut nodes_map: BTreeMap<String, KnowledgeNodeDto> = BTreeMap::new();
    let mut edge_keys: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut edges: Vec<KnowledgeEdgeDto> = Vec::new();
    let mut cluster_ids: HashMap<String, usize> = HashMap::new();
    let mut cluster_labels: BTreeMap<usize, String> = BTreeMap::new();

    let mut ensure_cluster = |hub_id: &str, label: &str| -> usize {
        if let Some(&cid) = cluster_ids.get(hub_id) {
            return cid;
        }
        let cid = cluster_ids.len() + 1;
        cluster_ids.insert(hub_id.to_string(), cid);
        cluster_labels.insert(cid, label.to_string());
        cid
    };

    let mut add_edge = |source: &str,
                        target: &str,
                        relation: &str,
                        confidence: &str,
                        weight: f32,
                        reason: String| {
        if source == target {
            return;
        }
        let (a, b) = if source <= target {
            (source.to_string(), target.to_string())
        } else {
            (target.to_string(), source.to_string())
        };
        if edge_keys.insert((a, b, relation.to_string())) {
            let idx = edges.len() + 1;
            edges.push(KnowledgeEdgeDto {
                id: format!("e_{idx}"),
                source: source.to_string(),
                target: target.to_string(),
                relation: relation.to_string(),
                confidence: confidence.to_string(),
                weight,
                reason,
            });
        }
    };

    // 1. Add memory neurons & their project/domain hubs
    let mut hub_keywords: Vec<(String, String, Vec<String>)> = Vec::new();

    for rec in memories {
        let (hub_id, hub_label, hub_sub) = extract_memory_hub(rec);
        let cluster = ensure_cluster(&hub_id, &hub_label);

        nodes_map
            .entry(hub_id.clone())
            .or_insert_with(|| KnowledgeNodeDto {
                id: hub_id.clone(),
                kind: "hub".to_string(),
                label: hub_label.clone(),
                subtitle: hub_sub.clone(),
                category: "hub".to_string(),
                cluster,
                degree: 0,
                ref_id: None,
                updated_at: None,
                preview: format!("Knowledge hub grouping related memories & notes for {hub_label}"),
                tags: Vec::new(),
                meta: KnowledgeNodeMeta {
                    subject: Some(hub_id.clone()),
                    predicate: None,
                    object: None,
                    path: rec.path.clone(),
                    confidence: None,
                    project: Some(hub_label.clone()),
                },
            });

        let kws: Vec<String> = hub_label
            .split_whitespace()
            .map(|w| w.to_ascii_lowercase())
            .filter(|w| w.len() >= 4)
            .collect();
        if !kws.is_empty() && !hub_keywords.iter().any(|(id, _, _)| id == &hub_id) {
            hub_keywords.push((hub_id.clone(), hub_label.clone(), kws));
        }

        let mem_node_id = format!("mem:{}", rec.id);
        let cat_str = serde_json::to_value(rec.category)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "fact".to_string());
        let label = format_memory_label(rec);
        let preview = if !rec.object.trim().is_empty() {
            rec.object.trim().to_string()
        } else {
            truncate_preview(&rec.raw_content, 160)
        };

        nodes_map.insert(
            mem_node_id.clone(),
            KnowledgeNodeDto {
                id: mem_node_id.clone(),
                kind: "memory".to_string(),
                label,
                subtitle: format!("{} · {}", title_case_slug(&cat_str), rec.predicate),
                category: cat_str,
                cluster,
                degree: 0,
                ref_id: Some(rec.id.clone()),
                updated_at: Some(rec.updated_at.to_rfc3339()),
                preview,
                tags: Vec::new(),
                meta: KnowledgeNodeMeta {
                    subject: Some(rec.subject.clone()),
                    predicate: Some(rec.predicate.clone()),
                    object: Some(rec.object.clone()),
                    path: rec.path.clone(),
                    confidence: Some(rec.confidence),
                    project: Some(hub_label.clone()),
                },
            },
        );

        add_edge(
            &mem_node_id,
            &hub_id,
            "belongs_to",
            "EXTRACTED",
            1.0,
            format!("Grouped under {hub_label}"),
        );
    }

    // 2. Build note knowledge graph using `harness-note-graph`
    let built = graph_notes(notes);
    let mut note_graph_node_to_ui: HashMap<String, String> = HashMap::new();
    let mut node_degree_in_note_graph: HashMap<String, usize> = HashMap::new();
    for e in &built.graph.edges {
        *node_degree_in_note_graph
            .entry(e.source.clone())
            .or_default() += 1;
        *node_degree_in_note_graph
            .entry(e.target.clone())
            .or_default() += 1;
    }

    // Add Note neurons first
    for note in notes {
        let project_slug = project_from_tags(&note.tags).unwrap_or_else(|| "inbox".to_string());
        let norm_proj = normalize_slug(&project_slug);
        let hub_id = format!("hub:project:{norm_proj}");
        let hub_label = title_case_slug(&project_slug);
        let cluster = ensure_cluster(&hub_id, &hub_label);

        nodes_map
            .entry(hub_id.clone())
            .or_insert_with(|| KnowledgeNodeDto {
                id: hub_id.clone(),
                kind: "hub".to_string(),
                label: hub_label.clone(),
                subtitle: "Project".to_string(),
                category: "hub".to_string(),
                cluster,
                degree: 0,
                ref_id: None,
                updated_at: None,
                preview: format!("Workspace project {hub_label}"),
                tags: Vec::new(),
                meta: KnowledgeNodeMeta {
                    subject: Some(hub_id.clone()),
                    predicate: None,
                    object: None,
                    path: None,
                    confidence: None,
                    project: Some(hub_label.clone()),
                },
            });

        let note_ui_id = format!("note:{}", note.id);
        let visible_tags: Vec<String> = note
            .tags
            .iter()
            .filter(|t| !t.starts_with("project:"))
            .cloned()
            .collect();
        let preview = if !note.summary_pointer.trim().is_empty() {
            note.summary_pointer.trim().to_string()
        } else {
            truncate_preview(&note.content, 180)
        };

        nodes_map.insert(
            note_ui_id.clone(),
            KnowledgeNodeDto {
                id: note_ui_id.clone(),
                kind: "note".to_string(),
                label: note.title.clone(),
                subtitle: format!("Note · {hub_label}"),
                category: "note".to_string(),
                cluster,
                degree: 0,
                ref_id: Some(note.id.clone()),
                updated_at: Some(note.updated_at.to_rfc3339()),
                preview,
                tags: visible_tags.clone(),
                meta: KnowledgeNodeMeta {
                    subject: None,
                    predicate: None,
                    object: None,
                    path: None,
                    confidence: None,
                    project: Some(hub_label.clone()),
                },
            },
        );

        note_graph_node_to_ui.insert(format!("note:{}", note.id), note_ui_id.clone());

        add_edge(
            &note_ui_id,
            &hub_id,
            "belongs_to",
            "EXTRACTED",
            1.0,
            format!("In project {hub_label}"),
        );

        // Also infer cross-project topic links when a note's title/tags/content strongly match another hub
        let note_hay = format!(
            "{} {} {}",
            note.title,
            visible_tags.join(" "),
            truncate_preview(&note.content, 500)
        )
        .to_ascii_lowercase();
        for (other_hub_id, other_hub_label, kws) in &hub_keywords {
            if other_hub_id != &hub_id && kws.iter().any(|kw| note_hay.contains(kw.as_str())) {
                add_edge(
                    &note_ui_id,
                    other_hub_id,
                    "related_topic",
                    "INFERRED",
                    0.72,
                    format!("Mentions topic {other_hub_label}"),
                );
            }
        }
    }

    // Add meaningful Tag & Concept bridge nodes from `harness_note_graph` (skipping headings & noisy singletons)
    let mut tag_and_concept_terms: Vec<(String, String, String)> = Vec::new();
    for gnode in built.graph.nodes.values() {
        match gnode.kind {
            NodeKind::Tag => {
                if gnode.label.starts_with("project:") {
                    continue;
                }
                let ui_id = format!("tag:{}", normalize_slug(&gnode.label));
                note_graph_node_to_ui.insert(gnode.id.clone(), ui_id.clone());
                let cluster = 0;
                nodes_map
                    .entry(ui_id.clone())
                    .or_insert_with(|| KnowledgeNodeDto {
                        id: ui_id.clone(),
                        kind: "tag".to_string(),
                        label: format!("#{}", gnode.label),
                        subtitle: "Tag".to_string(),
                        category: "tag".to_string(),
                        cluster,
                        degree: 0,
                        ref_id: None,
                        updated_at: None,
                        preview: format!("Tag #{} linking related notes and memories", gnode.label),
                        tags: vec![gnode.label.clone()],
                        meta: KnowledgeNodeMeta {
                            subject: None,
                            predicate: None,
                            object: None,
                            path: None,
                            confidence: None,
                            project: None,
                        },
                    });
                tag_and_concept_terms.push((
                    ui_id,
                    format!("#{}", gnode.label),
                    gnode.label.to_ascii_lowercase(),
                ));
            }
            NodeKind::Concept => {
                let deg = node_degree_in_note_graph
                    .get(&gnode.id)
                    .copied()
                    .unwrap_or(0);
                if deg < 2 || gnode.label.chars().count() < 4 {
                    continue;
                }
                let ui_id = format!("concept:{}", normalize_slug(&gnode.label));
                note_graph_node_to_ui.insert(gnode.id.clone(), ui_id.clone());
                nodes_map
                    .entry(ui_id.clone())
                    .or_insert_with(|| KnowledgeNodeDto {
                        id: ui_id.clone(),
                        kind: "concept".to_string(),
                        label: title_case_slug(&gnode.label),
                        subtitle: "Shared concept".to_string(),
                        category: "concept".to_string(),
                        cluster: 0,
                        degree: 0,
                        ref_id: None,
                        updated_at: None,
                        preview: format!(
                            "Recurring concept '{}' across multiple notes",
                            gnode.label
                        ),
                        tags: Vec::new(),
                        meta: KnowledgeNodeMeta {
                            subject: None,
                            predicate: None,
                            object: None,
                            path: None,
                            confidence: None,
                            project: None,
                        },
                    });
                tag_and_concept_terms.push((
                    ui_id,
                    title_case_slug(&gnode.label),
                    gnode.label.to_ascii_lowercase(),
                ));
            }
            _ => {}
        }
    }

    // Wire edges from `harness_note_graph` for mapped nodes
    for ge in &built.graph.edges {
        if ge.relation == Relation::Contains {
            continue;
        }
        let Some(src) = note_graph_node_to_ui.get(&ge.source) else {
            continue;
        };
        let Some(dst) = note_graph_node_to_ui.get(&ge.target) else {
            continue;
        };
        let rel = match ge.relation {
            Relation::References => "references",
            Relation::Tagged => "tagged",
            Relation::Mentions => "mentions",
            Relation::PartOf => "part_of",
            Relation::Contains => "contains",
        };
        let conf = match ge.confidence {
            Confidence::Extracted => "EXTRACTED",
            Confidence::Inferred => "INFERRED",
            Confidence::Ambiguous => "AMBIGUOUS",
        };
        let dst_label = nodes_map
            .get(dst)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| dst.clone());
        let reason = match ge.relation {
            Relation::Tagged => format!("Tagged {dst_label}"),
            Relation::References => format!("Cites {dst_label}"),
            Relation::Mentions => format!("Shares concept {dst_label}"),
            Relation::PartOf => format!("Part of {dst_label}"),
            Relation::Contains => format!("Contains {dst_label}"),
        };
        add_edge(src, dst, rel, conf, ge.weight, reason);
    }

    // 3. Cross-synapse memories <-> tags/concepts & direct memory <-> note links
    for rec in memories {
        let mem_ui_id = format!("mem:{}", rec.id);
        let mem_hay =
            format!("{} {} {}", rec.subject, rec.object, rec.raw_content).to_ascii_lowercase();
        for (bridge_id, bridge_label, term) in &tag_and_concept_terms {
            if term.len() >= 4 && mem_hay.contains(term.as_str()) {
                add_edge(
                    &mem_ui_id,
                    bridge_id,
                    "mentions",
                    "INFERRED",
                    0.68,
                    format!("References {bridge_label}"),
                );
            }
        }
    }

    // Cap noisy concept nodes: drop concepts with < 2 edges after cross-linking
    let mut temp_degrees: HashMap<String, usize> = HashMap::new();
    for e in &edges {
        *temp_degrees.entry(e.source.clone()).or_default() += 1;
        *temp_degrees.entry(e.target.clone()).or_default() += 1;
    }
    let keep_concepts_limit = 14usize;
    let mut concept_ids: Vec<(usize, String)> = nodes_map
        .values()
        .filter(|n| n.kind == "concept")
        .map(|n| (temp_degrees.get(&n.id).copied().unwrap_or(0), n.id.clone()))
        .collect();
    concept_ids.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let drop_concepts: BTreeSet<String> = concept_ids
        .into_iter()
        .enumerate()
        .filter(|(idx, (deg, _))| *deg < 2 || *idx >= keep_concepts_limit)
        .map(|(_, (_, id))| id)
        .collect();
    if !drop_concepts.is_empty() {
        for cid in &drop_concepts {
            nodes_map.remove(cid);
        }
        edges.retain(|e| !drop_concepts.contains(&e.source) && !drop_concepts.contains(&e.target));
    }

    // Recompute degrees cleanly
    let mut degrees: HashMap<String, usize> = HashMap::new();
    for e in &edges {
        *degrees.entry(e.source.clone()).or_default() += 1;
        *degrees.entry(e.target.clone()).or_default() += 1;
    }
    for node in nodes_map.values_mut() {
        node.degree = degrees.get(&node.id).copied().unwrap_or(0);
    }

    // Assign unclustered tag/concept nodes to the cluster of their strongest neighbor
    let adj_clusters: HashMap<String, usize> = edges
        .iter()
        .filter_map(|e| {
            let sc = nodes_map.get(&e.source).map(|n| n.cluster).unwrap_or(0);
            let tc = nodes_map.get(&e.target).map(|n| n.cluster).unwrap_or(0);
            if sc == 0 && tc > 0 {
                Some((e.source.clone(), tc))
            } else if tc == 0 && sc > 0 {
                Some((e.target.clone(), sc))
            } else {
                None
            }
        })
        .collect();
    for node in nodes_map.values_mut() {
        if node.cluster == 0 {
            if let Some(&c) = adj_clusters.get(&node.id) {
                node.cluster = c;
            }
        }
    }

    let mut cluster_counts: BTreeMap<usize, usize> = BTreeMap::new();
    for node in nodes_map.values() {
        if node.cluster > 0 {
            *cluster_counts.entry(node.cluster).or_default() += 1;
        }
    }
    let clusters: Vec<KnowledgeClusterDto> = cluster_labels
        .into_iter()
        .map(|(id, label)| KnowledgeClusterDto {
            id,
            label,
            node_count: cluster_counts.get(&id).copied().unwrap_or(0),
        })
        .collect();

    let nodes: Vec<KnowledgeNodeDto> = nodes_map.into_values().collect();
    let total_memories = nodes.iter().filter(|n| n.kind == "memory").count();
    let total_notes = nodes.iter().filter(|n| n.kind == "note").count();
    let total_hubs = nodes.iter().filter(|n| n.kind == "hub").count();
    let total_tags = nodes.iter().filter(|n| n.kind == "tag").count();
    let total_concepts = nodes.iter().filter(|n| n.kind == "concept").count();

    let memory_items: Vec<serde_json::Value> = memories
        .iter()
        .filter(|r| r.category != MemoryCategory::Transcript)
        .map(memory_to_json)
        .collect();
    let note_items: Vec<serde_json::Value> = notes
        .iter()
        .filter(|n| n.parent_id.is_none())
        .map(|n| note_to_json(n, false))
        .collect();

    KnowledgeGraphResponse {
        stats: KnowledgeStatsDto {
            total_memories,
            total_notes,
            total_hubs,
            total_tags,
            total_concepts,
            total_nodes: nodes.len(),
            total_edges: edges.len(),
        },
        nodes,
        edges,
        clusters,
        memories: memory_items,
        notes: note_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use harness_core::MemoryCategory;

    #[test]
    fn builds_unified_neuron_graph_linking_memories_and_notes() {
        let now = Utc::now();
        let memories = vec![
            MemoryRecord {
                id: "m1".to_string(),
                subject: "user.automation.stickman_videos".to_string(),
                predicate: "schedule".to_string(),
                object: "daily at 9pm research stickman videos and post to YouTube".to_string(),
                name: None,
                path: None,
                raw_content:
                    "user.automation.stickman_videos schedule daily at 9pm research stickman videos"
                        .to_string(),
                category: MemoryCategory::WorkflowRule,
                confidence: 1.0,
                created_at: now,
                updated_at: now,
                is_active: true,
                superseded_by: None,
            },
            MemoryRecord {
                id: "m2".to_string(),
                subject: "project.rust_agent_harness.profile".to_string(),
                predicate: "version".to_string(),
                object: "luna 0.1.9".to_string(),
                name: Some("rust-agent-harness".to_string()),
                path: Some("/home/janelle/Documents/GitHub/rust-agent-harness".to_string()),
                raw_content: "project.rust-agent-harness.profile version luna 0.1.9".to_string(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: now,
                updated_at: now,
                is_active: true,
                superseded_by: None,
            },
        ];

        let notes = vec![Note {
            id: "n1".to_string(),
            title: "Stickman Horror Format Teardown".to_string(),
            content: "# Teardown\nStickman videos using Kokoro TTS and YouTube pacing.".to_string(),
            summary_pointer: "Stickman horror video format notes".to_string(),
            tags: vec!["project:inbox".to_string(), "stickman".to_string()],
            created_at: now,
            updated_at: now,
            parent_id: None,
            part_number: None,
            part_count: None,
            content_hash: None,
        }];

        let graph = build_unified_knowledge_graph(&memories, &notes);
        assert_eq!(graph.stats.total_memories, 2);
        assert_eq!(graph.stats.total_notes, 1);
        assert!(graph.stats.total_hubs >= 2);
        assert!(graph.nodes.iter().any(|n| n.id == "tag:stickman"));
        assert!(graph
            .edges
            .iter()
            .any(|e| e.source == "mem:m1" && e.target == "tag:stickman"));
        assert!(graph
            .edges
            .iter()
            .any(|e| e.source == "note:n1" && e.target == "tag:stickman"));
    }
}
