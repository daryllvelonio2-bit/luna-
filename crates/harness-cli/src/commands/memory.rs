//! `luna memory` — inspect and maintain the Markdown memory store.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use harness_core::config::Config;
use harness_core::HarnessError;
use harness_memory::{store_config, MemoryStore, SearchFilter};

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Create the store layout and identity file
    Init,
    /// Show record counts, taxonomy, root, and embedder
    Status {
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// Mine a directory tree into verbatim records
    Mine {
        /// Directory to mine (default: the workspace root)
        dir: Option<PathBuf>,
        /// Domain override
        #[arg(long)]
        domain: Option<String>,
    },
    /// Hybrid search over active records
    Search {
        /// Keywords or a short question
        query: String,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        topic: Option<String>,
        /// Maximum results (default 8)
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Only records updated since an RFC 3339 timestamp
        #[arg(long)]
        since: Option<String>,
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// Print one record by id, subject, or subject.predicate
    Show {
        id: String,
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// Print the L0 identity plus a bounded L1 recent-context block
    Context {
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// Rebuild the derived search cache and manifest from Markdown
    Reindex,
    /// Report structural problems: invalid front matter, duplicates, dangling links
    Audit {
        #[arg(long)]
        json: bool,
    },
    /// Copy the canonical Markdown tree to another directory
    Export {
        /// Destination directory
        out: PathBuf,
    },
    /// Import rows from the legacy SQLite memory database
    Migrate {
        /// Source database (default: paths.memory_db)
        #[arg(long)]
        from_memory_db: Option<PathBuf>,
    },
    /// Delete one record, or every record with `all`
    Forget { id: String },
    /// List the domains and topics with counts
    Domains,
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

pub fn handle_memory_command(command: MemoryCommand, config: &Config) -> Result<(), HarnessError> {
    let store = MemoryStore::open(store_config(config))?;
    match command {
        MemoryCommand::Init => {
            println!("memory store ready at {}", store.root().display());
        }
        MemoryCommand::Status { json } => {
            let status = store.status()?;
            if json {
                let domains: serde_json::Value = status
                    .by_domain
                    .iter()
                    .map(|(domain, topics)| {
                        (
                            domain.clone(),
                            serde_json::Value::Object(
                                topics
                                    .iter()
                                    .map(|(topic, count)| {
                                        (topic.clone(), serde_json::json!(count))
                                    })
                                    .collect(),
                            ),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "root": status.root,
                        "embedder": status.embedder,
                        "records": status.records,
                        "domains": domains,
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("root:     {}", status.root.display());
                println!("embedder: {}", status.embedder);
                println!("records:  {}", status.records);
                for (domain, topics) in &status.by_domain {
                    println!("  {domain}");
                    for (topic, count) in topics {
                        println!("    {topic}: {count}");
                    }
                }
            }
        }
        MemoryCommand::Mine { dir, domain } => {
            let dir = dir.unwrap_or_else(|| {
                config
                    .paths
                    .workspace_root
                    .clone()
            });
            let report = store.mine(&dir, domain.as_deref())?;
            println!(
                "mined {} file(s): {} record(s), {} removed, {} skipped",
                report.files, report.records, report.removed, report.skipped
            );
        }
        MemoryCommand::Search {
            query,
            domain,
            topic,
            limit,
            since,
            json,
        } => {
            let since = since
                .as_deref()
                .map(parse_time)
                .transpose()?;
            let filter = SearchFilter {
                domain,
                topic,
                since,
                before: None,
            };
            let results = store.search(&query, &filter, limit)?;
            if json {
                let values: Vec<serde_json::Value> = results
                    .iter()
                    .map(|result| {
                        serde_json::json!({
                            "id": result.record.id,
                            "domain": result.record.domain,
                            "topic": result.record.topic,
                            "source_file": result.record.source_file,
                            "line_start": result.record.line_start,
                            "line_end": result.record.line_end,
                            "score": result.score,
                            "vector_score": result.vector,
                            "bm25_score": result.lexical,
                            "excerpt": result.excerpt,
                            "path": result.path,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "query": query,
                        "results": values,
                        "truncated": false,
                    }))
                    .unwrap_or_default()
                );
            } else if results.is_empty() {
                println!("no matches");
            } else {
                for (rank, result) in results.iter().enumerate() {
                    let span = if result.record.line_start > 0 {
                        format!(
                            "L{}-L{}",
                            result.record.line_start, result.record.line_end
                        )
                    } else {
                        "—".to_string()
                    };
                    let source = result
                        .record
                        .source_file
                        .as_deref()
                        .unwrap_or(&result.record.id);
                    println!(
                        "{}. {} [{:.3}] {}/{} {}{}",
                        rank + 1,
                        result.record.object,
                        result.score,
                        result.record.domain,
                        result.record.topic,
                        source,
                        span
                    );
                    if !result.excerpt.is_empty() {
                        println!("   {}", result.excerpt.replace('\n', " "));
                    }
                }
            }
        }
        MemoryCommand::Show { id, json } => match store.get_record(&id)? {
            Some(record) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "id": record.id,
                            "domain": record.domain,
                            "topic": record.topic,
                            "subject": record.subject,
                            "predicate": record.predicate,
                            "object": record.object,
                            "category": record.category.to_string(),
                            "confidence": record.confidence,
                            "is_active": record.is_active,
                            "superseded_by": record.superseded_by,
                            "source_file": record.source_file,
                            "line_start": record.line_start,
                            "line_end": record.line_end,
                            "updated_at": record.updated_at,
                            "body": record.body,
                        }))
                        .unwrap_or_default()
                    );
                } else {
                    println!("id:        {}", record.id);
                    println!("domain:    {}", record.domain);
                    println!("topic:     {}", record.topic);
                    println!("category:  {}", record.category);
                    println!("active:    {}", record.is_active);
                    if let Some(source) = &record.source_file {
                        println!("source:    {source}");
                    }
                    println!();
                    println!("{}", record.body);
                }
            }
            None => println!("no record matching '{id}'"),
        },
        MemoryCommand::Context { limit } => {
            print!("{}", store.context_block(limit)?);
        }
        MemoryCommand::Reindex => {
            let count = store.reindex()?;
            println!("reindexed {count} record(s) from Markdown");
        }
        MemoryCommand::Audit { json } => {
            let report = store.audit()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "records": report.records,
                        "inactive": report.inactive,
                        "invalid": report.invalid,
                        "unreadable": report.unreadable,
                        "empty_bodies": report.empty_bodies,
                        "dangling_supersessions": report.dangling_supersessions,
                        "duplicate_active": report.duplicate_active,
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("records:   {}", report.records);
                println!("inactive:  {}", report.inactive);
                print_issues("invalid front matter", &report.invalid);
                print_issues("unreadable", &report.unreadable);
                print_issues("empty bodies", &report.empty_bodies);
                print_issues("dangling supersessions", &report.dangling_supersessions);
                print_issues("duplicate active", &report.duplicate_active);
                if report.invalid.is_empty()
                    && report.unreadable.is_empty()
                    && report.empty_bodies.is_empty()
                    && report.dangling_supersessions.is_empty()
                    && report.duplicate_active.is_empty()
                {
                    println!("clean");
                }
            }
        }
        MemoryCommand::Export { out } => {
            let count = store.export(&out)?;
            println!("exported {count} record(s) to {}", out.display());
        }
        MemoryCommand::Migrate { from_memory_db } => {
            let source = from_memory_db.unwrap_or_else(|| config.paths.memory_db.clone());
            let count = store.migrate_from_sqlite(&source)?;
            println!("imported {count} record(s) from {}", source.display());
        }
        MemoryCommand::Forget { id } => {
            if id.eq_ignore_ascii_case("all") {
                let count = store.delete_all_records()?;
                println!("deleted {count} record(s)");
            } else if store.delete_record(&id)? {
                println!("deleted {id}");
            } else {
                println!("no record matching '{id}'");
            }
        }
        MemoryCommand::Domains => {
            let status = store.status()?;
            for (domain, topics) in &status.by_domain {
                println!("{domain}");
                for (topic, count) in topics {
                    println!("  {topic} ({count})");
                }
            }
        }
    }
    Ok(())
}

fn parse_time(text: &str) -> Result<chrono::DateTime<chrono::Utc>, HarnessError> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&chrono::Utc))
        .map_err(|error| HarnessError::ConfigurationError(format!("invalid timestamp '{text}': {error}")))
}

fn print_issues(label: &str, issues: &[String]) {
    if issues.is_empty() {
        return;
    }
    println!("{label}:");
    for issue in issues {
        println!("  - {issue}");
    }
}
