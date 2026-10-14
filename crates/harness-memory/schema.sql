CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    object TEXT NOT NULL,
    raw_content TEXT NOT NULL,
    category TEXT NOT NULL CHECK(category IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer', 'harness_improvement')),
    confidence REAL NOT NULL DEFAULT 1.0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_active INTEGER NOT NULL DEFAULT 1,
    superseded_by TEXT REFERENCES memories(id)
);

CREATE TABLE IF NOT EXISTS memory_embeddings (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memories_subject_predicate ON memories(subject, predicate);
CREATE INDEX IF NOT EXISTS idx_memories_active ON memories(is_active);
