//! Content-addressed identifiers and slugs for the memory store.

/// Turn arbitrary text into a filesystem-safe slug.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in input.trim().chars() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "general".to_string()
    } else {
        out
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

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 96 deterministic bits, rendered as hex. Not a cryptographic hash; it is a
/// stable content address for deduplication and file naming.
pub fn hash96(bytes: &[u8]) -> String {
    let a = fnv1a64(bytes, 0);
    let b = fnv1a64(bytes, 0x9e37_79b9_7f4a_7c15);
    let c = fnv1a64(bytes, 0xc2b2_ae3d_27d4_eb4f);
    format!("{a:016x}{b:016x}{c:016x}")
}

pub fn body_hash(body: &str) -> String {
    format!("hash:{}", hash96(body.as_bytes()))
}

/// A record id derived from its identity, so re-mining the same chunk is a no-op.
pub fn record_id(
    domain: &str,
    topic: &str,
    source_file: &str,
    chunk_index: usize,
    content: &str,
) -> String {
    let mut buf = Vec::with_capacity(source_file.len() + content.len() + 48);
    buf.extend_from_slice(domain.as_bytes());
    buf.push(0);
    buf.extend_from_slice(topic.as_bytes());
    buf.push(0);
    buf.extend_from_slice(source_file.as_bytes());
    buf.push(0);
    buf.extend_from_slice(chunk_index.to_string().as_bytes());
    buf.push(0);
    buf.extend_from_slice(content.as_bytes());
    let digest = hash96(&buf);
    format!("rec_{}", &digest[..24])
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
        assert!(a.starts_with("rec_"));
        assert_eq!(a.len(), 4 + 24);
    }

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slugify("crates/harness-memory"), "crates-harness-memory");
        assert_eq!(slugify("  "), "general");
        assert_eq!(safe_id("user.profile"), "user.profile");
        assert_eq!(safe_id("../etc/passwd"), "etc_passwd");
    }
}
