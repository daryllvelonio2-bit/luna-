//! Lexical (Okapi-BM25) and hybrid retrieval over the in-memory record set.
//!
//! Retrieval pipeline:
//! - Okapi-BM25 with smoothed corpus-relative IDF over the candidate set
//! - Min-max normalization on BM25 (`score / max_bm25`)
//! - Absolute vector similarity (cosine clamped to [0, 1]), never normalized
//!   relative to candidates to prevent candidate-set reshuffling
//! - Default weights: 0.6 vector, 0.4 BM25
//! - Deterministic tie-breaking: combined score desc, then vector distance desc.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::store::embed;

pub const DEFAULT_VECTOR_WEIGHT: f32 = 0.6;
pub const DEFAULT_BM25_WEIGHT: f32 = 0.4;

const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Stop words from the reference design's canonical English locale (`i18n/en.json`).
pub const STOPWORDS: &[&str] = &[
    "the",
    "this",
    "that",
    "these",
    "those",
    "some",
    "many",
    "most",
    "each",
    "every",
    "other",
    "only",
    "such",
    "very",
    "will",
    "would",
    "could",
    "should",
    "must",
    "shall",
    "yeah",
    "okay",
    "also",
    "even",
    "then",
    "now",
    "already",
    "still",
    "back",
    "done",
    "make",
    "take",
    "give",
    "know",
    "think",
    "want",
    "need",
    "going",
    "come",
    "find",
    "work",
    "added",
    "saved",
    "session",
    "summary",
    "conversation",
    "topics",
    "source",
    "about",
    "once",
    "just",
    "really",
    "actually",
    "here",
    "there",
    "where",
    "good",
    "great",
    "better",
    "thank",
    "please",
    "sorry",
    "right",
    "wrong",
    "true",
    "false",
    "a",
    "an",
    "and",
    "or",
    "but",
    "in",
    "on",
    "at",
    "to",
    "for",
    "of",
    "with",
    "by",
    "from",
    "as",
    "is",
    "was",
    "are",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "do",
    "does",
    "did",
    "may",
    "might",
    "can",
    "it",
    "its",
    "they",
    "them",
    "their",
    "we",
    "our",
    "you",
    "your",
    "i",
    "my",
    "me",
    "he",
    "she",
    "his",
    "her",
    "who",
    "what",
    "when",
    "why",
    "how",
    "which",
    "if",
    "so",
    "not",
    "no",
    "yes",
    "ok",
];

static TOKEN_RE: OnceLock<Regex> = OnceLock::new();

fn token_regex() -> &'static Regex {
    TOKEN_RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}_]{2,}").unwrap())
}

/// Lowercased word tokens of two or more characters, dropping canonical stop words.
/// Matches the reference design `lexical.py:tokenize` verbatim.
pub fn tokenize(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let re = token_regex();
    let stop_set: HashSet<&'static str> = STOPWORDS.iter().copied().collect();
    re.find_iter(&lower)
        .map(|m| m.as_str().to_string())
        .filter(|t| !stop_set.contains(t.as_str()))
        .collect()
}

struct Document {
    id: String,
    terms: HashMap<String, usize>,
    length: usize,
}

/// Okapi-BM25 index over a candidate document set.
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

    /// Okapi-BM25 scoring matching the reference design's `bm25_scores` formula.
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
    pub index_boost: f32,
    pub matched_via: String,
}

struct Candidate {
    id: String,
    lexical: f64,
    vector: f64,
    boost: f64,
}

/// Hybrid search re-ranking.
/// Matches the reference design `_hybrid_rank` (searcher/ranking.py:170) verbatim:
/// - Absolute vector similarity `vec_sim = cosine.max(0.0)` in [0, 1]
/// - Okapi-BM25 divided by `max_bm25` (0 to 1)
/// - Combined score: `vector_weight * vec_sim + bm25_weight * bm25_norm`
/// - Tie-break: combined score desc, then raw vector similarity desc
pub fn hybrid(
    documents: &[(String, String, Vec<f32>)],
    query: &str,
    vector_weight: f32,
    lexical_weight: f32,
    limit: usize,
) -> Vec<Hit> {
    hybrid_boosted(
        documents,
        &HashMap::new(),
        query,
        vector_weight,
        lexical_weight,
        limit,
    )
}

/// Hybrid search re-ranking with content index boost support.
pub fn hybrid_boosted(
    documents: &[(String, String, Vec<f32>)],
    boosts: &HashMap<String, f32>,
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
        .map(|(id, _, vector)| {
            let boost = *boosts.get(id).unwrap_or(&0.0f32) as f64;
            Candidate {
                id: id.clone(),
                lexical: *lexical_scores.get(id).unwrap_or(&0.0),
                // Absolute cosine similarity clamped to [0, 1], matching _distance_to_similarity
                vector: embed::cosine(&query_vector, vector).max(0.0) as f64,
                boost,
            }
        })
        .filter(|candidate| {
            candidate.lexical > 0.0 || candidate.vector > 0.0 || candidate.boost > 0.0
        })
        .collect();

    if candidates.is_empty() {
        return Vec::new();
    }

    // Min-max normalize BM25 only: max_bm25 scaling matches the reference design ranking.py:206-207
    let max_bm25 = candidates.iter().map(|c| c.lexical).fold(0.0f64, f64::max);

    candidates.sort_by(|a, b| {
        let a_bm25_norm = if max_bm25 > 0.0 {
            a.lexical / max_bm25
        } else {
            0.0
        };
        let b_bm25_norm = if max_bm25 > 0.0 {
            b.lexical / max_bm25
        } else {
            0.0
        };

        let a_vec_boosted = (a.vector + a.boost).clamp(0.0, 1.0);
        let b_vec_boosted = (b.vector + b.boost).clamp(0.0, 1.0);

        let a_score = a_vec_boosted * vector_weight as f64 + a_bm25_norm * lexical_weight as f64;
        let b_score = b_vec_boosted * vector_weight as f64 + b_bm25_norm * lexical_weight as f64;

        b_score
            .partial_cmp(&a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.vector
                    .partial_cmp(&a.vector)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|candidate| {
            let bm25_norm = if max_bm25 > 0.0 {
                candidate.lexical / max_bm25
            } else {
                0.0
            };
            let vec_boosted = (candidate.vector + candidate.boost).clamp(0.0, 1.0);
            let score = vec_boosted * vector_weight as f64 + bm25_norm * lexical_weight as f64;
            let matched_via = if candidate.boost > 0.0 {
                "record+index".to_string()
            } else {
                "record".to_string()
            };
            Hit {
                id: candidate.id,
                score: score as f32,
                lexical: candidate.lexical as f32,
                vector: candidate.vector as f32,
                index_boost: candidate.boost as f32,
                matched_via,
            }
        })
        .collect()
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
    fn index_boost_improves_ranking() {
        let docs = vec![
            document("a", "document alpha general discussion"),
            document("b", "document beta general discussion"),
        ];
        // Without boost, a and b tie or have similar scores
        let mut boosts = HashMap::new();
        boosts.insert("b".to_string(), 0.40f32);
        let hits = hybrid_boosted(&docs, &boosts, "general", 0.6, 0.4, 5);
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("b"));
        assert_eq!(hits[0].matched_via, "record+index");
        assert_eq!(hits[0].index_boost, 0.40);
    }

    #[test]
    fn tokenizer_drops_stopwords_and_keeps_words() {
        let tokens = tokenize("MemoryStore is a Markdown file");
        assert!(tokens.contains(&"memorystore".to_string()));
        assert!(tokens.contains(&"markdown".to_string()));
        assert!(tokens.contains(&"file".to_string()));
        assert!(!tokens.contains(&"is".to_string()));
    }
}
