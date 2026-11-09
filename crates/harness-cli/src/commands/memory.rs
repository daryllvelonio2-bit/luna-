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
        /// Directory or file to mine (default: workspace root or sessions_db)
        dir: Option<PathBuf>,
        /// Mining mode: projects, convos, sessions
        #[arg(long, default_value = "projects")]
        mode: String,
        /// Domain override
        #[arg(long)]
        domain: Option<String>,
        /// Topic override
        #[arg(long)]
        topic: Option<String>,
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
        /// Only records authored since an RFC 3339 timestamp
        #[arg(long)]
        since: Option<String>,
        /// Only records authored before an RFC 3339 timestamp
        #[arg(long)]
        before: Option<String>,
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
    /// Knowledge graph operations: query, add, invalidate, supersede, timeline, stats
    #[command(subcommand)]
    Kg(KgCommand),
    /// Within-domain entity co-occurrence links
    Links {
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        prune: bool,
        #[arg(long, default_value_t = 2)]
        min_count: usize,
        #[arg(long)]
        json: bool,
    },
    /// Cross-domain links: propose, apply, prune, list
    #[command(subcommand)]
    Crosslinks(CrosslinksCommand),
    /// Graph traversal using BFS over domain topologies and bridges
    Traverse {
        start: String,
        #[arg(long, default_value_t = 2)]
        max_hops: usize,
        #[arg(long)]
        json: bool,
    },
    /// Find bridge entities (cross-links) connecting two domains
    Bridges {
        #[arg(long)]
        domain_a: Option<String>,
        #[arg(long)]
        domain_b: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Daily append-only agent journal
    #[command(subcommand)]
    Journal(JournalCommand),
    /// Multi-agent coordination event log
    #[command(subcommand)]
    Events(EventsCommand),
    /// Byte-exact artifacts
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    /// Long-lived background writer daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Run Model Context Protocol server (stdio or HTTP)
    Mcp {
        #[arg(long)]
        stdio: bool,
        #[arg(long)]
        http: bool,
        #[arg(long, default_value_t = 9876)]
        port: u16,
    },
    /// Run shared HTTP server
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 9876)]
        port: u16,
        #[arg(long)]
        tls_cert: Option<PathBuf>,
        #[arg(long)]
        tls_key: Option<PathBuf>,
    },
    /// Semantic lifecycle hooks for session management
    #[command(subcommand)]
    Hook(HookCommand),
    /// Output skill instructions to stdout
    Instructions { name: Option<String> },
    /// Render shared-brain rules block for an agent identity
    Rules {
        #[arg(long, default_value = "local")]
        host: String,
        #[arg(long, default_value = "claude-code")]
        harness: String,
        #[arg(long, default_value = "luna")]
        project: String,
        #[arg(long, default_value = "full")]
        mcp: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start daemon in foreground or background
    Start {
        #[arg(long, default_value_t = 9876)]
        port: u16,
        #[arg(long)]
        bg: bool,
    },
    /// Stop running daemon
    Stop,
    /// Report daemon health and counts
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List jobs in the daemon queue
    Jobs {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Wait for a job to complete
    Wait {
        job_id: String,
        #[arg(long, default_value_t = 60)]
        timeout_s: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Run hook logic (reads JSON from stdin, writes JSON to stdout)
    Run {
        event: String,
        #[arg(long, default_value = "claude-code")]
        harness: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum JournalCommand {
    /// Append an entry to an agent's journal
    Write {
        content: String,
        #[arg(long, default_value = "luna")]
        agent: String,
    },
    /// Read journal entries for an agent
    Read {
        #[arg(long, default_value = "luna")]
        agent: String,
        #[arg(long)]
        date: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum EventsCommand {
    /// Append a coordination event
    Append {
        body: String,
        #[arg(long, default_value = "main")]
        stream: String,
        #[arg(long = "type", default_value = "delegation")]
        event_type: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        corr: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// List events from a stream
    List {
        #[arg(long, default_value = "main")]
        stream: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        unacked_by: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Mark an event as acknowledged
    Ack {
        event_id: String,
        #[arg(long)]
        agent: String,
        #[arg(long, default_value = "main")]
        stream: String,
    },
    /// Wait for an event
    Wait {
        agent: String,
        #[arg(long, default_value = "main")]
        stream: String,
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// Store a byte artifact
    Put {
        file: PathBuf,
        #[arg(long)]
        id: Option<String>,
    },
    /// Retrieve an artifact
    Get {
        id: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum KgCommand {
    /// Query knowledge graph for an entity's relationships
    Query {
        entity: String,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long, default_value = "both")]
        direction: String,
        #[arg(long)]
        json: bool,
    },
    /// Add a relationship triple
    Add {
        subject: String,
        predicate: String,
        object: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        conf: Option<f32>,
        #[arg(long)]
        src: Option<String>,
        #[arg(long)]
        file: Option<String>,
    },
    /// Invalidate an active fact
    Invalidate {
        subject: String,
        predicate: String,
        object: String,
        #[arg(long)]
        ended: Option<String>,
    },
    /// Atomically supersede an active fact with a replacement
    Supersede {
        subject: String,
        predicate: String,
        old_obj: String,
        new_obj: String,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        conf: Option<f32>,
    },
    /// Chronological timeline of facts
    Timeline {
        entity: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        json: bool,
    },
    /// Statistics for knowledge graph
    Stats {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum CrosslinksCommand {
    /// Propose cross-domain links based on shared entities
    Propose {
        #[arg(long, default_value_t = 60)]
        max: usize,
        #[arg(long)]
        json: bool,
    },
    /// Apply approved cross-links into canonical storage
    Apply {
        #[arg(long)]
        json: bool,
    },
    /// Prune generic tokens and dangling cross-links
    Prune {
        #[arg(long)]
        json: bool,
    },
    /// List cross-links
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

pub async fn handle_memory_command(
    command: MemoryCommand,
    config: &Config,
) -> Result<(), HarnessError> {
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
                                    .map(|(topic, count)| (topic.clone(), serde_json::json!(count)))
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
        MemoryCommand::Mine {
            dir,
            mode,
            domain,
            topic,
        } => {
            let report = match mode.to_lowercase().as_str() {
                "sessions" => {
                    let db_path = dir.unwrap_or_else(|| config.paths.sessions_db.clone());
                    store.mine_sessions(&db_path, domain.as_deref(), topic.as_deref())?
                }
                "convos" => {
                    let path = dir.unwrap_or_else(|| config.paths.workspace_root.clone());
                    store.mine_convos(&path, domain.as_deref(), topic.as_deref())?
                }
                _ => {
                    let dir = dir.unwrap_or_else(|| config.paths.workspace_root.clone());
                    store.mine(&dir, domain.as_deref())?
                }
            };
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
            before,
            json,
        } => {
            let since = since.as_deref().map(parse_time).transpose()?;
            let before = before.as_deref().map(parse_time).transpose()?;
            let filter = SearchFilter {
                domain,
                topic,
                since,
                before,
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
                            "index_boost": result.index_boost,
                            "matched_via": result.matched_via,
                            "index_preview": result.index_preview,
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
                        format!("L{}-L{}", result.record.line_start, result.record.line_end)
                    } else {
                        "—".to_string()
                    };
                    let source = result
                        .record
                        .source_file
                        .as_deref()
                        .unwrap_or(&result.record.id);
                    let via = if result.index_boost > 0.0 {
                        format!(" [boost +{:.2}]", result.index_boost)
                    } else {
                        String::new()
                    };
                    println!(
                        "{}. {} [{:.3}]{} {}/{} {}{}",
                        rank + 1,
                        result.record.object,
                        result.score,
                        via,
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
        MemoryCommand::Kg(kg_cmd) => match kg_cmd {
            KgCommand::Query {
                entity,
                as_of,
                direction,
                json,
            } => {
                let res = store.kg_query(&entity, as_of.as_deref(), Some(&direction))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
                } else {
                    if let Some(resolved) = &res.resolved_from {
                        println!("resolved from '{resolved}' -> '{}'", res.entity);
                    }
                    if let Some(ao) = &res.as_of {
                        println!("facts valid as-of {ao}:");
                    } else {
                        println!("facts for '{}':", res.entity);
                    }
                    for fact in &res.facts {
                        let cur = if fact.is_current() {
                            "(current)"
                        } else {
                            "(historical)"
                        };
                        let from = fact.valid_from.as_deref().unwrap_or("?");
                        let to = fact.valid_to.as_deref().unwrap_or("now");
                        println!(
                            "  {} {} {} [{} .. {}] {}",
                            fact.subject, fact.predicate, fact.object, from, to, cur
                        );
                    }
                    if !res.candidates.is_empty() {
                        println!("\nDid you mean one of:");
                        for cand in &res.candidates {
                            println!("  - {}", cand.name);
                        }
                    }
                }
            }
            KgCommand::Add {
                subject,
                predicate,
                object,
                from,
                to,
                conf,
                src,
                file,
            } => {
                let tid = store.kg_add_triple(
                    &subject,
                    &predicate,
                    &object,
                    from.as_deref(),
                    to.as_deref(),
                    conf,
                    src.as_deref(),
                    file.as_deref(),
                    None,
                    None,
                )?;
                println!("added triple {tid}: {subject} -> {predicate} -> {object}");
            }
            KgCommand::Invalidate {
                subject,
                predicate,
                object,
                ended,
            } => {
                let count = store.kg_invalidate(&subject, &predicate, &object, ended.as_deref())?;
                println!("invalidated {count} fact(s) for {subject} -> {predicate} -> {object}");
            }
            KgCommand::Supersede {
                subject,
                predicate,
                old_obj,
                new_obj,
                at,
                conf,
            } => {
                let tid = store.kg_supersede(
                    &subject,
                    &predicate,
                    &old_obj,
                    &new_obj,
                    at.as_deref(),
                    conf,
                    None,
                    None,
                    None,
                    None,
                )?;
                println!("superseded {old_obj} -> {new_obj} on {subject} -> {predicate} (new triple: {tid})");
            }
            KgCommand::Timeline {
                entity,
                limit,
                offset,
                json,
            } => {
                let facts = store.kg_timeline(entity.as_deref(), limit, offset)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&facts).unwrap_or_default()
                    );
                } else {
                    for f in &facts {
                        let from = f.valid_from.as_deref().unwrap_or("?");
                        let to = f.valid_to.as_deref().unwrap_or("now");
                        println!(
                            "{} | {} -> {} -> {} [{from} .. {to}]",
                            f.id, f.subject, f.predicate, f.object
                        );
                    }
                }
            }
            KgCommand::Stats { json } => {
                let stats = store.kg_stats()?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&stats).unwrap_or_default()
                    );
                } else {
                    println!("entities:           {}", stats.entities);
                    println!("triples:            {}", stats.triples);
                    println!("current facts:      {}", stats.current_facts);
                    println!("expired facts:      {}", stats.expired_facts);
                    println!(
                        "relationship types: {}",
                        stats.relationship_types.join(", ")
                    );
                }
            }
        },
        MemoryCommand::Links {
            domain,
            prune,
            min_count,
            json,
        } => {
            if prune {
                let pruned = store.prune_links(domain.as_deref(), min_count)?;
                println!("pruned {pruned} link(s)");
            } else {
                let links = match domain {
                    Some(ref d) => store.compute_links(d)?,
                    None => store.list_links(None)?,
                };
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&links).unwrap_or_default()
                    );
                } else {
                    for l in &links {
                        println!(
                            "{} | {} <-> {} (count: {})",
                            l.domain, l.entity_a, l.entity_b, l.count
                        );
                    }
                }
            }
        }
        MemoryCommand::Crosslinks(cross_cmd) => match cross_cmd {
            CrosslinksCommand::Propose { max, json } => {
                let proposals = store.crosslinks_propose(max)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&proposals).unwrap_or_default()
                    );
                } else {
                    println!("proposed {} crosslink(s):", proposals.len());
                    for p in &proposals {
                        println!(
                            "  {} | {} <-> {} (strength: {}) [{}]",
                            p.entity, p.from_endpoint, p.to_endpoint, p.strength, p.status
                        );
                    }
                }
            }
            CrosslinksCommand::Apply { json } => {
                let count = store.crosslinks_apply()?;
                if json {
                    println!("{}", serde_json::json!({ "applied": count }));
                } else {
                    println!("applied {count} crosslink(s)");
                }
            }
            CrosslinksCommand::Prune { json } => {
                let count = store.crosslinks_prune()?;
                if json {
                    println!("{}", serde_json::json!({ "pruned": count }));
                } else {
                    println!("pruned {count} crosslink(s)");
                }
            }
            CrosslinksCommand::List { json } => {
                let links = store.list_crosslinks()?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&links).unwrap_or_default()
                    );
                } else {
                    for l in &links {
                        println!(
                            "{} | {} <-> {} (strength: {}) [{}]",
                            l.entity, l.from_endpoint, l.to_endpoint, l.strength, l.status
                        );
                    }
                }
            }
        },
        MemoryCommand::Traverse {
            start,
            max_hops,
            json,
        } => {
            let steps = store.traverse(&start, max_hops)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&steps).unwrap_or_default()
                );
            } else {
                println!("traversal from '{start}':");
                for s in &steps {
                    let via = s.connected_via.as_deref().unwrap_or("root");
                    println!(
                        "  [hop {}] {} (via {}, count: {})",
                        s.hop, s.node, via, s.count
                    );
                }
            }
        }
        MemoryCommand::Bridges {
            domain_a,
            domain_b,
            json,
        } => {
            let cross_links = store.find_cross_links(domain_a.as_deref(), domain_b.as_deref())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&cross_links).unwrap_or_default()
                );
            } else {
                for t in &cross_links {
                    println!(
                        "bridge: {} | {} <-> {} (strength: {})",
                        t.bridge_entity, t.domain_a, t.domain_b, t.strength
                    );
                }
            }
        }
        MemoryCommand::Journal(cmd) => match cmd {
            JournalCommand::Write { content, agent } => {
                let entry = store.journal_write(&agent, &content, None)?;
                println!(
                    "journal entry written for {} at {} {}: {}",
                    entry.agent, entry.date, entry.time, entry.content
                );
            }
            JournalCommand::Read {
                agent,
                date,
                limit,
                json,
            } => {
                let entries = store.journal_read(&agent, date.as_deref(), Some(limit))?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&entries).unwrap_or_default()
                    );
                } else {
                    for e in entries {
                        println!("## [{}] {} {}\n{}\n", e.agent, e.date, e.time, e.content);
                    }
                }
            }
        },
        MemoryCommand::Events(cmd) => match cmd {
            EventsCommand::Append {
                body,
                stream,
                event_type,
                from,
                to,
                corr,
                status,
            } => {
                let ev = store.event_append(
                    &stream,
                    &event_type,
                    &from,
                    to.as_deref(),
                    &body,
                    corr.as_deref(),
                    status.as_deref(),
                )?;
                println!("appended event {}: {} -> {:?}", ev.id, ev.from, ev.to);
            }
            EventsCommand::List {
                stream,
                since,
                unacked_by,
                limit,
                json,
            } => {
                let since_ts = since.as_deref().map(parse_time).transpose()?;
                let events =
                    store.event_list(&stream, since_ts, Some(limit), unacked_by.as_deref())?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&events).unwrap_or_default()
                    );
                } else {
                    for ev in events {
                        println!(
                            "{} [{}] from: {} to: {:?} ({})\n  {}",
                            ev.id,
                            ev.event_type,
                            ev.from,
                            ev.to,
                            ev.at.to_rfc3339(),
                            ev.body
                        );
                    }
                }
            }
            EventsCommand::Ack {
                event_id,
                agent,
                stream,
            } => {
                store.event_ack(&stream, &agent, &event_id)?;
                println!("event {event_id} acknowledged by {agent}");
            }
            EventsCommand::Wait {
                agent,
                stream,
                timeout_ms,
                json,
            } => {
                let ev_opt = store.event_wait(&stream, &agent, timeout_ms).await?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&ev_opt).unwrap_or_default()
                    );
                } else {
                    match ev_opt {
                        Some(ev) => println!("received event {}: {}", ev.id, ev.body),
                        None => println!("wait timed out with no events"),
                    }
                }
            }
        },
        MemoryCommand::Artifact(cmd) => match cmd {
            ArtifactCommand::Put { file, id } => {
                let bytes = std::fs::read(&file).map_err(|e| {
                    HarnessError::MemoryError(format!("Failed to read {}: {e}", file.display()))
                })?;
                let mut meta = std::collections::HashMap::new();
                meta.insert(
                    "source_filename".to_string(),
                    file.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                );
                let art_id = store.artifact_put(id.as_deref(), &bytes, meta)?;
                println!("stored artifact {art_id} ({} bytes)", bytes.len());
            }
            ArtifactCommand::Get { id, out } => {
                let Some((bytes, meta)) = store.artifact_get(&id)? else {
                    return Err(HarnessError::MemoryError(format!(
                        "Artifact not found: {id}"
                    )));
                };
                if let Some(out_path) = out {
                    std::fs::write(&out_path, &bytes).map_err(|e| {
                        HarnessError::MemoryError(format!(
                            "Failed to write to {}: {e}",
                            out_path.display()
                        ))
                    })?;
                    println!(
                        "saved artifact {id} ({} bytes) to {}",
                        bytes.len(),
                        out_path.display()
                    );
                } else {
                    println!(
                        "artifact {id}: {} bytes, created {}",
                        meta.size_bytes,
                        meta.created_at.to_rfc3339()
                    );
                }
            }
        },
        MemoryCommand::Daemon(cmd) => match cmd {
            DaemonCommand::Start { port, bg } => {
                if bg {
                    let exe =
                        std::env::current_exe().map_err(|e| HarnessError::Io(e.to_string()))?;
                    let child = std::process::Command::new(exe)
                        .args(["memory", "daemon", "start", "--port", &port.to_string()])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .map_err(|e| HarnessError::Io(e.to_string()))?;
                    println!(
                        "daemon started in background (pid: {}) on port {port}",
                        child.id()
                    );
                } else {
                    println!("starting memory writer daemon on 127.0.0.1:{port}...");
                    let runtime = harness_memory::DaemonRuntime::new(store.root().to_path_buf())?;
                    runtime.run_servers(port).await?;
                }
            }
            DaemonCommand::Stop => {
                match harness_memory::NativeDaemonClient::connect_root(store.root()) {
                    Ok(client) => {
                        let _ = client.shutdown().await;
                        println!("daemon shutdown signal sent");
                    }
                    Err(e) => {
                        println!("daemon not running or connection failed: {e}");
                    }
                }
            }
            DaemonCommand::Status { json } => {
                match harness_memory::NativeDaemonClient::connect_root(store.root()) {
                    Ok(client) => match client.ping().await {
                        Ok(val) => {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&val).unwrap_or_default()
                                );
                            } else {
                                println!("daemon status: running");
                                println!(
                                    "pid:         {}",
                                    val.get("pid")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                );
                                println!(
                                    "store_root:  {}",
                                    val.get("store_root")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("")
                                );
                                if let Some(counts) = val.get("counts") {
                                    println!("queue:       {}", counts);
                                }
                            }
                        }
                        Err(e) => {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::json!({ "running": false, "error": e.to_string() })
                                );
                            } else {
                                println!("daemon not responding: {e}");
                            }
                        }
                    },
                    Err(_) => {
                        if json {
                            println!("{}", serde_json::json!({ "running": false }));
                        } else {
                            println!("daemon is not running");
                        }
                    }
                }
            }
            DaemonCommand::Jobs { limit, json } => {
                let client = harness_memory::NativeDaemonClient::connect_root(store.root())?;
                let jobs = client.list_jobs(limit).await?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&jobs).unwrap_or_default()
                    );
                } else {
                    println!("queued / recent jobs (limit {limit}):");
                    for j in jobs {
                        println!(
                            "  {} | [{}] {} (attempts: {})",
                            j.id, j.state, j.kind, j.attempts
                        );
                    }
                }
            }
            DaemonCommand::Wait { job_id, timeout_s } => {
                let client = harness_memory::NativeDaemonClient::connect_root(store.root())?;
                let start = std::time::Instant::now();
                let deadline = std::time::Duration::from_secs(timeout_s);
                loop {
                    let job = client.get_job(&job_id).await?;
                    if job.state == "succeeded" || job.state == "failed" || job.state == "cancelled"
                    {
                        println!("job {job_id} finished: {}", job.state);
                        if let Some(res) = job.result {
                            println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
                        }
                        break;
                    }
                    if start.elapsed() > deadline {
                        println!(
                            "timed out waiting for job {job_id} (current state: {})",
                            job.state
                        );
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        },
        MemoryCommand::Mcp {
            stdio: _,
            http,
            port,
        } => {
            if http {
                println!("starting MCP HTTP server on 127.0.0.1:{port}...");
                let runtime = harness_memory::DaemonRuntime::new(store.root().to_path_buf())?;
                runtime.run_servers(port).await?;
            } else {
                harness_memory::run_stdio_server(&store)?;
            }
        }
        MemoryCommand::Serve {
            host,
            port,
            tls_cert,
            tls_key,
        } => {
            if tls_cert.is_some() || tls_key.is_some() {
                return Err(HarnessError::ConfigurationError(
                    "TLS is not built into this binary; bind to loopback, or terminate TLS at a reverse proxy and forward to the daemon. Remove --tls-cert/--tls-key."
                        .to_string(),
                ));
            }
            println!("serving memory store on {host}:{port} (bearer token required)...");
            let runtime = harness_memory::DaemonRuntime::new(store.root().to_path_buf())?;
            runtime.run_servers_on(&host, port).await?;
        }
        MemoryCommand::Hook(cmd) => match cmd {
            HookCommand::Run { event, harness } => {
                let mut input = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
                let out = harness_memory::run_hook(&event, &harness, &input, &store)?;
                println!("{out}");
            }
        },
        MemoryCommand::Instructions { name } => {
            let text = harness_memory::run_instructions(name.as_deref().unwrap_or("help"))?;
            println!("{text}");
        }
        MemoryCommand::Rules {
            host,
            harness,
            project,
            mcp,
        } => {
            let rules = harness_memory::render_shared_brain_rules(&host, &harness, &project, &mcp)?;
            println!("{rules}");
        }
    }
    Ok(())
}

fn parse_time(text: &str) -> Result<chrono::DateTime<chrono::Utc>, HarnessError> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&chrono::Utc))
        .map_err(|error| {
            HarnessError::ConfigurationError(format!("invalid timestamp '{text}': {error}"))
        })
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
