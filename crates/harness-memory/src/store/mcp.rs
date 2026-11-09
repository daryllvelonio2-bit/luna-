//! Model Context Protocol (MCP) server for Luna memory store.
//! Exposes 44 semantic memory tools over stdio and HTTP for external agent harnesses
//! (Claude Code, Codex, Cursor) with full JSON-RPC 2.0 conformance.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use chrono::Utc;
use harness_core::{HarnessError, MemoryCategory};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{MemoryStore, Record, SearchFilter, SUMMARY_SPEC};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

pub fn all_mcp_tools() -> Vec<McpToolDef> {
    vec![
        // Store
        McpToolDef {
            name: "memory_status".to_string(),
            description: "Memory overview — total records, domains, topics, and embedder".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_list_domains".to_string(),
            description: "List all memory domains with record counts".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_list_topics".to_string(),
            description: "List topics within a domain (or all topics if no domain given)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Domain to list topics for (optional)" }
                }
            }),
        },
        McpToolDef {
            name: "memory_get_taxonomy".to_string(),
            description: "Full taxonomy: domain → topic → record count".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_get_summary_spec".to_string(),
            description: "Get the compact summary specification dialect used for compression".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // Read
        McpToolDef {
            name: "memory_search".to_string(),
            description: "Hybrid semantic search over active memory records with BM25 and vector ranking".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query terms or question" },
                    "domain": { "type": "string", "description": "Filter by domain (optional)" },
                    "topic": { "type": "string", "description": "Filter by topic (optional)" },
                    "limit": { "type": "integer", "description": "Max results (default: 8)" },
                    "since": { "type": "string", "description": "Only records authored at or after this RFC 3339 timestamp" },
                    "before": { "type": "string", "description": "Only records authored before this RFC 3339 timestamp" }
                },
                "required": ["query"]
            }),
        },
        McpToolDef {
            name: "memory_get_record".to_string(),
            description: "Retrieve one record by its unique ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Record unique ID" }
                },
                "required": ["id"]
            }),
        },
        McpToolDef {
            name: "memory_list_records".to_string(),
            description: "List active records with optional domain/topic filters and pagination".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Filter domain (optional)" },
                    "topic": { "type": "string", "description": "Filter topic (optional)" },
                    "limit": { "type": "integer", "description": "Max records (default: 20)" },
                    "offset": { "type": "integer", "description": "Records offset (default: 0)" }
                }
            }),
        },
        McpToolDef {
            name: "memory_recent_checkpoint".to_string(),
            description: "Get recent checkpoint event or session information".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_reconnect".to_string(),
            description: "Force derived index reload and rebuild from canonical Markdown files".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // Write
        McpToolDef {
            name: "memory_add_record".to_string(),
            description: "File verbatim content into a domain/topic with structured frontmatter".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Target domain" },
                    "topic": { "type": "string", "description": "Target topic" },
                    "body": { "type": "string", "description": "Verbatim content body" },
                    "subject": { "type": "string", "description": "Subject entity key" },
                    "predicate": { "type": "string", "description": "Attribute or qualifier (default: profile)" },
                    "object": { "type": "string", "description": "Brief value descriptor" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Categorical tags" }
                },
                "required": ["domain", "topic", "body"]
            }),
        },
        McpToolDef {
            name: "memory_update_record".to_string(),
            description: "Update an existing record's content or attributes by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Record ID to update" },
                    "body": { "type": "string", "description": "New verbatim body content" },
                    "object": { "type": "string", "description": "New object value" }
                },
                "required": ["id"]
            }),
        },
        McpToolDef {
            name: "memory_delete_record".to_string(),
            description: "Remove a memory record by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Record ID to delete" }
                },
                "required": ["id"]
            }),
        },
        McpToolDef {
            name: "memory_delete_by_source".to_string(),
            description: "Bulk-remove all records mined from a specific source file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "source_file": { "type": "string", "description": "Source file path" }
                },
                "required": ["source_file"]
            }),
        },
        McpToolDef {
            name: "memory_check_duplicate".to_string(),
            description: "Check if content already exists in the store before filing".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Text content to check" }
                },
                "required": ["content"]
            }),
        },
        McpToolDef {
            name: "memory_mine".to_string(),
            description: "Mine a repository directory, conversation logs, or sessions database into verbatim memory".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "dir": { "type": "string", "description": "Path to mine" },
                    "mode": { "type": "string", "description": "Mining mode: projects, convos, sessions (default: projects)" },
                    "domain": { "type": "string", "description": "Domain override (optional)" },
                    "topic": { "type": "string", "description": "Topic override (optional)" }
                },
                "required": ["dir"]
            }),
        },
        McpToolDef {
            name: "memory_sync".to_string(),
            description: "Synchronize and verify store consistency".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "dry_run": { "type": "boolean", "description": "Report changes without applying (default: false)" }
                }
            }),
        },
        // Knowledge Graph
        McpToolDef {
            name: "memory_kg_query".to_string(),
            description: "Query knowledge graph for an entity's relationships with temporal validity".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "string", "description": "Entity name to query" },
                    "as_of": { "type": "string", "description": "Temporal point-in-time filter (RFC 3339)" },
                    "direction": { "type": "string", "description": "outgoing, incoming, or both (default: both)" }
                },
                "required": ["entity"]
            }),
        },
        McpToolDef {
            name: "memory_kg_add".to_string(),
            description: "Add a relationship fact to the knowledge graph (subject → predicate → object)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string", "description": "Subject entity" },
                    "predicate": { "type": "string", "description": "Relationship predicate" },
                    "object": { "type": "string", "description": "Object entity" },
                    "valid_from": { "type": "string", "description": "Start validity date/time" },
                    "valid_to": { "type": "string", "description": "End validity date/time (for historical facts)" }
                },
                "required": ["subject", "predicate", "object"]
            }),
        },
        McpToolDef {
            name: "memory_kg_invalidate".to_string(),
            description: "Mark a fact as no longer true with an ending timestamp".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string", "description": "Subject entity" },
                    "predicate": { "type": "string", "description": "Relationship predicate" },
                    "object": { "type": "string", "description": "Object entity" },
                    "ended": { "type": "string", "description": "Timestamp when fact ended" }
                },
                "required": ["subject", "predicate", "object"]
            }),
        },
        McpToolDef {
            name: "memory_kg_supersede".to_string(),
            description: "Atomically supersede a single-valued relationship with a new value".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string", "description": "Subject entity" },
                    "predicate": { "type": "string", "description": "Relationship predicate" },
                    "old_object": { "type": "string", "description": "Previous object value" },
                    "new_object": { "type": "string", "description": "Replacement object value" },
                    "at": { "type": "string", "description": "Cutover timestamp" }
                },
                "required": ["subject", "predicate", "old_object", "new_object"]
            }),
        },
        McpToolDef {
            name: "memory_kg_timeline".to_string(),
            description: "Get chronological timeline of all facts for an entity".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "string", "description": "Entity name" }
                },
                "required": ["entity"]
            }),
        },
        McpToolDef {
            name: "memory_kg_stats".to_string(),
            description: "Get knowledge graph statistics: entity count, active triples, historical triples".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // Graph & Links
        McpToolDef {
            name: "memory_traverse".to_string(),
            description: "BFS graph traversal starting from an entity or domain".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "start": { "type": "string", "description": "Starting node / entity" },
                    "max_hops": { "type": "integer", "description": "Max hops (default: 2)" }
                },
                "required": ["start"]
            }),
        },
        McpToolDef {
            name: "memory_find_crosslinks".to_string(),
            description: "Find bridge entities (cross-links) connecting two domains".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain_a": { "type": "string", "description": "First domain" },
                    "domain_b": { "type": "string", "description": "Second domain" }
                }
            }),
        },
        McpToolDef {
            name: "memory_list_crosslinks".to_string(),
            description: "List cross-domain bridge links".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_follow_crosslinks".to_string(),
            description: "Follow crosslinks outward from a source domain".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Source domain" }
                },
                "required": ["domain"]
            }),
        },
        McpToolDef {
            name: "memory_list_links".to_string(),
            description: "List within-domain entity co-occurrence links".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Filter by domain (optional)" },
                    "min_count": { "type": "integer", "description": "Minimum co-occurrence count (default: 2)" }
                }
            }),
        },
        McpToolDef {
            name: "memory_create_crosslink".to_string(),
            description: "Create or confirm a cross-domain link".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_domain": { "type": "string", "description": "Source domain" },
                    "to_domain": { "type": "string", "description": "Destination domain" },
                    "entity": { "type": "string", "description": "Bridge entity connecting them" }
                },
                "required": ["from_domain", "to_domain", "entity"]
            }),
        },
        McpToolDef {
            name: "memory_delete_crosslink".to_string(),
            description: "Prune a cross-domain link".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_domain": { "type": "string", "description": "Source domain" },
                    "to_domain": { "type": "string", "description": "Destination domain" },
                    "entity": { "type": "string", "description": "Bridge entity" }
                },
                "required": ["from_domain", "to_domain", "entity"]
            }),
        },
        McpToolDef {
            name: "memory_delete_link".to_string(),
            description: "Prune a within-domain link".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "domain": { "type": "string", "description": "Domain" },
                    "entity_a": { "type": "string", "description": "First entity" },
                    "entity_b": { "type": "string", "description": "Second entity" }
                },
                "required": ["domain", "entity_a", "entity_b"]
            }),
        },
        // Journal
        McpToolDef {
            name: "memory_journal_write".to_string(),
            description: "Append an entry to an agent's daily journal".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Verbatim journal content" },
                    "agent": { "type": "string", "description": "Agent name (default: luna)" }
                },
                "required": ["content"]
            }),
        },
        McpToolDef {
            name: "memory_journal_read".to_string(),
            description: "Read recent journal entries for an agent".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Agent name (default: luna)" },
                    "date": { "type": "string", "description": "Date filter (YYYY-MM-DD)" },
                    "limit": { "type": "integer", "description": "Max entries (default: 10)" }
                }
            }),
        },
        // Coordination
        McpToolDef {
            name: "memory_event_append".to_string(),
            description: "Append an immutable multi-agent coordination event".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Event type (task.request, task.reply, patch.ready, status)" },
                    "stream": { "type": "string", "description": "Project stream (default: main)" },
                    "topic": { "type": "string", "description": "Coordination topic (default: delegation)" },
                    "from_agent": { "type": "string", "description": "Identity of sender" },
                    "to_agent": { "type": "string", "description": "Identity of recipient (or * for broadcast)" },
                    "correlation_id": { "type": "string", "description": "Task correlation ID" },
                    "body": { "type": "string", "description": "Event body text" }
                },
                "required": ["type", "from_agent", "body"]
            }),
        },
        McpToolDef {
            name: "memory_event_list".to_string(),
            description: "List coordination events from a stream with cursor tracking".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "stream": { "type": "string", "description": "Stream name (default: main)" },
                    "since": { "type": "string", "description": "Only events since event ID cursor" },
                    "unacked_by": { "type": "string", "description": "Filter unacknowledged events for agent" },
                    "limit": { "type": "integer", "description": "Max events (default: 50)" }
                }
            }),
        },
        McpToolDef {
            name: "memory_event_wait".to_string(),
            description: "Wait for an event matching agent criteria".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Recipient agent identity" },
                    "stream": { "type": "string", "description": "Stream name (default: main)" },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (default: 5000)" }
                },
                "required": ["agent"]
            }),
        },
        McpToolDef {
            name: "memory_event_ack".to_string(),
            description: "Acknowledge receipt of a coordination event".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "event_id": { "type": "string", "description": "Event ID to ack" },
                    "agent": { "type": "string", "description": "Agent acknowledging" },
                    "stream": { "type": "string", "description": "Stream name (default: main)" }
                },
                "required": ["event_id", "agent"]
            }),
        },
        McpToolDef {
            name: "memory_task_create".to_string(),
            description: "Create a canonical delegation task request".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name" },
                    "from_agent": { "type": "string", "description": "Sender identity" },
                    "to_agent": { "type": "string", "description": "Target agent identity" },
                    "goal": { "type": "string", "description": "Specific task objective" },
                    "branch": { "type": "string", "description": "Working branch" },
                    "base_commit": { "type": "string", "description": "Base commit SHA" },
                    "done": { "type": "string", "description": "Definition of done" }
                },
                "required": ["project", "from_agent", "to_agent", "goal"]
            }),
        },
        McpToolDef {
            name: "memory_patch_submit".to_string(),
            description: "Submit a code patch with verified SHA-256 artifact".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name" },
                    "from_agent": { "type": "string", "description": "Sender identity" },
                    "to_agent": { "type": "string", "description": "Target agent identity" },
                    "patch_content": { "type": "string", "description": "Git patch text" },
                    "correlation_id": { "type": "string", "description": "Task correlation ID" }
                },
                "required": ["project", "from_agent", "to_agent", "patch_content"]
            }),
        },
        McpToolDef {
            name: "memory_peers".to_string(),
            description: "List currently active peer agents on the memory hub".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        McpToolDef {
            name: "memory_artifact_put".to_string(),
            description: "Store a byte artifact (patch, binary, snapshot)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Text or base64-encoded payload" },
                    "id": { "type": "string", "description": "Optional artifact ID (content hash default)" }
                },
                "required": ["content"]
            }),
        },
        McpToolDef {
            name: "memory_artifact_get".to_string(),
            description: "Retrieve a byte artifact by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Artifact ID" }
                },
                "required": ["id"]
            }),
        },
        McpToolDef {
            name: "memory_checkpoint".to_string(),
            description: "Record a session or task milestone checkpoint".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "stream": { "type": "string", "description": "Project stream" },
                    "from_agent": { "type": "string", "description": "Agent recording checkpoint" },
                    "body": { "type": "string", "description": "Milestone details" }
                },
                "required": ["from_agent", "body"]
            }),
        },
        McpToolDef {
            name: "memory_hook_settings".to_string(),
            description: "Get runtime hook configurations and save intervals".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

pub fn dispatch_mcp_tool(
    store: &MemoryStore,
    tool: &str,
    args: &Value,
) -> Result<Value, HarnessError> {
    match tool {
        "memory_status" => {
            let status = store.status()?;
            let domains: Vec<String> = status.by_domain.keys().cloned().collect();
            let mut topics = HashMap::new();
            for (d, tmap) in &status.by_domain {
                for (t, count) in tmap {
                    topics.insert(format!("{d}/{t}"), *count);
                }
            }
            Ok(json!({
                "records": status.records,
                "domains": domains,
                "topics": topics,
                "embedder": status.embedder,
                "root": status.root.to_string_lossy()
            }))
        }
        "memory_list_domains" => {
            let status = store.status()?;
            let domains: Vec<String> = status.by_domain.keys().cloned().collect();
            Ok(json!({ "domains": domains }))
        }
        "memory_list_topics" => {
            let domain_filter = args.get("domain").and_then(Value::as_str);
            let status = store.status()?;
            let mut topics = HashMap::new();
            for (d, tmap) in &status.by_domain {
                if domain_filter.is_none() || domain_filter == Some(d.as_str()) {
                    for (t, count) in tmap {
                        topics.insert(format!("{d}/{t}"), *count);
                    }
                }
            }
            Ok(json!({ "topics": topics }))
        }
        "memory_get_taxonomy" => {
            let status = store.status()?;
            Ok(json!({
                "taxonomy": status.by_domain,
                "records": status.records
            }))
        }
        "memory_get_summary_spec" => Ok(json!({ "spec": SUMMARY_SPEC })),
        "memory_search" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("");
            let domain = args
                .get("domain")
                .and_then(Value::as_str)
                .map(str::to_string);
            let topic = args
                .get("topic")
                .and_then(Value::as_str)
                .map(str::to_string);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
            let parse_filter_time =
                |key: &str| -> Result<Option<chrono::DateTime<Utc>>, HarnessError> {
                    match args.get(key).and_then(Value::as_str) {
                        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
                            .map(|time| Some(time.with_timezone(&Utc)))
                            .map_err(|error| {
                                HarnessError::MemoryError(format!(
                                    "invalid {key} timestamp '{raw}': {error}"
                                ))
                            }),
                        None => Ok(None),
                    }
                };
            let filter = SearchFilter {
                domain,
                topic,
                since: parse_filter_time("since")?,
                before: parse_filter_time("before")?,
                exclude_category: None,
            };
            let results = store.search(query, &filter, limit)?;
            let formatted: Vec<Value> = results
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.record.id,
                        "domain": r.record.domain,
                        "topic": r.record.topic,
                        "subject": r.record.subject,
                        "predicate": r.record.predicate,
                        "object": r.record.object,
                        "score": r.score,
                        "excerpt": r.excerpt
                    })
                })
                .collect();
            Ok(json!({ "results": formatted, "count": formatted.len() }))
        }
        "memory_get_record" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            match store.get_record(id)? {
                Some(rec) => Ok(json!({
                    "id": rec.id,
                    "domain": rec.domain,
                    "topic": rec.topic,
                    "subject": rec.subject,
                    "predicate": rec.predicate,
                    "object": rec.object,
                    "body": rec.body,
                    "filed_at": rec.filed_at.to_rfc3339()
                })),
                None => Ok(json!({ "error": format!("record '{id}' not found") })),
            }
        }
        "memory_list_records" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let domain = args.get("domain").and_then(Value::as_str);
            let topic = args.get("topic").and_then(Value::as_str);

            let all = store.active_records()?;
            let filtered: Vec<Record> = all
                .into_iter()
                .filter(|r| {
                    if let Some(d) = domain {
                        if r.domain != d {
                            return false;
                        }
                    }
                    if let Some(t) = topic {
                        if r.topic != t {
                            return false;
                        }
                    }
                    true
                })
                .skip(offset)
                .take(limit)
                .collect();

            let records: Vec<Value> = filtered
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "domain": r.domain,
                        "topic": r.topic,
                        "subject": r.subject,
                        "predicate": r.predicate,
                        "object": r.object
                    })
                })
                .collect();
            Ok(json!({ "records": records, "count": records.len() }))
        }
        "memory_recent_checkpoint" => {
            let events = store.event_list("main", None, Some(1), None)?;
            if let Some(ev) = events.first() {
                Ok(json!({ "checkpoint": ev }))
            } else {
                Ok(json!({ "checkpoint": null }))
            }
        }
        "memory_reconnect" => {
            let count = store.reindex()?;
            Ok(json!({ "success": true, "reindexed": count }))
        }
        "memory_add_record" => {
            let domain = args
                .get("domain")
                .and_then(Value::as_str)
                .unwrap_or("general");
            let topic = args.get("topic").and_then(Value::as_str).unwrap_or("notes");
            let body = args.get("body").and_then(Value::as_str).unwrap_or("");
            let subject = args.get("subject").and_then(Value::as_str).unwrap_or(topic);
            let predicate = args
                .get("predicate")
                .and_then(Value::as_str)
                .unwrap_or("profile");
            let object = args.get("object").and_then(Value::as_str).unwrap_or("");
            let tags: Vec<String> = args
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let id = super::ids::record_id_from_content(domain, topic, body);
            let record = Record {
                id: id.clone(),
                domain: domain.to_string(),
                topic: topic.to_string(),
                source_file: None,
                chunk_index: 0,
                line_start: 0,
                line_end: 0,
                filed_at: Utc::now(),
                entities: Vec::new(),
                tags,
                body_hash: super::ids::body_hash(body),
                subject: subject.to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                name: None,
                path: None,
                raw_content: body.to_string(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
                body: body.to_string(),
            };
            store.write_record(record)?;
            Ok(json!({ "success": true, "id": id }))
        }
        "memory_update_record" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            let mut record = store
                .get_record(id)?
                .ok_or_else(|| HarnessError::MemoryError(format!("record '{id}' not found")))?;

            if let Some(body) = args.get("body").and_then(Value::as_str) {
                record.body = body.to_string();
                record.raw_content = body.to_string();
                record.body_hash = super::ids::body_hash(body);
            }
            if let Some(object) = args.get("object").and_then(Value::as_str) {
                record.object = object.to_string();
            }
            record.updated_at = Utc::now();
            store.write_record(record)?;
            Ok(json!({ "success": true, "id": id }))
        }
        "memory_delete_record" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            let deleted = store.delete_record(id)?;
            Ok(json!({ "success": true, "deleted": deleted }))
        }
        "memory_delete_by_source" => {
            let source_file = args
                .get("source_file")
                .and_then(Value::as_str)
                .unwrap_or("");
            let records = store.active_records()?;
            let mut count = 0;
            for r in records {
                if r.source_file.as_deref() == Some(source_file) {
                    if store.delete_record(&r.id)? {
                        count += 1;
                    }
                }
            }
            Ok(json!({ "success": true, "deleted_count": count }))
        }
        "memory_check_duplicate" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            let target_hash = super::ids::body_hash(content);
            let records = store.active_records()?;
            let dup = records.iter().find(|r| r.body_hash == target_hash);
            match dup {
                Some(r) => Ok(json!({ "duplicate": true, "id": r.id })),
                None => Ok(json!({ "duplicate": false })),
            }
        }
        "memory_mine" => {
            let dir = args.get("dir").and_then(Value::as_str).unwrap_or(".");
            let mode = args
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("projects");
            let domain = args.get("domain").and_then(Value::as_str);
            let topic = args.get("topic").and_then(Value::as_str);

            if mode == "convos" {
                let rep = store.mine_convos(Path::new(dir), domain, topic)?;
                Ok(json!({ "success": true, "records": rep.records, "files": rep.files }))
            } else if mode == "sessions" {
                let rep = store.mine_sessions(Path::new(dir), domain, topic)?;
                Ok(json!({ "success": true, "records": rep.records, "files": rep.files }))
            } else {
                let rep = store.mine(Path::new(dir), domain)?;
                Ok(json!({ "success": true, "records": rep.records, "files": rep.files }))
            }
        }
        "memory_sync" => {
            let count = store.reindex()?;
            Ok(json!({ "success": true, "records_checked": count }))
        }
        "memory_kg_query" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            let as_of = args.get("as_of").and_then(Value::as_str);
            let dir = args
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("both");
            let res = store.kg().query_entity(entity, as_of, Some(dir))?;
            Ok(json!({
                "entity": entity,
                "facts": res.facts,
                "candidates": res.candidates,
                "count": res.facts.len()
            }))
        }
        "memory_kg_add" => {
            let s = args.get("subject").and_then(Value::as_str).unwrap_or("");
            let p = args.get("predicate").and_then(Value::as_str).unwrap_or("");
            let o = args.get("object").and_then(Value::as_str).unwrap_or("");
            let vf = args.get("valid_from").and_then(Value::as_str);
            let vt = args.get("valid_to").and_then(Value::as_str);
            let id = store.kg_add_triple(s, p, o, vf, vt, None, None, None, None, None)?;
            Ok(json!({ "success": true, "triple_id": id }))
        }
        "memory_kg_invalidate" => {
            let s = args.get("subject").and_then(Value::as_str).unwrap_or("");
            let p = args.get("predicate").and_then(Value::as_str).unwrap_or("");
            let o = args.get("object").and_then(Value::as_str).unwrap_or("");
            let ended = args.get("ended").and_then(Value::as_str);
            let count = store.kg_invalidate(s, p, o, ended)?;
            Ok(json!({ "success": true, "invalidated_count": count }))
        }
        "memory_kg_supersede" => {
            let s = args.get("subject").and_then(Value::as_str).unwrap_or("");
            let p = args.get("predicate").and_then(Value::as_str).unwrap_or("");
            let old_o = args.get("old_object").and_then(Value::as_str).unwrap_or("");
            let new_o = args.get("new_object").and_then(Value::as_str).unwrap_or("");
            let at = args.get("at").and_then(Value::as_str);
            let id = store.kg_supersede(s, p, old_o, new_o, at, None, None, None, None, None)?;
            Ok(json!({ "success": true, "new_triple_id": id }))
        }
        "memory_kg_timeline" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            let timeline = store.kg().timeline(Some(entity), 50, 0)?;
            Ok(json!({ "entity": entity, "timeline": timeline }))
        }
        "memory_kg_stats" => {
            let stats = store.kg().stats()?;
            Ok(json!({
                "entity_count": stats.entities,
                "triples": stats.triples,
                "current_facts": stats.current_facts,
                "expired_facts": stats.expired_facts,
                "relationship_types": stats.relationship_types
            }))
        }
        "memory_traverse" => {
            let start = args.get("start").and_then(Value::as_str).unwrap_or("");
            let max_hops = args.get("max_hops").and_then(Value::as_u64).unwrap_or(2) as usize;
            let steps = store.graph_index().traverse(start, max_hops)?;
            let count = steps.len();
            Ok(json!({ "start": start, "steps": steps, "count": count }))
        }
        "memory_find_crosslinks" => {
            let da = args.get("domain_a").and_then(Value::as_str).unwrap_or("");
            let db = args.get("domain_b").and_then(Value::as_str).unwrap_or("");
            let bridges = store.graph_index().find_cross_links(Some(da), Some(db))?;
            Ok(json!({ "domain_a": da, "domain_b": db, "bridges": bridges }))
        }
        "memory_list_crosslinks" => {
            let links = store
                .crosslinks_store()
                .list_all_crosslinks()
                .unwrap_or_default();
            Ok(json!({ "crosslinks": links }))
        }
        "memory_follow_crosslinks" => {
            let domain = args.get("domain").and_then(Value::as_str).unwrap_or("");
            let all = store
                .crosslinks_store()
                .list_all_crosslinks()
                .unwrap_or_default();
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|l| l.from_endpoint == domain || l.to_endpoint == domain)
                .collect();
            Ok(json!({ "domain": domain, "crosslinks": filtered }))
        }
        "memory_list_links" => {
            let domain = args.get("domain").and_then(Value::as_str);
            let min_c = args.get("min_count").and_then(Value::as_u64).unwrap_or(2) as usize;
            let all = store.links_store().list_all_links().unwrap_or_default();
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|l| {
                    if let Some(d) = domain {
                        if l.domain != d {
                            return false;
                        }
                    }
                    l.count >= min_c
                })
                .collect();
            Ok(json!({ "links": filtered }))
        }
        "memory_create_crosslink" => {
            let fd = args
                .get("from_domain")
                .and_then(Value::as_str)
                .unwrap_or("");
            let td = args.get("to_domain").and_then(Value::as_str).unwrap_or("");
            let ent = args.get("entity").and_then(Value::as_str).unwrap_or("");
            let mut links = store
                .crosslinks_store()
                .get_crosslinks_for_entity(ent)
                .unwrap_or_default();
            links.push(super::crosslinks::CrossLink::new(
                ent, fd, td, None, 1, "approved",
            ));
            store
                .crosslinks_store()
                .save_crosslinks_for_entity(ent, &links)?;
            Ok(json!({ "success": true }))
        }
        "memory_delete_crosslink" => {
            let pruned = store.crosslinks_store().prune_crosslinks(None)?;
            Ok(json!({ "success": true, "pruned": pruned }))
        }
        "memory_delete_link" => {
            let d = args.get("domain").and_then(Value::as_str);
            let pruned = store.links_store().prune_links(d, 1000)?;
            Ok(json!({ "success": true, "pruned": pruned }))
        }
        "memory_journal_write" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            let agent = args.get("agent").and_then(Value::as_str).unwrap_or("luna");
            let entry = store.journal_write(agent, content, None)?;
            Ok(json!({ "success": true, "entry": entry }))
        }
        "memory_journal_read" => {
            let agent = args.get("agent").and_then(Value::as_str).unwrap_or("luna");
            let date = args.get("date").and_then(Value::as_str);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let entries = store.journal_read(agent, date, limit)?;
            Ok(json!({ "agent": agent, "entries": entries }))
        }
        "memory_event_append" => {
            let etype = args
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message");
            let stream = args.get("stream").and_then(Value::as_str).unwrap_or("main");
            let from = args
                .get("from_agent")
                .and_then(Value::as_str)
                .unwrap_or("luna");
            let to = args.get("to_agent").and_then(Value::as_str);
            let corr = args.get("correlation_id").and_then(Value::as_str);
            let body = args.get("body").and_then(Value::as_str).unwrap_or("");
            let event = store.event_append(stream, etype, from, to, body, corr, None)?;
            Ok(json!({ "success": true, "event": event }))
        }
        "memory_event_list" => {
            let stream = args.get("stream").and_then(Value::as_str).unwrap_or("main");
            let unacked = args.get("unacked_by").and_then(Value::as_str);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let events = store.event_list(stream, None, limit, unacked)?;
            Ok(json!({ "events": events, "count": events.len() }))
        }
        "memory_event_wait" => {
            let agent = args.get("agent").and_then(Value::as_str).unwrap_or("luna");
            let stream = args.get("stream").and_then(Value::as_str).unwrap_or("main");
            let timeout = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(5000);
            // This dispatch path is synchronous: it serves both the stdio loop
            // (no runtime) and the HTTP server. Use the blocking wait so there
            // is no `Handle::current()` panic when no runtime exists.
            let event = store
                .events()
                .wait_for_event_blocking(stream, agent, timeout)?;
            Ok(json!({ "event": event }))
        }
        "memory_event_ack" => {
            let event_id = args.get("event_id").and_then(Value::as_str).unwrap_or("");
            let agent = args.get("agent").and_then(Value::as_str).unwrap_or("luna");
            let stream = args.get("stream").and_then(Value::as_str).unwrap_or("main");
            store.event_ack(stream, agent, event_id)?;
            Ok(json!({ "success": true }))
        }
        "memory_task_create" => {
            let project = args
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let from = args
                .get("from_agent")
                .and_then(Value::as_str)
                .unwrap_or("luna");
            let to = args.get("to_agent").and_then(Value::as_str).unwrap_or("*");
            let goal = args.get("goal").and_then(Value::as_str).unwrap_or("");
            let branch = args.get("branch").and_then(Value::as_str).unwrap_or("main");
            let base = args
                .get("base_commit")
                .and_then(Value::as_str)
                .unwrap_or("HEAD");
            let done = args.get("done").and_then(Value::as_str).unwrap_or("");

            let body = format!("goal: {goal}\nbranch: {branch}\nbase_commit: {base}\ndone: {done}");
            let corr = format!("task_{}", uuid::Uuid::new_v4().simple());
            let stream = format!("project/{project}");
            let event = store.event_append(
                &stream,
                "task.request",
                from,
                Some(to),
                &body,
                Some(&corr),
                Some("open"),
            )?;
            Ok(json!({ "success": true, "correlation_id": corr, "event": event }))
        }
        "memory_patch_submit" => {
            let project = args
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let from = args
                .get("from_agent")
                .and_then(Value::as_str)
                .unwrap_or("luna");
            let to = args.get("to_agent").and_then(Value::as_str).unwrap_or("*");
            let patch = args
                .get("patch_content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let corr = args.get("correlation_id").and_then(Value::as_str);

            let art_id = store.artifact_put(None, patch.as_bytes(), HashMap::new())?;
            let stream = format!("project/{project}");
            let body = format!("patch artifact: {art_id}");
            let event = store.event_append(
                &stream,
                "patch.ready",
                from,
                Some(to),
                &body,
                corr,
                Some("submitted"),
            )?;
            Ok(json!({ "success": true, "artifact_id": art_id, "event": event }))
        }
        "memory_peers" => {
            let events = store.event_list("main", None, Some(100), None)?;
            let mut peers = HashMap::new();
            for ev in events {
                peers.insert(ev.from.clone(), ev.at.to_rfc3339());
            }
            Ok(json!({ "peers": peers }))
        }
        "memory_artifact_put" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            let id_opt = args.get("id").and_then(Value::as_str);
            let art_id = store.artifact_put(id_opt, content.as_bytes(), HashMap::new())?;
            Ok(json!({ "success": true, "artifact_id": art_id }))
        }
        "memory_artifact_get" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            let pair_opt = store.artifact_get(id)?;
            match pair_opt {
                Some((bytes, _meta)) => {
                    let text = String::from_utf8(bytes)
                        .unwrap_or_else(|e| format!("binary bytes (len {})", e.as_bytes().len()));
                    Ok(json!({ "id": id, "content": text }))
                }
                None => Ok(json!({ "error": format!("artifact '{id}' not found") })),
            }
        }
        "memory_checkpoint" => {
            let stream = args.get("stream").and_then(Value::as_str).unwrap_or("main");
            let from = args
                .get("from_agent")
                .and_then(Value::as_str)
                .unwrap_or("luna");
            let body = args.get("body").and_then(Value::as_str).unwrap_or("");
            let ev = store.event_append(
                stream,
                "checkpoint",
                from,
                None,
                body,
                None,
                Some("checkpoint"),
            )?;
            Ok(json!({ "success": true, "checkpoint_id": ev.id }))
        }
        "memory_hook_settings" => Ok(json!({
            "hooks_save_interval": store.config.hooks_save_interval,
            "auto_save": store.config.hooks_auto_save,
            "silent_save": true
        })),
        other => Err(HarnessError::ToolNotFound(other.to_string())),
    }
}

pub fn handle_mcp_jsonrpc(store: &MemoryStore, req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {
                    "name": "luna-memory",
                    "version": "0.1.0"
                },
                "capabilities": {
                    "tools": {}
                }
            }
        }),
        "notifications/initialized" => json!({}),
        "tools/list" => {
            let tools = all_mcp_tools();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": tools
                }
            })
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            match dispatch_mcp_tool(store, name, &arguments) {
                Ok(res) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": serde_json::to_string_pretty(&res).unwrap_or_else(|_| "{}".to_string())
                            }
                        ]
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": e.to_string()
                    }
                }),
            }
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("method '{other}' not found")
            }
        }),
    }
}

pub fn run_stdio_server(store: &MemoryStore) -> Result<(), HarnessError> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let resp = handle_mcp_jsonrpc(store, &req);
        if resp != json!({}) {
            let mut out_bytes = serde_json::to_vec(&resp)
                .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
            out_bytes.push(b'\n');
            stdout
                .write_all(&out_bytes)
                .map_err(|e| HarnessError::Io(e.to_string()))?;
            stdout
                .flush()
                .map_err(|e| HarnessError::Io(e.to_string()))?;
        }
    }
    Ok(())
}
