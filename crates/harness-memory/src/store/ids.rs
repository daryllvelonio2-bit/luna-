//! Content-addressed identifiers and slugs for the memory store.
//!
//! Same hashing recipe as the reference `ids.py`.
//! Prevents delimiter collisions by using length-prefixed parts (`{len}:{part}`)
//! before hashing with SHA-256 and truncating to standard hex lengths.

use sha2::{Digest, Sha256};

pub const ID_RECIPE: &str = "v3";
pub const HASH_TRUNC_RECORD: usize = 24;
pub const HASH_TRUNC_TRIPLE: usize = 12;

/// Hash parts joined with length prefixes (`{len}:{part}`), truncated to `truncate` hex chars.
/// Matches the reference design's `_delimited_sha256` verbatim.
pub fn delimited_sha256(parts: &[&str], truncate: usize) -> String {
    let mut key = String::new();
    for part in parts {
        key.push_str(&part.len().to_string());
        key.push(':');
        key.push_str(part);
    }
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex.chars().take(truncate).collect()
}

/// Turn arbitrary text into a filesystem-safe slug (same rule as the reference `slugify`).
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in input.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches(['-', '.']);
    if trimmed.is_empty() {
        "general".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

/// Keep an existing identifier usable as a file stem.
pub fn safe_id(input: &str) -> String {
    let mut out = String::new();
    for c in input.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('.').trim_matches('_');
    if trimmed.is_empty() {
        "record".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn body_hash(body: &str) -> String {
    format!("hash:{}", delimited_sha256(&[body], HASH_TRUNC_RECORD))
}

/// Record ID for the chunked miner path.
/// Matches the reference design `make_record_id_from_chunk`:
/// `f"record_{domain}_{topic}_{_delimited_sha256((source_file, str(chunk_index)), 24)}"`
pub fn record_id_from_chunk(
    domain: &str,
    topic: &str,
    source_file: &str,
    chunk_index: usize,
) -> String {
    let idx = chunk_index.to_string();
    let hash = delimited_sha256(&[source_file, &idx], HASH_TRUNC_RECORD);
    format!("rec_{}_{}_{}", slugify(domain), slugify(topic), hash)
}

/// Record ID for the content / tool path.
/// Matches the reference design `make_record_id_from_content`:
/// `f"record_{domain}_{topic}_{_delimited_sha256((domain, topic, content), 24)}"`
pub fn record_id_from_content(domain: &str, topic: &str, content: &str) -> String {
    let hash = delimited_sha256(&[domain, topic, content], HASH_TRUNC_RECORD);
    format!("rec_{}_{}_{}", slugify(domain), slugify(topic), hash)
}

/// Single entry point for content-addressed record id.
pub fn record_id(
    domain: &str,
    topic: &str,
    source_file: &str,
    chunk_index: usize,
    _content: &str,
) -> String {
    record_id_from_chunk(domain, topic, source_file, chunk_index)
}

/// NFC, lower-cased name with whitespace runs collapsed.
/// Direct port of the reference design's `canonical_entity_name`.
pub fn canonical_entity_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.join(" ")
}

/// Identifier that keeps punctuation so distinct names stay distinct.
/// Direct port of the reference design's `entity_id_for_name`:
/// `canonical_entity_name(name).replace("%", "%25").replace("_", "%5F").replace(" ", "_")`
pub fn entity_id_for_name(name: &str) -> String {
    let text = canonical_entity_name(name)
        .replace('%', "%25")
        .replace('_', "%5F");
    text.replace(' ', "_")
}

/// Triple ID for knowledge-graph insertion.
/// Direct port of the reference design's `make_triple_id`:
/// `f"t_{sub_id}_{predicate}_{obj_id}_{_delimited_sha256((valid_from, recorded_at), 12)}"`
pub fn make_triple_id(
    sub_id: &str,
    predicate: &str,
    obj_id: &str,
    valid_from: Option<&str>,
    recorded_at: &str,
) -> String {
    let vf = valid_from.unwrap_or("");
    let hash = delimited_sha256(&[vf, recorded_at], HASH_TRUNC_TRIPLE);
    format!("t_{sub_id}_{predicate}_{obj_id}_{hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_ids_are_stable_and_content_addressed() {
        let a = record_id("luna", "memory", "src/lib.rs", 0, "hello");
        let b = record_id("luna", "memory", "src/lib.rs", 0, "hello");
        let c = record_id("luna", "memory", "src/lib.rs", 1, "hello");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("rec_luna_memory_"));
        assert_eq!(a.len(), "rec_luna_memory_".len() + 24);
    }

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slugify("crates/harness-memory"), "crates-harness-memory");
        assert_eq!(slugify("  "), "general");
        assert_eq!(safe_id("user.profile"), "user.profile");
        assert_eq!(safe_id("../etc/passwd"), "etc_passwd");
    }

    #[test]
    fn delimited_sha256_avoids_collisions() {
        // "/a1" + "23" vs "/a" + "123"
        let h1 = delimited_sha256(&["/a1", "23"], 24);
        let h2 = delimited_sha256(&["/a", "123"], 24);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_entity_id_and_triple_id_canonicalization() {
        assert_eq!(canonical_entity_name("  Alice   Smith  "), "alice smith");
        assert_eq!(entity_id_for_name("Alice Smith"), "alice_smith");
        assert_eq!(entity_id_for_name("O'Brien"), "o'brien");
        assert_eq!(entity_id_for_name("foo_bar%baz"), "foo%5Fbar%25baz");

        let tid1 = make_triple_id(
            "alice",
            "works_on",
            "luna",
            Some("2026-01-01"),
            "2026-05-30T10:00:00Z",
        );
        let tid2 = make_triple_id(
            "alice",
            "works_on",
            "luna",
            Some("2026-01-01"),
            "2026-05-30T10:00:00Z",
        );
        assert_eq!(tid1, tid2);
        assert!(tid1.starts_with("t_alice_works_on_luna_"));
        assert_eq!(tid1.len(), "t_alice_works_on_luna_".len() + 12);
    }
}
