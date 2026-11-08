# Luna memory store — Markdown-native design

Status: Proposed (design record, no code yet).  
Date: 2026-11-08.  
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
| Search index | pure-file: Tantivy (BM25) + `vectors.f32` (exact cosine) | no SQLite anywhere; `search_index/` is disposable |
| Vector search | brute-force exact cosine with a write-through append log | viable to ~100k records; optional ANN later behind a trait |
| Embeddings | local ONNX MiniLM 384-d default, optional embeddinggemma | zero API calls by default |
| SQLite | **not** the source of truth | existing `memory.db` is migrated, then retired |
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
- Multi-user authentication and remote hosting (a local hub is the v1 model).
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
   embedder must refuse, not silently degrade.
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
    corpus.list
    tantivy/
    vectors.f32
    vectors.ids
    vectors.deleted
    kg.jsonl
    graph.json
```

Everything outside `search_index/` is canonical. `search_index/` is gitignored
and rebuildable with `luna memory reindex`.

Large stores may shard records by id prefix
(`records/ab/<record-id>.md`) once a topic exceeds `memory.store.shard_after`
(default 5,000 files). The path is resolved from the id, so sharding is not
part of the logical model.

## 4. Data model

### 4.1 Record

One file per verbatim chunk. Front matter is YAML (parsed with `serde_yaml`,
already a workspace dependency). The body is untouched source text.

```markdown
---
id: rec_9f2c1a7b3d4e5f60718293a4
domain: luna
topic: memory
source_file: crates/harness-memory/src/engine.rs
source_hash: blake3:ab12cd34...
chunk_index: 3
line_start: 363
line_end: 476
filed_at: 2026-11-08T21:59:00Z
authored_at: 2026-11-08T18:00:00Z
authored_at_source: git
content_date: 2026-11-08
content_date_source: git
entities: [retrieve, sqlite, like, confidence]
extract_mode: code
parent_record_id: null
hlc: 1783038849123-000000-rep_local
tags: [memory, retrieval]
is_bookkeeping: false
body_hash: blake3:9911...
---
<verbatim chunk, byte-for-byte as read after decoding>
```

Field rules:

| Field | Meaning |
|---|---|
| `id` | `rec_` + first 24 hex of `blake3(domain, topic, source_file, chunk_index, content)` |
| `domain` / `topic` | taxonomy placement; lowercase slugs |
| `source_file` | absolute or repo-relative path at mine time |
| `source_hash` | hash of the whole source file when mined |
| `chunk_index` | 0-based index within the source |
| `line_start` / `line_end` | 1-based inclusive span in the source |
| `filed_at` | when the store wrote it (UTC) |
| `authored_at` | when the content was authored, if known |
| `authored_at_source` | `git`, `frontmatter`, `filesystem`, `unknown` |
| `content_date` | the date the content is *about* (for temporal queries) |
| `content_date_source` | `git`, `frontmatter`, `filename`, `unknown` |
| `entities` | extracted candidate entities (see §7.3) |
| `extract_mode` | `code`, `prose`, `transcript`, `journal` (affects chunking) |
| `parent_record_id` | set when this is a chunk of a logical entry |
| `hlc` | hybrid logical clock stamp for replica-safe merge order |
| `body_hash` | hash of the body, used by the incremental indexer |

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

Topics are assigned by the miner (§6) and can be renamed by
`luna memory topics apply`. A rename rewrites `topic:` in every affected
record front matter and regenerates content indexes.

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
  "embedder": { "name": "all-MiniLM-L6-v2", "dim": 384, "revision": "v1" },
  "record_count": 1234,
  "corpus_hash": "blake3:...",
  "built_at": "2026-11-08T22:00:00Z",
  "tantivy_schema": 1
}
```

Open rules:

1. No manifest → build.
2. `embedder` ≠ configured embedder → refuse with
   `embedder identity mismatch` and the exact reindex command.
3. `corpus_hash` ≠ current `corpus.list` hash → incremental update.
4. `schema` unknown → refuse (newer store, older binary).

`corpus.list` is the indexer's work list: one line per record,
`<id>\t<body_hash>\t<relative_path>`, sorted by id. It is maintained by the
single writer and can also be rebuilt by scanning front matter only.

### 5.2 Lexical index (Tantivy)

Schema (one document per record):

| field | type | stored | indexed |
|---|---|---|---|
| `id` | STRING | yes | exact |
| `domain` | STRING | yes | exact/facet |
| `topic` | STRING | yes | exact/facet |
| `source_file` | STRING | yes | exact |
| `filed_at` | STRING (ISO) | yes | fast |
| `authored_at` | STRING (ISO) | yes | fast |
| `content_date` | STRING (ISO) | yes | fast |
| `entities` | STRING | yes | tokenized |
| `text` | TEXT | yes | tokenized (BM25) |
| `body_hash` | STRING | yes | no |

`text` includes the content-index topic line and the body when the record is
short, otherwise the body alone. Deletes use `delete_term(id)`. Commits are
batched per mine run or per MCP write.

### 5.3 Vector index (append-only exact cosine)

No ANN in v1. Vectors are L2-normalized at write, so cosine is a dot product.

Files:

- `vectors.f32` — header (`LPV1`, `u32 dim`, `u64 count`) then row-major
  little-endian `f32` rows.
- `vectors.ids` — one record id per line, same row order.
- `vectors.deleted` — tombstoned ids, one per line.

Query: `memmap2` the matrix, iterate live rows, dot with the query vector,
keep a bounded min-heap of the top `k * overfetch`. Skip ids in
`vectors.deleted` (a `HashSet` loaded once). At 100k × 384 this is ~150 MB
mapped and a few milliseconds of SIMD-able work.

Writes append the new vector and id, then atomically replace the header count.
Deletes append to `vectors.deleted`. Compaction (`reindex` or when tombstones
exceed 20%) rewrites the live matrix and atomically renames it over the old one.

`SearchIndexBackend` is a trait so an ANN (`hnsw_rs`, `usearch`) or `sqlite-vec`
backend can be added later without touching the store or the query pipeline.

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

1. Acquire the store lock (`search_index/.lock`, `fs2` exclusive) or route to
   the daemon.
2. Compute ids and hashes.
3. Write each file to a temp path in the same directory, `fsync`, then rename.
4. Append the index delta (Tantivy add/delete, vector append) or enqueue it in
   `search_index/pending.jsonl` when batching.
5. Append to the run journal `search_index/journal/<run_id>.jsonl`.
6. Release the lock.

Crash recovery replays the journal: every entry is idempotent because ids are
content-addressed. A temp file left behind is removed on the next write.

### 6.2 Project miner

`luna memory mine <dir>`:

- Walk the tree with gitignore rules (`ignore` crate), skipping
  `.git`, `node_modules`, `target`, binaries, and `memory.store.skip_dirs`.
- Detect file kind: code, prose, config, transcript.
- Route to a topic by directory and content signals; a `memory.yaml` in the
  project supplies explicit `domain`/`topics` overrides.
- Chunk ~`memory.store.chunk_chars` (default 800) on paragraph/symbol
  boundaries; never mid-UTF-8; preserve line spans.
- Rewrite a source only if `source_hash` changed; purge records for deleted or
  newly ignored files (`sync`).
- Build content indexes and compact summaries for each source after its records
  land.
- Emit link updates for the domain.

### 6.3 Conversation miner

`luna memory mine --mode convos <dir-or-file>`:

- Claude Code JSONL, ChatGPT export, Slack export, Codex transcripts, plain
  text.
- Normalize to a canonical exchange stream; pair user+assistant as one unit.
- One record per exchange (or per bounded window for long turns), with
  `extract_mode: transcript`, `authored_at` from the message, and
  `parent_record_id` for the session.
- Never summarise; store verbatim.

### 6.4 Notes and sessions

- `luna memory index-notes` indexes `paths.notes_dir` **by reference**: the
  record body is the note body, and `source_file` points at the note. The note
  stays canonical; no copy is made.
- `luna memory mine --mode sessions` mines `sessions.db` transcripts into a
  `conversations` domain. This is how Luna's existing history becomes
  searchable.

## 7. Retrieval

Pipeline, in order:

1. Sanitize the query (length, control chars, path traversal).
2. Parse filters: domain, topic, source_file, `since`, `before`, `as_of`.
3. Embed the query locally.
4. Vector search over `vectors.f32`.
5. BM25 search over Tantivy.
6. Blend: `score = wv * vector_norm + wb * bm25_norm`, defaults `0.6 / 0.4`,
   configurable (`memory.store.hybrid_vector_weight`,
   `memory.store.hybrid_bm25_weight`). Each component is min-max normalized
   over the candidate union.
7. Content-index boost: the best pointer-index hit per `source_file` adds
   `+ memory.store.index_boost * (1 - rank / n_hits)` to that source's records.
   Content indexes never remove candidates.
8. Optional LLM rerank (`memory.store.rerank: off | local | provider`), off by
   default; never required for the headline retrieval path.
9. Deduplicate chunks that belong to the same `parent_record_id`; keep the best.
10. Read the best records' bodies and return id, path, span, score components,
    and provenance.

Result shape (shared by CLI and MCP):

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
      "excerpt": "...",
      "body": "..."
    }
  ],
  "truncated": false
}
```

Time windows compare `content_date` when present, else `authored_at`, else
`filed_at`; the response says which field was used.

## 8. Temporal model

- `filed_at` — store write time.
- `authored_at` — when the content was written (git commit, transcript
  timestamp, front matter, else filesystem, else unknown).
- `content_date` — the date the content describes (best effort).
- HLC — 13-digit ms, 6-hex counter, replica id; lexicographic order equals
  causal order. Stored per record and per KG write so two replicas can merge
  without trusting wall clocks.
- KG validity — `from`/`to`; `as_of` returns only triples valid at that
  instant; `supersede` edits the boundary atomically.
- Deletes leave no tombstone in Markdown: a removed record is removed from
  disk, but the run journal and git history retain it.

## 9. Write routing and concurrency

| policy | behavior |
|---|---|
| `direct` | write files directly under the store lock |
| `prefer` | use the daemon if running, else direct |
| `require` | daemon only; block if unavailable |

Config: `memory.store.write_routing: direct | prefer | require` (default
`prefer`). Hooks never cold-start the daemon and keep a 500 ms budget.

The daemon (`luna memory daemon start`) is the single long-lived writer. It
provides two endpoints: a native Luna connection (default, IPC/socket with no
MCP overhead) and an optional MCP/HTTP server for external agents. It owns the
Tantivy writer and serializes index commits. Readers open the index directly and
never touch the lock.

## 10. CLI surface

```
luna memory init [dir]
luna memory mine <dir> [--mode projects|convos|sessions] [--domain D] [--topic T]
luna memory sweep
luna memory sync [--dry-run]
luna memory search <query> [--domain D] [--topic T] [--since D] [--before D] [--results N]
luna memory compact [--domain D]
luna memory context [--domain D]
luna memory split <dir>
luna memory hook run <event>
luna memory instructions
luna memory rules
luna memory repair [--max-seq-id | --encoding | ...]
luna memory daemon start|stop|status|jobs|wait
luna memory mcp [--stdio|--http]
luna memory serve [--host H --port P] [--tls-cert C --tls-key K]
luna memory reindex [--force]
luna memory migrate [--from-memory-db|--from-notes|--from-sessions]
luna memory migrate-domains
luna memory links [--domain D] [--prune]
luna memory audit
luna memory topics propose|apply
luna memory domains split
luna memory kg query|add|invalidate|supersede|timeline|stats|normalize|import-legacy
luna memory crosslinks propose|apply|prune
luna memory status
luna memory export [--out DIR]
luna memory update check|plan|configure
luna memory events append|list|wait|watch|ack|sync
luna memory task create|launch
luna memory artifact put|get
luna memory set-embedder
```

Backed by `harness-memory`; `harness-cli` gets a `memory` subcommand. Command
implementations live in `harness-memory/src/store/ops/` so the MCP server and
CLI share one code path.

## 11. Communication endpoints: Native Luna & MCP

The memory subsystem exposes two distinct endpoints:

1. **Native Luna connection (default)**: Luna connects directly via native IPC
   (Unix domain socket or direct memory engine bindings) without any MCP wrapping
   or serialization overhead. This is the zero-overhead default path for Luna's
   interactive sessions, background savers, and hooks.
2. **MCP server endpoint (optional)**: For external tools and third-party agents
   (Claude Code, Codex, Cursor). Run `luna memory mcp` (stdio) or `luna memory serve`
   (HTTP, optional TLS and bearer token). Tool names use the `memory_` prefix so
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
| `session_end` | final async mine + journal entry |
| `precompact` | synchronous mine before compaction |

They are off unless `hooks.enabled` and `memory.store.hooks_auto_save` are
true. Background work is detached/spawned so a hook returns within its budget
(500 ms target). Workspace-provided hooks still require `hooks.trust_workspace`.

Client plugins (Claude Code, Codex, Cursor) can be generated by
`luna memory hooks install <client>`; they call `luna memory hook run <event>`
and receive the same JSON contract Luna's hooks already use.

## 13. Configuration

Add to `harness-core/src/config.rs`:

```rust
pub struct MemorySettings {
    // existing: mode, background_*, ...
    pub store: MemoryStoreSettings,
}

pub struct MemoryStoreSettings {
    pub enabled: bool,              // default true once migrated
    pub embedder: String,           // "minilm" | "embeddinggemma" | "none"
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
    pub overfetch: usize,           // 4
}
```

And `paths.memory_dir` (`~/.luna/memory`). Env overrides follow the existing
`LUNA_` convention, e.g. `LUNA_MEMORY_STORE_ENABLED=false`.

`memory.mode` stays: `session` (agent writes) and `background` (the existing
`MemorySaver` writes). In store mode, `save_memory` becomes an adapter that
writes a `record` plus an optional KG triple, so existing prompts and tools
keep working.

## 14. Migration from current Luna memory

`luna memory migrate` is idempotent and preserves the old files as backups.

| Source | Destination | Rule |
|---|---|---|
| `memory.db` rows | `domains/legacy/topics/memory/records/` | one record per active row; `subject/predicate/object` becomes a KG triple |
| `notes/*.md` | indexed by reference under `domains/notes/` | no copy; `source_file` points at the note |
| `sessions.db` | `domains/conversations/` | mined as transcripts, one record per exchange |
| `notes.db` | already imported by `harness-notes` | untouched |

After a successful migration, `memory.db` is renamed
`memory.db.store-imported`; the store is the writer from then on. The old
`save_memory`/`update_memory`/`forget_memory` tools remain as adapters so a
rollback is possible.

## 15. Operations

| Command | Behavior |
|---|---|
| `audit` | score topics, naming, cross-links, links, KG; report unreachable records, broken links, invalid front matter |
| `repair` | fix front matter, rebuild missing content indexes/summaries, drop dangling pointers, normalize ids, repair encoding |
| `export` | write a clean Markdown tree (records + KG + journal) outside the store |
| `sync` | remove records whose source is deleted, moved, or gitignored |
| `backup` | `cp -r` (content) + optionally `tar` the search index; retention by count |
| `reindex` | full rebuild from Markdown; verifies manifest and embedder identity |
| `set-embedder` | re-embed every record into a new index generation, or refuse |
| `status` | counts by domain/topic, index age, embedder, stale sources |

Because the store is files, backup and export are trivial; the hard part is
the search index, and the answer is always "rebuild it."

## 16. Performance and scale

| Corpus | Text | Vectors (384-d f32) | Tantivy | Query |
|---|---|---|---|---|
| 5k records | ~4 MB | ~8 MB | ~2 MB | < 30 ms |
| 50k records | ~40 MB | ~77 MB | ~20 MB | < 100 ms |
| 100k records | ~80 MB | ~154 MB | ~40 MB | < 200 ms |
| 500k records | ~400 MB | ~768 MB | ~200 MB | use ANN backend |

File count is the real cost. At 50k records the tree is large but workable;
sharding by id prefix keeps `read_dir` fast. If this becomes a problem, a
`pack` mode can move a topic's records into one Markdown file with section
fences, at the cost of exact line spans.

## 17. Crate layout

Extend `crates/harness-memory`; keep the existing modules as the compatibility
surface.

```
crates/harness-memory/
  Cargo.toml
  src/lib.rs
  src/engine.rs            # legacy SQLite engine (compatibility)
  src/saver.rs             # existing background saver
  src/tools.rs             # existing save/update/forget adapters
  src/store/
    mod.rs
    config.rs
    error.rs
    ids.rs
    frontmatter.rs
    atomic.rs
    record.rs
    pointer_index.rs       # the content-index layer
    summary.rs             # compact summary
    context.rs             # L0-L3
    kg.rs
    links.rs
    crosslinks.rs
    journal.rs
    events.rs
    migrate.rs
    mcp.rs
    hooks.rs
    daemon.rs
    ops/
      audit.rs
      repair.rs
      export.rs
      sync.rs
      backup.rs
    mine/
      mod.rs
      project.rs
      convo.rs
      notes.rs
      sessions.rs
  src/search/
    mod.rs
    query.rs
    ranking.rs
    render.rs
  src/search_index/
    mod.rs
    manifest.rs
    lexical.rs             # Tantivy
    vectors.rs             # append-only f32 matrix
    kg_index.rs
    graph.rs
  tests/
```

Dependencies to add to the workspace:

- `tantivy` — BM25 index.
- `fastembed` (or `ort` + `tokenizers`) — local MiniLM/embeddinggemma.
- `memmap2` — vector matrix reads.
- `fs2` — file locks.
- `blake3` — content hashes.
- `ignore` — gitignore-aware walking.
- `serde_yaml` — already present.
- `pulldown-cmark` — already present, for rendering.

`harness-notes` stays as the user's note store and is indexed by reference.

## 18. Phased implementation

| Phase | Deliverable | Exit criterion |
|---|---|---|
| 0 | scaffolding: config, front matter, ids, atomic writes, lock | round-trip a record file |
| 1 | project miner + Tantivy + vector matrix + hybrid search | `mine` a repo, `search` returns the correct record |
| 2 | content indexes, compact summaries, L0–L3 context | context text under 900 tokens, search improves |
| 3 | KG + temporal + links/cross-links | `kg query --as-of`, `traverse` work |
| 4 | conversation/session miner, journal, event log | mine `sessions.db`, query it |
| 5 | native daemon connection (default) + optional MCP server, hooks, plugins | native Luna connection active; Claude Code/Codex can read and write via MCP |
| 6 | audit/repair/export/sync/backup, migration, eval | `luna memory migrate` + R@5 harness |

Phase 1 is the load-bearing one: if hybrid retrieval over Markdown is not good,
nothing above it matters.

## 19. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Index and files drift | manifest `corpus_hash`; reindex; index is disposable |
| File count / inode pressure | shard by id prefix; optional pack mode |
| Concurrent writes | single-writer lock + daemon; idempotent content-addressed ids |
| Partial multi-file writes | run journal + replay; atomic rename per file |
| User edits break front matter | validate on read; `audit` lists; never silently drop |
| Embedding model drift | recorded identity; refuse and require `set-embedder` |
| Markdown escaping in pointer lines | escape `|`, `\`, newlines; validate on read |
| KG edits leave inconsistent triples | one-line-per-triple files; `kg normalize`; supersede is atomic |
| Large binary index in backups | index is gitignored; backups copy Markdown, rebuild index |
| Tantivy is a heavy dependency | isolate in `search_index/lexical.rs`; a fallback substring scan exists for degraded mode |

## 20. Open questions

1. **Notes duplication.** Index notes by reference (recommended) or copy them
   into a `notes` domain? By-reference keeps one canonical copy and matches
   Luna's current file-per-note model.
2. **Compact-summary generation.** Local heuristic only, or optional LLM
   refinement? Recommend heuristic by default, LLM opt-in.
3. **Embedding distribution.** `fastembed` bundles ONNX; embeddinggemma is
   ~300 MB. Ship MiniLM only, make embeddinggemma a download.
4. **Keep the legacy SQLite engine forever?** Recommend: adapter for one
   release, then deprecate once migration and tests are stable.
5. **Multi-agent hub in v1?** Recommend: yes for MCP/HTTP, no auth beyond a
   bearer token and loopback default.

## 21. Module map

| Concept | Module |
|---|---|
| record | `store/record.rs`, `store/frontmatter.rs` |
| content index | `store/pointer_index.rs` |
| compact summary | `store/summary.rs` |
| context layers | `store/context.rs` |
| knowledge graph | `store/kg.rs`, `search_index/kg_index.rs` |
| links / cross-links | `store/links.rs`, `store/crosslinks.rs`, `search_index/graph.rs` |
| search pipeline | `search/query.rs`, `search/ranking.rs`, `search/render.rs` |
| lexical index | `search_index/lexical.rs` |
| vector index | `search_index/vectors.rs` |
| project miner | `store/mine/project.rs` |
| conversation miner | `store/mine/convo.rs`, `store/mine/sessions.rs` |
| MCP surface | `store/mcp.rs` |
| hooks | `store/hooks.rs` |
| daemon / routing | `store/daemon.rs`, `store/atomic.rs` |
| backups, repair, audit, sync, export | `store/ops/*` |
| journal | `store/journal.rs` |
| event log | `store/events.rs` |
| migration | `store/migrate.rs` |

Deliberately not reproduced: multi-database backend adapters, ANN capacity and
repair machinery, and database/index synchronization. Their job is done by the
file store, the append-only vector log, the manifest, and `reindex`.
