# Luna memory store — Markdown-native design

Status: Implemented in `harness-memory` (0.1.6). This document records the
shipped design; where the implementation uses a simpler mechanism than the
original plan, the simpler behavior is documented here.
Date: 2026-11-08, revised 2026-11-09.
Scope: A full persistent-memory system for Luna, with **Markdown as the
canonical store** and a **derived, rebuildable, pure-file search index**.

This document uses Luna's own vocabulary throughout: **domains → topics →
indexes → records**, **links**, **cross-links**, **compact summaries**,
**journal**, **event log**, and **context layers**.

## 0. Decision record

| Decision | Choice | Consequence |
|---|---|---|
| Canonical storage | Markdown files + YAML front matter | human-readable, git-diffable, user-editable, `cp -r` backup |
| Record granularity | one `.md` per verbatim chunk | exact line spans, clean diffs, more files |
| Search index | Derived index: in-memory BM25 + in-memory 384-d vectors, rebuilt from Markdown; only `search_index/manifest.json` is persisted | no SQLite anywhere; `search_index/` is disposable |
| Vector search | brute-force exact cosine over vectors recomputed on load | viable to ~100k records; optional ANN later behind a trait |
| Embeddings | built-in feature hashing (384-d, `hash-fnv-v1`); no model download | zero API calls; the `Embedder` seam allows an ONNX backend later |
| SQLite | **not** the source of truth | existing `memory.db` is imported once, then kept only as a backup |
| Integration point | extend `harness-memory`, expose `luna memory` | no new crate, no new brand name |

The one unavoidable deviation from a database-backed design: the search index
must be a real index. With Markdown as truth, pluggability moves to the
*search-index* layer (`BruteForce` now, optional `Hnsw` or `SqliteVec` later).
Everything above that line — taxonomy, content indexes, compact summaries,
knowledge graph, temporal model, links, cross-links, retrieval pipeline, MCP
surface, hooks, daemon, ops — is preserved.

## 1. Goals and non-goals

### Goals

1. Store memory verbatim, not only distilled conclusions.
2. Organize it spatially and semantically (domains, topics, content indexes).
3. Retrieve by meaning **and** by exact words, with filters and time windows.
4. Keep a temporal, provenance-carrying knowledge graph.
5. Expose the whole system over MCP so other agents can use it.
6. Keep every durable byte human-readable Markdown.
7. Never require a cloud API for storage, indexing, or retrieval.
8. Survive corruption: the search index can always be rebuilt from Markdown,
   and Markdown can always be restored from git or a directory backup.

### Non-goals (for v1)

- Multi-database backend adapters (not meaningful on files).
- ANN at million-record scale (brute force is enough for personal use).
- Multi-user accounts; v1 has one bearer token and binds loopback by default.
- Lossless reconstruction from a compact summary (it is a summary by design).

## 2. Design principles

1. **Markdown is the database.** If the search index is deleted, nothing is lost.
2. **Every fact points at its evidence.** A record id is the provenance unit.
3. **The search index is a cache, not a citizen.** It has a schema version, an
   embedder identity, and a one-command rebuild.
4. **One writer.** All mutations go through a store lock or the daemon.
   Readers never need the lock.
5. **Content-addressed ids.** A record id is a hash of its identity, so
   re-mining is idempotent and collisions are detectable.
6. **Fail loud on model drift.** Opening an index built by a different
   embedder must rebuild with the configured embedder, never mix vectors.
7. **A pass that saves nothing is a success.** Luna's existing curation policy
   stays; the store is the substrate underneath it.

## 3. Directory layout

Default root: `~/.luna/memory` (`paths.memory_dir`).

```
~/.luna/memory/
  store.md                       # store metadata + format version
  identity.md                    # L0 identity text (context layer)
  SUMMARY.md                     # the compact-summary spec
  domains/
    <domain-slug>/
      domain.md                  # domain metadata
      topics/
        <topic-slug>/
          topic.md               # topic metadata
          records/
            <record-id>.md       # one verbatim chunk
          indexes/
            <source-slug>.index.md
          summaries/
            <source-slug>.summary.md
  links/
    <domain-slug>.links.md
  crosslinks/
    <entity-slug>.crosslinks.md
  graph/
    entities/<entity-slug>.md
    triples/<subject-slug>.md
  journal/
    <agent-slug>/<YYYY-MM-DD>.md
  events/
    <stream-slug>/
      <YYYY-MM-DD>.md
      acks/<agent-slug>.md
  artifacts/
    <artifact-id>/meta.md
    <artifact-id>/payload
  sources/                       # optional verbatim mirror (off by default)
  search_index/                  # DERIVED — safe to delete
    manifest.json
    kg.jsonl
    graph.json
```

Everything outside `search_index/` is canonical. `search_index/` is gitignored
and rebuildable with `luna memory reindex`.

`memory.store.shard_after` is reserved: the current writer always uses
`records/<id>.md` and does not shard by prefix yet.

## 4. Data model

### 4.1 Record

One file per verbatim chunk. Front matter is YAML (parsed with `serde_yaml`,
already a workspace dependency). The body is untouched source text.

```markdown
---
id: rec_luna_memory_9f2c1a7b3d4e5f60718293a4
domain: luna
topic: memory
source_file: crates/harness-memory/src/engine.rs
chunk_index: 3
line_start: 363
line_end: 476
filed_at: 2026-11-08T21:59:00Z
entities: retrieve, sqlite, like, confidence
tags: memory, retrieval
body_hash: hash:9911ab22cd33ef44aa55bb66
subject: engine.retrieval
predicate: uses
object: SQLite
name: engine.rs
path: crates/harness-memory/src/engine.rs
category: fact
confidence: 0.9000
created_at: 2026-11-08T18:00:00Z
updated_at: 2026-11-08T21:59:00Z
is_active: true
---
<verbatim chunk, byte-for-byte as read after decoding>
```

Field rules:

| Field | Meaning |
|---|---|
| `id` | `rec_<domain>_<topic>_<24 hex of SHA-256 over the delimited source/chunk tuple>` |
| `domain` / `topic` | taxonomy placement; lowercase slugs |
| `source_file` | absolute or repo-relative path at mine time |
| `chunk_index` | 0-based index within the source |
| `line_start` / `line_end` | 1-based inclusive span in the source |
| `filed_at` | when the store wrote it (UTC) |
| `entities` / `tags` | extracted candidates, comma-separated |
| `body_hash` | `hash:` plus a truncated SHA-256 of the body |
| `subject` / `predicate` / `object` | the one-line memory triple used by prompts and supersession |
| `name` / `path` | optional display name and source path |
| `category` | `fact`, `user_profile`, `code_pattern`, `workflow_rule`, `note_pointer` |
| `confidence` | 0.0–1.0 |
| `created_at` | authored time: source mtime for mined files, transcript message time for sessions |
| `updated_at` | last write; not used for `since`/`before` filters |
| `is_active` / `superseded_by` | supersession state |

SQLite-backed columns such as `created_at` are preserved on import, so a
migrated store keeps its original ordering.

Missing or unparseable front matter is a hard error for the indexer and a
warning for the reader: the file is listed under `memory audit` but never
silently dropped.

### 4.2 Domain and topic metadata

`domain.md`:

```markdown
---
domain: luna
created_at: 2026-11-08T21:00:00Z
source_roots: [/home/janelle/Documents/GitHub/rust-agent-harness]
description: The Luna harness itself
---
```

`topic.md`:

```markdown
---
topic: memory
domain: luna
created_at: 2026-11-08T21:05:00Z
description: crates/harness-memory
---
```

Topics are assigned by the miner (§6). There is no rename command yet: to
rename a topic, move its directory and rewrite `topic:` in the affected record
front matter, then run `luna memory reindex`.

### 4.3 Content index

A content index is the pointer layer between a topic and its records. It is a
compact map from topics/entities to record ids. It never gates retrieval; it
only boosts the records of a matched source file.

`domains/luna/topics/memory/indexes/engine.index.md`:

```markdown
---
source_file: crates/harness-memory/src/engine.rs
records: [rec_9f2c1a7b3d4e5f60718293a4, rec_a1b2c3d4e5f60718293a4b5c6]
built_at: 2026-11-08T21:59:10Z
chunk_indexes: [3, 4]
---
- retrieval is LIKE-based|engine.rs;retrieve;sqlite|2026-11-08:L363-476|→rec_9f2c1a7b3d4e5f60718293a4
- recent-memory fallback|fallback;recency|2026-11-08:L477-520|→rec_a1b2c3d4e5f60718293a4b5c6
```

Line grammar (`|` and `\` escaped with `\`):

```
topic | entity;entity | [YYYY-MM-DD:Lstart-Lend] | →record_id[,record_id]
```

Limits: up to `memory.store.index_chars` (default 1,500) of pointer lines per
file; entity/topic extraction scans the first `memory.store.index_scan_chars`
(default 5,000) of the source. At most 3 record pointers per line and per
index file.

### 4.4 Compact summary

One `.summary.md` per source, using a structured symbolic format. The spec
lives in `SUMMARY.md` and is returned by `memory_get_summary_spec`.

```markdown
---
source_file: crates/harness-memory/src/engine.rs
built_at: 2026-11-08T21:59:10Z
lossy: true
---
FILE 7|luna/memory|2026-11-08|Memory engine
S1:engine;retrieve;sqlite|retrieval;fallback;"LIKE over six columns"|CORE|TECHNICAL
S2:engine;confidence;active|ranking;supersession|"one active record per topic"|CORE|TECHNICAL
X:S1<->S2|retrieval feeds ranking
ARC:neutral->focused
```

Format:

```
Header:  FILE_NUM|DOMAIN/TOPIC|DATE|TITLE
Entry:   S<id>:ENTITIES|topic_keywords|"key_quote"|WEIGHT|FLAGS
Cross:   X:S<id><->S<id>|label
Arc:     ARC:state->state->state
```

A compact summary is explicitly **lossy**. The search index and every UI
surface must label it as a summary and keep the record pointer.

### 4.5 Knowledge graph

Canonical files per entity and per subject.

`graph/entities/sqlite.md`:

```markdown
---
entity: SQLite
type: technology
aliases: [sqlite3, sqlite-vec]
created_at: 2026-11-08T21:00:00Z
updated_at: 2026-11-08T21:00:00Z
---
## Attributes
- key: language | value: C | from: 2000-08-17 | to: null
- key: used_by | value: Luna | from: 2026-01-01 | to: null
## Notes
Single-file embedded database.
```

`graph/triples/luna.md` (one line per triple; `|` and newlines escaped):

```markdown
---
subject: Luna
updated_at: 2026-11-08T21:00:00Z
---
- p: uses | o: SQLite | from: 2026-01-01 | to: null | conf: 1.0 | src: rec_9f2c... | file: crates/harness-memory/src/engine.rs
- p: has_feature | o: Markdown notes | from: 2026-02-01 | to: null | conf: 1.0 | src: rec_a1b2... | file: crates/harness-notes/src/file_engine.rs
- p: superseded | o: old memory engine | from: 2026-06-01 | to: null | conf: 0.9 | src: rec_c3d4...
```

Rules:

- A triple is `subject → predicate → object` with `from`/`to` validity.
- `to: null` means currently true.
- `conf` is 0.0–1.0.
- `src` is the record id that evidences it.
- Single-valued predicates (model, employer, address) use **supersede**:
  one atomic edit that sets the old triple's `to` and adds the new one at the
  same boundary. `kg_supersede` must never leave an overlap.
- Incoming edges are answered from the derived `search_index/kg.jsonl`,
  rebuilt from the canonical files.

### 4.6 Links and cross-links

Links are within-domain entity co-occurrence.

`links/luna.links.md`:

```markdown
---
domain: luna
built_at: 2026-11-08T22:00:00Z
---
- a: retrieve | b: sqlite | count: 12 | records: [rec_9f2c..., rec_a1b2...]
- a: retrieve | b: bm25 | count: 4 | records: [rec_e5f6...]
```

Cross-links are links between domains, proposed and pruned, never auto-applied
blindly.

`crosslinks/sqlite.crosslinks.md`:

```markdown
---
entity: sqlite
---
- from: luna/memory | to: luna/storage | label: sqlite | strength: 12 | status: approved
```

`luna memory crosslinks propose` writes `crosslinks/proposal.md` for review;
`apply` materializes approved rows. `prune` removes generic-token, dangling,
and duplicate-spelling links.

### 4.7 Identity, context layers, journal

- `identity.md` — L0, always injected (~100 tokens).
- L1 — generated from the most recent records into a bounded text block
  (~800 tokens), never persisted as truth.
- L2 — on-demand topic/domain recall.
- L3 — deep search.

The journal is per-agent, per-day, append-only:

```markdown
---
agent: luna
date: 2026-11-08
---
## 21:59
Investigated the memory store. Confirmed the retrieval pipeline is hybrid.
```

### 4.8 Event log and artifacts

Coordination state for multi-agent use.

`events/main/2026-11-08.md` — each event is a fenced YAML block plus prose:

````markdown
## evt_01J...
```yaml
type: delegation
from: claude
to: codex
at: 2026-11-08T22:10:00Z
status: open
correlation_id: corr_42
```
Implement the record writer.
````

`events/main/acks/<agent>.md` lists acked event ids. `event_wait` polls the day
files (and the next day at midnight) with a bounded timeout. Artifacts are
exact bytes (`artifacts/<id>/payload`) with Markdown metadata
(`artifacts/<id>/meta.md`); this is the only non-Markdown canonical content,
because an artifact is defined by being byte-exact.

## 5. Derived search index

### 5.1 Manifest

`search_index/manifest.json`:

```json
{
  "schema": 1,
  "embedder": { "name": "hash", "dim": 384 },
  "record_count": 1234,
  "built_at": "2026-11-08T22:00:00Z"
}
```

The manifest is informational and detects a changed embedder; the index itself
is rebuilt from Markdown rather than read from disk.

Open rules:

1. No manifest → build from Markdown and write one.
2. `embedder` ≠ configured embedder → rebuild from Markdown with the new
   embedder, then overwrite the manifest.
3. `schema` unknown → rebuild from Markdown.
4. `record_count` is advisory; the actual count always comes from scanning
   `domains/`.

There is no `corpus.list` file: the scan in step 1 is the work list, and it is
rebuilt whenever the process loads or `reindex` runs.

### 5.2 Lexical index (in-memory BM25)

The shipped build does not link Tantivy. On first read (or after `reindex`) the
store loads every record, tokenizes `search_text(record)` (subject, predicate,
object, entities, tags, and body), and keeps the postings in memory. BM25 is
scored per query with corpus-relative IDF, then min-max normalized over the
candidate set.

| field | used for |
|---|---|
| `id`, `domain`, `topic`, `source_file` | filtering |
| `created_at` | time windows (`since`/`before`) |
| `entities`, `tags`, `body` | BM25 terms |

`search_index/manifest.json` records the schema, embedder identity, and record
count so a stale or mismatched cache can be detected. The index itself is not
persisted; deleting `search_index/` only forces a rebuild.

### 5.3 Vector index (in-memory exact cosine)

No ANN in v1. Vectors are L2-normalized when produced, so cosine is a dot
product. The shipped embedder is `hash-fnv-v1`, a deterministic feature hasher
over word unigrams, word bigrams, and character trigrams. It needs no model
download and no network, and its identity is part of the manifest so changing
it forces a reindex.

Vectors are not persisted. On first read the store recomputes each record's
384-d vector from its text and keeps it beside the parsed record; `reindex`
drops and recomputes the set. A query is embedded the same way and scored
against every live candidate with exact cosine, then blended with BM25.

`Embedder` is a trait, so a local model backend can replace the hasher later
without touching the store or the query pipeline.

### 5.4 Graph indexes

- `search_index/kg.jsonl` — one JSON object per triple, including `subject`,
  `predicate`, `object`, `from`, `to`, `conf`, `src`. Powers incoming-edge and
  as-of queries without scanning every `graph/` file.
- `search_index/graph.json` — adjacency for links and cross-links, rebuilt from
  those files.

Both are rebuilt by `reindex` and updated on the corresponding write.

## 6. Ingestion

### 6.1 Write protocol

All mutations follow the same sequence:

1. Acquire the store lock (`search_index/.lock`, exclusive-create file with
   stale detection) or route to the daemon.
2. Compute ids and hashes.
3. Write each file to a temp path in the same directory, then `rename` it over
   the target.
4. Update the in-memory cache and rewrite `search_index/manifest.json`.
5. Release the lock.

Writes are idempotent because a record id is stable for a given source and
chunk index, so re-running a mine overwrites the same file. A temp file left
behind by a crash is removed on the next write to that path.

### 6.2 Project miner

`luna memory mine <dir>`:

- Walk the tree with `walkdir`, skipping hidden directories, `node_modules`,
  `target`, and the other members of `SKIP_DIRS`.
- Accept only known text extensions; binaries and unknown files are skipped.
- Route to a topic by directory and filename signals; `FOLDER_TOPIC_MAP` maps
  well-known folders (e.g. `crates/<name>`, `src/`, `docs/`) to topics.
- Chunk ~`memory.store.chunk_chars` (default 800) on paragraph/symbol
  boundaries; never mid-UTF-8; preserve line spans.
- Re-mining a source writes the new chunks and deletes that source's old record
  ids in the same domain, so edits do not leave duplicates. Deleted files are
  cleaned up on the next mine of their directory.
- Build content indexes and compact summaries for each source after its records
  land.
- Within-domain links are computed on demand with `luna memory links --domain D`,
  not during mining; `reindex` then folds the saved links and cross-links into
  the graph.

### 6.3 Conversation miner

`luna memory mine --mode convos <dir-or-file>`:

- Claude Code JSONL, ChatGPT export, Slack export, Codex transcripts, plain
  text.
- Normalize to a canonical exchange stream; pair user+assistant as one unit.
- One record per exchange (or per bounded window for long turns), stored
  verbatim. Authored time is carried in `created_at`: session mining uses the
  transcript message timestamp, and file mining uses the file modified time.
- Never summarise; store verbatim.

### 6.4 Notes and sessions

- Notes stay canonical in `harness-notes`; the memory store does not copy or
  re-index note bodies. The background saver writes short note-pointer records
  (`category: note_pointer`) so prompts know a note exists.
- `luna memory mine --mode sessions <sessions.db>` mines transcript rows into a
  `conversations` domain. This is how Luna's existing history becomes
  searchable.

## 7. Retrieval

Pipeline, in order:

1. Load the in-memory record set (rebuilding it from Markdown if stale).
2. Parse filters: domain, topic, `since`, `before` (both compare `created_at`).
3. Embed the query with the configured embedder.
4. Exact cosine over every live candidate's in-memory vector.
5. BM25 over the in-memory record set.
6. Blend: `score = wv * vector + wb * bm25_norm`, defaults `0.6 / 0.4`,
   configurable (`memory.store.hybrid_vector_weight`,
   `memory.store.hybrid_bm25_weight`). BM25 is min-max normalized over the
   candidate union; vector similarity is absolute cosine clamped to `[0, 1]`
   and never normalized, so the candidate set cannot reshuffle the vector
   signal.
7. Content-index boost: the best pointer-index hit per `source_file` adds
   `+ memory.store.index_boost * (1 - rank / n_hits)` to that source's records.
   Content indexes never remove candidates.
8. Sort by score and truncate to `limit`.

Reranking and `parent_record_id` deduplication are not implemented; every hit
is an independent record.

Result shape (CLI `--json`; MCP returns the same records inside a `content`
text block and adds `count`):

```json
{
  "query": "...",
  "results": [
    {
      "id": "rec_...",
      "domain": "luna",
      "topic": "memory",
      "source_file": "...",
      "line_start": 363,
      "line_end": 476,
      "score": 0.87,
      "vector_score": 0.81,
      "bm25_score": 0.63,
      "index_boost": 0.04,
      "matched_via": "record",
      "index_preview": null,
      "excerpt": "...",
      "path": "/home/janelle/.luna/memory/domains/..."
    }
  ],
  "truncated": false
}
```

Time windows compare the record's authored time, stored in `created_at`.
Project and conversation miners populate it from the source file's modified
time (sessions use the transcript message timestamp); `updated_at` is only the
last-write time and is deliberately not used for `since`/`before`.

## 8. Temporal model

- `created_at` — authored time: when the content was written (source mtime for
  files, transcript message time for sessions, preserved on SQLite import).
- `updated_at` — last write time.
- KG validity — `from`/`to`; `as_of` returns only triples valid at that
  instant; `supersede` edits the boundary atomically.
- Deletes leave no tombstone in Markdown: a removed record is removed from
  disk, but the journal and git history retain it.

## 9. Write routing and concurrency

| policy | behavior |
|---|---|
| `direct` | write files directly under the store lock |
| `prefer` | send `save`/`update`/`forget` to the daemon if its endpoint is present; fall back to direct on failure |
| `require` | daemon only; startup fails if no daemon endpoint exists, and a failed write is an error |

Config: `memory.store.write_routing: direct | prefer | require` (default
`direct`). The daemon-backed engine reloads Markdown before reads so writes made
by the daemon process are visible. Note-pointer writes stay direct. Hooks never
cold-start the daemon.

The daemon (`luna memory daemon start`) is the single long-lived writer. It
provides two endpoints: a native Luna connection (default, IPC/socket with no
MCP overhead) and an optional MCP/HTTP server for external agents. It serializes
queued work and index commits. Readers reload from Markdown.

Both endpoints require the bearer token stored at
`<memory root>/daemon/token` (mode `0600`); `/health` is the only public route.
`luna memory serve --host H` binds the requested interface. TLS is not built in:
the process refuses `--tls-cert`/`--tls-key` rather than silently ignoring them,
so terminate TLS at a reverse proxy.

## 10. CLI surface

```
luna memory init
luna memory status [--json]
luna memory mine [dir] [--mode projects|convos|sessions] [--domain D] [--topic T]
luna memory search <query> [--domain D] [--topic T] [--since RFC3339] [--before RFC3339] [--limit N]
luna memory show <id|subject|subject.predicate>
luna memory context [--limit N]
luna memory reindex
luna memory audit
luna memory export <dir>
luna memory migrate [--from-memory-db <memory.db>]
luna memory forget <id|all>
luna memory domains
luna memory kg query|add|invalidate|supersede|timeline|stats
luna memory links [--domain D]
luna memory crosslinks propose|apply|prune|list
luna memory traverse <start> [--domain-a D --domain-b D]
luna memory bridges [--domain-a D --domain-b D]
luna memory journal write|read
luna memory events append|list|ack|wait
luna memory artifact put|get
luna memory daemon start|stop|status|jobs|wait
luna memory mcp [--stdio|--http] [--port N]
luna memory serve [--host H] [--port N]
luna memory hook run <event> [--harness H]
luna memory instructions [name]
luna memory rules [--host H --harness H --project P]
```

`serve` accepts `--tls-cert`/`--tls-key` only to reject them with an explanatory
error; TLS is not built in. Backed by `harness-memory`; `harness-cli` wires the
`memory` subcommand. The MCP server and CLI call the same store methods.

## 11. Communication endpoints: Native Luna & MCP

The memory subsystem exposes two distinct endpoints:

1. **Native Luna connection (default)**: Luna connects directly via native IPC
   (Unix domain socket or direct memory engine bindings) without any MCP wrapping
   or serialization overhead. This is the zero-overhead default path for Luna's
   interactive sessions, background savers, and hooks.
2. **MCP server endpoint (optional)**: For external tools and third-party agents
   (Claude Code, Codex, Cursor). Run `luna memory mcp` (stdio) or `luna memory serve`
   (HTTP, bearer token required). Tool names use the `memory_` prefix so
   they never collide with Luna's existing `mcp__server__tool` namespace.

| Group | Tools |
|---|---|
| Store | `memory_status`, `memory_list_domains`, `memory_list_topics`, `memory_get_taxonomy`, `memory_get_summary_spec` |
| Read | `memory_search`, `memory_get_record`, `memory_list_records`, `memory_recent_checkpoint`, `memory_reconnect` |
| Write | `memory_add_record`, `memory_update_record`, `memory_delete_record`, `memory_delete_by_source`, `memory_check_duplicate`, `memory_mine`, `memory_sync` |
| KG | `memory_kg_query`, `memory_kg_add`, `memory_kg_invalidate`, `memory_kg_supersede`, `memory_kg_timeline`, `memory_kg_stats` |
| Graph | `memory_traverse`, `memory_find_crosslinks`, `memory_list_crosslinks`, `memory_follow_crosslinks`, `memory_list_links`, `memory_create_crosslink`, `memory_delete_crosslink`, `memory_delete_link` |
| Journal | `memory_journal_write`, `memory_journal_read` |
| Coordination | `memory_event_append`, `memory_event_list`, `memory_event_wait`, `memory_event_ack`, `memory_task_create`, `memory_patch_submit`, `memory_peers`, `memory_artifact_put`, `memory_artifact_get`, `memory_checkpoint` |
| Hooks | `memory_hook_settings` |

Parameter schemas follow the existing MCP conventions: an input object with
explicit types and a `required` list.

## 12. Hooks

Luna already has generic lifecycle hooks (`crates/harness-core/src/hooks.rs`).
The memory store adds semantic hooks:

| event | action |
|---|---|
| `session_start` | inject L0 + L1 via the memory prompt block |
| `stop` | auto-save every `memory.store.hooks_save_interval` human messages (default 10) |
| `session_end` | final mine + journal entry |
| `precompact` | synchronous mine before compaction |

Clients should invoke these only when `hooks.enabled` and
`memory.store.hooks_auto_save` are true. The memory side independently refuses
writes when `hooks_auto_save` is false. Background work is detached/spawned so a
hook returns within its budget (500 ms target). Workspace-provided hooks still
require `hooks.trust_workspace`.

Client plugins (Claude Code, Codex, Cursor) call `luna memory hook run <event>`
with a JSON payload and receive the same JSON contract Luna's hooks already
use. There is no installer command yet; wire the hook in the client's own
settings.

## 13. Configuration

Shipped in `harness-core/src/config.rs`:

```rust
pub struct MemorySettings {
    // existing: mode, background_*, ...
    pub store: MemoryStoreSettings,
}

pub struct MemoryStoreSettings {
    pub enabled: bool,              // default true
    pub embedder: String,           // "hash" (shipped) | "none"; model names warn and fall back
    pub chunk_chars: usize,         // 800
    pub index_chars: usize,         // 1500
    pub index_scan_chars: usize,    // 5000
    pub hybrid_vector_weight: f32,  // 0.6
    pub hybrid_bm25_weight: f32,    // 0.4
    pub index_boost: f32,           // 0.1
    pub rerank: String,             // "off" | "local" | "provider"
    pub auto_mine: bool,            // false
    pub hooks_auto_save: bool,      // true
    pub hooks_save_interval: usize, // 10
    pub write_routing: String,      // "direct" | "prefer" | "require"
    pub shard_after: usize,         // 5000 files per topic
}
```

The internal `StoreConfig` also carries `overfetch` (4), but it has no
user-facing setting. `rerank` is accepted and stored but not yet acted on; all
retrieval uses the hybrid blend.

And `paths.memory_dir` (`~/.luna/memory`). Env overrides follow the existing
`LUNA_` convention, e.g. `LUNA_MEMORY_STORE_ENABLED=false`.

`memory.mode` stays: `session` (agent writes) and `background` (the existing
`MemorySaver` writes). In store mode, `save_memory` becomes an adapter that
writes a `record` plus an optional KG triple, so existing prompts and tools
keep working.

## 14. Migration from current Luna memory

The Markdown store imports `memory.db` automatically the first time it opens
and finds no record files. `luna memory migrate` runs the same import
explicitly. The import is idempotent because each row keeps its original id.

| Source | Destination | Rule |
|---|---|---|
| `memory.db` rows | `domains/<derived>/topics/<derived>/records/` | one record per row, id preserved |
| `notes/*.md` | not imported | notes stay in `harness-notes`; memory keeps note-pointer records |
| `sessions.db` | `domains/conversations/` | mined as transcripts, one record per exchange |
| `notes.db` | already imported by `harness-notes` | untouched |

`memory.db` is left in place as a backup; it is no longer the writer. The old
`save_memory`/`update_memory`/`forget_memory` tools use the Markdown engine, so
a rollback means setting `memory.store.enabled = false`.

## 15. Operations

Shipped today:

| Command | Behavior |
|---|---|
| `audit` | report invalid front matter, duplicate active records, empty bodies, and dangling supersessions |
| `export` | copy the canonical Markdown tree to another directory |
| `reindex` | drop the cache and rebuild it from Markdown; rewrite the manifest |
| `status` | counts by domain/topic, root, and embedder |
| `migrate` | import rows from the legacy SQLite database |
| `forget` | delete one record, or every record with `all` |

Planned but not shipped: `repair`, `sync`, `backup`, `set-embedder`. Until
they exist, repair means editing Markdown by hand, sync means re-running
`mine`, and backup is `cp -r` of the store root (the index rebuilds itself).

## 16. Performance and scale

| Corpus | Text | Vectors (384-d f32, RAM) | Query |
|---|---|---|---|
| 5k records | ~4 MB | ~8 MB | < 30 ms |
| 50k records | ~40 MB | ~77 MB | < 100 ms |
| 100k records | ~80 MB | ~154 MB | < 200 ms |
| 500k records | ~400 MB | ~768 MB | use ANN backend |

Because the index is rebuilt from Markdown on load, cold start is proportional
to corpus size; steady-state queries are in-memory scans.

File count is the real cost. At 50k records the tree is large but workable.
Prefix sharding is reserved rather than implemented, so a very large topic is
one directory; if that becomes a problem, the `shard_after` setting is the
intended knob.

## 17. Crate layout

Extend `crates/harness-memory`; keep the existing modules as the compatibility
surface.

```
crates/harness-memory/
  Cargo.toml
  src/lib.rs               # engine selection, config wiring
  src/engine.rs            # legacy SQLite engine (compatibility)
  src/saver.rs             # existing background saver
  src/tools.rs             # save/update/forget adapters
  src/store/
    mod.rs                 # store, search, audit, export, migration
    ids.rs
    frontmatter.rs
    atomic.rs
    embed.rs               # hash embedder
    pointer_index.rs       # the content-index layer
    summary.rs             # compact summary
    context.rs             # L0-L3
    kg.rs
    links.rs
    crosslinks.rs
    graph.rs
    journal.rs
    events.rs
    mcp.rs
    hooks.rs
    daemon.rs
    engine.rs              # Markdown engine + daemon write routing
    miner.rs               # project miner
    convo.rs               # conversation miner
    sessions.rs            # sessions.db miner
    search.rs              # BM25 and blending
  tests/
```

Dependencies actually used by the store:

- `sha2` — content and cache-tag hashing.
- `regex` — compact-summary signal extraction.
- `walkdir` — tree walking.
- `serde_yaml` — front matter.
- `rusqlite` — legacy migration source and the daemon job queue.
- `tokio` — daemon sockets and HTTP/MCP serving.

No Tantivy, ONNX, or vector-file dependency is linked; the index is in memory.

`harness-notes` stays as the user's note store and is indexed by reference.

## 18. Phased implementation

| Phase | Deliverable | Exit criterion |
|---|---|---|
| 0 | scaffolding: config, front matter, ids, atomic writes, lock | round-trip a record file |
| 1 | project miner + in-memory BM25 + vectors + hybrid search | `mine` a repo, `search` returns the correct record |
| 2 | content indexes, compact summaries, L0–L3 context | context text under 900 tokens, search improves |
| 3 | KG + temporal + links/cross-links | `kg query --as-of`, `traverse` work |
| 4 | conversation/session miner, journal, event log | mine `sessions.db`, query it |
| 5 | native daemon connection (default) + optional MCP server, hooks, plugins | native Luna connection active; Claude Code/Codex can read and write via MCP |
| 6 | audit/export/migration, eval | `luna memory migrate` + R@5 harness |

Phase 1 is the load-bearing one: if hybrid retrieval over Markdown is not good,
nothing above it matters.

## 19. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Index and files drift | index is rebuilt from Markdown on load; `reindex` forces it; index is disposable |
| File count / inode pressure | one file per record; topic directory boundaries; prefix sharding is reserved |
| Concurrent writes | single-writer lock + daemon; idempotent content-addressed ids |
| Partial multi-file writes | atomic rename per file; a failed mine can be re-run idempotently |
| User edits break front matter | validate on read; `audit` lists; never silently drop |
| Embedding model drift | embedder identity recorded in the manifest; changing it triggers a rebuild |
| Markdown escaping in pointer lines | escape `|`, `\`, newlines; validate on read |
| KG edits leave inconsistent triples | one-line-per-triple files; `supersede` is atomic |
| Large binary index in backups | index is gitignored; backups copy Markdown, rebuild index |
| In-memory index costs cold-start time | rebuild once on load; manifest lets a future persistent index be added behind the same query path |

## 20. Open questions

1. **Notes duplication.** Index notes by reference (recommended) or copy them
   into a `notes` domain? By-reference keeps one canonical copy and matches
   Luna's current file-per-note model.
2. **Compact-summary generation.** Local heuristic only, or optional LLM
   refinement? Recommend heuristic by default, LLM opt-in.
3. **Embedding distribution.** The shipped default is the dependency-free
   `hash` embedder. A local ONNX MiniLM backend can be added behind the
   `Embedder` trait; it would be a download, not a build dependency.
4. **Keep the legacy SQLite engine forever?** Recommend: adapter for one
   release, then deprecate once migration and tests are stable.
5. **Multi-agent hub in v1?** Yes for MCP/HTTP, with a required bearer token.
   The default bind stays loopback; non-loopback binds are allowed but still
   authenticated, and TLS is expected to terminate at a reverse proxy.

## 21. Module map

| Concept | Module |
|---|---|
| record, store, migration, audit, export | `store/mod.rs`, `store/frontmatter.rs`, `store/ids.rs` |
| content index | `store/pointer_index.rs` |
| compact summary | `store/summary.rs` |
| context layers | `store/context.rs` |
| knowledge graph | `store/kg.rs` |
| links / cross-links | `store/links.rs`, `store/crosslinks.rs`, `store/graph.rs` |
| search pipeline | `store/search.rs`, `store/embed.rs` |
| project miner | `store/miner.rs` |
| conversation miner | `store/convo.rs`, `store/sessions.rs` |
| MCP surface | `store/mcp.rs` |
| hooks | `store/hooks.rs` |
| daemon / routing | `store/daemon.rs`, `store/atomic.rs`, `store/engine.rs` |
| journal | `store/journal.rs` |
| event log | `store/events.rs` |
| engine selection | `lib.rs` (`build_memory_engine`) |

Deliberately not reproduced: multi-database backend adapters, ANN capacity and
repair machinery, and database/index synchronization. Their job is done by the
Markdown store, an in-memory index, the manifest, and `reindex`.
