//! Lexical (BM25) and hybrid retrieval over the in-memory record set.
//!
//! The lexical index is rebuilt from the Markdown records and never
//! persisted, so it cannot drift from the canonical store. Hybrid scoring
//! min-max normalizes each component over the candidate union, then blends
//! by the configured weights. A query with no lexical or vector hits falls
//! back to the caller's recency path.

use std::collections::HashMap;

use crate::store::embed;

const K1: f64 = 1.5;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "he",
    "her", "his", "i", "in", "is", "it", "its", "me", "my", "of", "on", "or", "our", "she", "that",
    "the", "their", "them", "they", "this", "to", "was", "we", "were", "what", "when", "where",
    "which", "who", "will", "with", "you", "your",
];

/// Lowercase, split on non-alphanumerics and camel-case boundaries, drop
/// one-letter fragments and stopwords.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && previous_lower && !current.is_empty() {
                push_token(&mut out, std::mem::take(&mut current));
            }
            for lower in c.to_lowercase() {
                current.push(lower);
            }
            previous_lower = c.is_lowercase();
        } else {
            push_token(&mut out, std::mem::take(&mut current));
            previous_lower = false;
        }
    }
    push_token(&mut out, current);
    out
}

fn push_token(out: &mut Vec<String>, token: String) {
    if token.chars().count() < 2 {
        return;
    }
    if STOPWORDS.contains(&token.as_str()) {
        return;
    }
    out.push(token);
}

struct Document {
    id: String,
    terms: HashMap<String, usize>,
    length: usize,
}

/// BM25 index over a fixed document set.
pub struct LexicalIndex {
    documents: Vec<Document>,
    document_frequency: HashMap<String, usize>,
    average_length: f64,
}

impl LexicalIndex {
    pub fn build(documents: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut built = Vec::new();
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut total_length = 0usize;
        for (id, text) in documents {
            let tokens = tokenize(&text);
            total_length += tokens.len();
            let mut terms: HashMap<String, usize> = HashMap::new();
            for token in tokens.iter() {
                *terms.entry(token.clone()).or_insert(0) += 1;
            }
            for term in terms.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
            built.push(Document {
                id,
                length: tokens.len(),
                terms,
            });
        }
        let average_length = if built.is_empty() {
            0.0
        } else {
            total_length as f64 / built.len() as f64
        };
        Self {
            documents: built,
            document_frequency: df,
            average_length,
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, f64)> {
        let query_terms = tokenize(query);
        if query_terms.is_empty() || self.documents.is_empty() {
            return Vec::new();
        }
        let total = self.documents.len() as f64;
        let mut scored: Vec<(String, f64)> = self
            .documents
            .iter()
            .filter_map(|document| {
                let mut score = 0.0f64;
                let mut matched = false;
                for term in &query_terms {
                    let Some(&frequency) = document.terms.get(term) else {
                        continue;
                    };
                    matched = true;
                    let df = *self.document_frequency.get(term).unwrap_or(&0) as f64;
                    let idf = ((total - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let tf = frequency as f64;
                    let denominator = tf
                        + K1 * (1.0 - B
                            + B * document.length as f64 / self.average_length.max(1.0));
                    score += idf * (tf * (K1 + 1.0)) / denominator;
                }
                if matched {
                    Some((document.id.clone(), score))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

/// A scored record with its component scores exposed for the CLI/JSON view.
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: String,
    pub score: f32,
    pub lexical: f32,
    pub vector: f32,
}

struct Candidate {
    id: String,
    lexical: f64,
    vector: f64,
}

/// Hybrid search. `documents` is `(id, text, precomputed vector)`.
pub fn hybrid(
    documents: &[(String, String, Vec<f32>)],
    query: &str,
    vector_weight: f32,
    lexical_weight: f32,
    limit: usize,
) -> Vec<Hit> {
    if documents.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }
    let query_vector = embed::embed(query);
    let lexical = LexicalIndex::build(
        documents
            .iter()
            .map(|(id, text, _)| (id.clone(), text.clone())),
    );
    let lexical_scores: HashMap<String, f64> =
        lexical.search(query, documents.len()).into_iter().collect();

    let mut candidates: Vec<Candidate> = documents
        .iter()
        .map(|(id, _, vector)| Candidate {
            id: id.clone(),
            lexical: *lexical_scores.get(id).unwrap_or(&0.0),
            vector: embed::cosine(&query_vector, vector).max(0.0) as f64,
        })
        .filter(|candidate| candidate.lexical > 0.0 || candidate.vector > 0.0)
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    let (lexical_min, lexical_max) = bounds(candidates.iter().map(|c| c.lexical));
    let (vector_min, vector_max) = bounds(candidates.iter().map(|c| c.vector));
    for candidate in &mut candidates {
        candidate.lexical = normalize(candidate.lexical, lexical_min, lexical_max);
        candidate.vector = normalize(candidate.vector, vector_min, vector_max);
    }
    candidates.sort_by(|a, b| {
        let a_score = a.vector * vector_weight as f64 + a.lexical * lexical_weight as f64;
        let b_score = b.vector * vector_weight as f64 + b.lexical * lexical_weight as f64;
        b_score
            .partial_cmp(&a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|candidate| Hit {
            id: candidate.id,
            score: (candidate.vector * vector_weight as f64
                + candidate.lexical * lexical_weight as f64) as f32,
            lexical: candidate.lexical as f32,
            vector: candidate.vector as f32,
        })
        .collect()
}

fn bounds(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for value in values {
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
    }
    if !min.is_finite() {
        (0.0, 0.0)
    } else {
        (min, max)
    }
}

fn normalize(value: f64, min: f64, max: f64) -> f64 {
    if (max - min).abs() < f64::EPSILON {
        if max > 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        (value - min) / (max - min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(id: &str, text: &str) -> (String, String, Vec<f32>) {
        (id.to_string(), text.to_string(), embed::embed(text))
    }

    #[test]
    fn bm25_finds_the_matching_document() {
        let index = LexicalIndex::build(vec![
            ("a".to_string(), "markdown memory store".to_string()),
            ("b".to_string(), "quantum lattice gauge".to_string()),
        ]);
        let hits = index.search("memory store", 5);
        assert_eq!(hits.first().map(|h| h.0.as_str()), Some("a"));
    }

    #[test]
    fn hybrid_ranks_relevant_first() {
        let docs = vec![
            document("a", "records are stored as markdown files"),
            document("b", "quantum chromodynamics lattice"),
        ];
        let hits = hybrid(&docs, "markdown records", 0.6, 0.4, 5);
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("a"));
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn tokenizer_splits_camel_case_and_drops_stopwords() {
        assert_eq!(
            tokenize("MemoryStore is a Markdown file"),
            vec!["memory", "store", "markdown", "file"]
        );
    }
}
