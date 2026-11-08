//! Deterministic local embeddings.
//!
//! Default `memory.store.embedder` is `hash`, which needs no model download
//! and no network. It is a feature-hashing embedder: word unigrams, word
//! bigrams, and character trigrams are hashed into a fixed 384-dimensional
//! space and L2-normalized. That captures enough lexical overlap to make
//! vector search useful, and the `Embedder` seam lets an ONNX MiniLM backend
//! replace it later without touching the store.
//!
//! The embedder identity is part of the derived index manifest, so changing
//! implementations forces a reindex instead of silently mixing spaces.

use crate::store::search::tokenize;

pub const DIM: usize = 384;

pub fn identity() -> &'static str {
    "hash-fnv-v1:384"
}

fn feature(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Embed `text` into a unit-length 384-d vector.
pub fn embed(text: &str) -> Vec<f32> {
    let tokens = tokenize(text);
    let mut vector = vec![0.0f32; DIM];
    if tokens.is_empty() {
        return vector;
    }

    let mut add = |term: &str, weight: f32| {
        let hash = feature(term);
        let index = (hash as usize) % DIM;
        // A sign bit keeps collisions from always reinforcing each other.
        let sign = if (hash >> 63) & 1 == 1 { -1.0 } else { 1.0 };
        vector[index] += sign * weight;
    };

    for token in &tokens {
        add(token, 1.0);
        let chars: Vec<char> = token.chars().collect();
        if chars.len() >= 3 {
            for window in chars.windows(3) {
                let trigram: String = window.iter().collect();
                add(&trigram, 0.35);
            }
        }
    }
    for pair in tokens.windows(2) {
        add(&format!("{} {}", pair[0], pair[1]), 0.5);
    }

    let norm = vector
        .iter()
        .map(|value| (*value as f64) * (*value as f64))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value = (*value as f64 / norm) as f32;
        }
    }
    vector
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum::<f64>() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_are_unit_length_and_stable() {
        let a = embed("the memory store is markdown");
        let b = embed("the memory store is markdown");
        assert_eq!(a, b);
        let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn related_text_scores_higher_than_unrelated() {
        let query = embed("markdown memory records");
        let near = embed("records are stored as markdown files");
        let far = embed("quantum chromodynamics lattice");
        assert!(cosine(&query, &near) > cosine(&query, &far));
    }
}
