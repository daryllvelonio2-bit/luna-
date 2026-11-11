//! Normalization, verbatim and evidence matching, and the secret scan.

use super::model::{NoteItem, Operation};
use super::transcript::{Provenance, RenderedTranscript};

/// The tag a note carries when its only support is the assistant's own answer.
pub(super) const UNVERIFIED_TAG: &str = "unverified";

pub(super) fn mark_unverified(note: &mut NoteItem) {
    if !note_is_unverified(&note.tags) {
        note.tags.push(UNVERIFIED_TAG.to_string());
    }
}

pub(super) fn note_is_unverified(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| tag.eq_ignore_ascii_case(UNVERIFIED_TAG))
}

/// Remove decorations a model adds around an otherwise verbatim quote: a leading role label
/// (`USER:`, `Assistant —`) and one matched pair of surrounding quotes.
pub(super) fn strip_evidence_decoration(evidence: &str) -> &str {
    let mut text = evidence.trim();
    for label in ["user", "assistant", "luna", "system", "tool"] {
        if let Some(rest) = text.get(..label.len()) {
            if rest.eq_ignore_ascii_case(label) {
                let rest = text[label.len()..].trim_start_matches([' ', '\t', ':', '—', '-']);
                text = rest.trim();
                break;
            }
        }
    }
    for quote in ['"', '\'', '“', '‘'] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(closing_quote(quote)) {
            text = &text[quote.len_utf8()..text.len() - closing_quote(quote).len_utf8()];
            text = text.trim();
            break;
        }
    }
    // A model may mark a shortened quote with an ellipsis. The text before it is still verbatim.
    if let Some(rest) = text.strip_suffix("...") {
        text = rest.trim_end();
    } else if let Some(rest) = text.strip_suffix('…') {
        text = rest.trim_end();
    }
    text
}

fn closing_quote(open: char) -> char {
    match open {
        '“' => '”',
        '‘' => '’',
        other => other,
    }
}

/// Strip markdown heading lines (`# ...`).
pub(super) fn strip_headings(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a note body carries substantive text once headings and whitespace are removed.
pub(super) fn note_body_len(content: &str) -> usize {
    normalize(&strip_headings(content)).chars().count()
}

/// Whether a note body is made of user or tool words. Headings are ignored.
pub(super) fn note_has_authoritative_source(
    content: &str,
    transcript: &RenderedTranscript,
) -> bool {
    let body = strip_headings(content);
    matches!(
        transcript.provenance_of(&body),
        Some(Provenance::User | Provenance::Tool)
    )
}

/// Whether a note body is copied from the transcript rather than written from memory.
///
/// A note may add markdown headings (`# ...`) to label the copied span. The entire non-heading
/// body must appear as a single contiguous span in the transcript after whitespace normalization.
/// Stitching disconnected paragraphs from different turns or injecting invented sentences is
/// refused.
pub(super) fn note_is_verbatim(content: &str, transcript: &str) -> bool {
    let body = strip_headings(content);
    let body = normalize(&body);
    if body.is_empty() {
        return false;
    }
    let norm_transcript = normalize(transcript);
    if norm_transcript.contains(&body) {
        return true;
    }
    let stripped_transcript = normalize(&strip_headings(transcript));
    stripped_transcript.contains(&body)
}

/// Whether an evidence quote appears in the rendered transcript.
///
/// The comparison ignores case, runs of whitespace, and markdown inline formatting (such as
/// backticks or bold markers), and supports mid-quote ellipses (`...` or `…`) when each span
/// appears in order; it does not forgive a paraphrase, because a paraphrase is not evidence.
pub(super) fn evidence_supported(evidence: &str, transcript: &str) -> bool {
    segment_matches_quote(transcript, evidence)
}

/// Checks whether `haystack` contains `quote` after whitespace/case normalization, inline
/// markdown formatting normalization, or ordered ellipsis-split segments.
pub(super) fn segment_matches_quote(haystack: &str, quote: &str) -> bool {
    let norm_quote = normalize(quote);
    if norm_quote.is_empty() {
        return false;
    }
    let norm_haystack = normalize(haystack);
    if norm_haystack.contains(&norm_quote) {
        return true;
    }

    let stripped_quote = normalize_strip_formatting(quote);
    if stripped_quote
        .chars()
        .filter(|c| c.is_alphanumeric())
        .count()
        >= 4
    {
        let stripped_haystack = normalize_strip_formatting(haystack);
        if stripped_haystack.contains(&stripped_quote) {
            return true;
        }
    }

    if quote.contains("...") || quote.contains('…') {
        let parts: Vec<String> = quote
            .split("...")
            .flat_map(|s| s.split('…'))
            .map(normalize_strip_formatting)
            .filter(|p| p.chars().filter(|c| c.is_alphanumeric()).count() >= 4)
            .collect();
        if parts.len() >= 2 {
            let stripped_haystack = normalize_strip_formatting(haystack);
            let mut search_from = 0;
            let mut all_found = true;
            for part in &parts {
                if let Some(pos) = stripped_haystack[search_from..].find(part.as_str()) {
                    search_from += pos + part.len();
                } else {
                    all_found = false;
                    break;
                }
            }
            if all_found {
                return true;
            }
        }
    }

    false
}

/// Lowercase, strip inline markdown formatting characters (backticks, asterisks, quotes), and
/// collapse whitespace.
fn normalize_strip_formatting(text: &str) -> String {
    let filtered: String = text
        .chars()
        .map(|c| match c {
            '`' | '*' | '"' | '\'' | '“' | '”' | '‘' | '’' => ' ',
            other => other,
        })
        .collect();
    normalize(&filtered)
}

/// Lowercase and collapse whitespace.
pub(super) fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether any text field of an operation looks like a credential.
pub(super) fn operation_is_secret(operation: &Operation) -> bool {
    let mut fields: Vec<&str> = Vec::new();
    if let Some(evidence) = operation.evidence.as_deref() {
        fields.push(evidence);
    }
    if let Some(reason) = operation.reason.as_deref() {
        fields.push(reason);
    }
    if let Some(item) = operation.memory.as_ref() {
        fields.push(&item.subject);
        fields.push(&item.predicate);
        fields.push(&item.object);
        if let Some(name) = item.name.as_deref() {
            fields.push(name);
        }
        if let Some(path) = item.path.as_deref() {
            fields.push(path);
        }
    }
    if let Some(note) = operation.note.as_ref() {
        fields.push(&note.title);
        fields.push(&note.content);
    }
    fields.iter().any(|field| contains_secret(field))
}

/// A deliberately blunt credential scan: prefixes and key names that never belong in memory.
fn contains_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    const MARKERS: &[&str] = &[
        "api_key",
        "api-key",
        "apikey",
        "password=",
        "password:",
        "passwd",
        "bearer ",
        "authorization:",
        "private key",
        "-----begin",
        "secret_key",
        "access_token",
        "refresh_token",
        "client_secret",
    ];
    if MARKERS.iter().any(|marker| lower.contains(marker)) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "sk-", "ghp_", "gho_", "ghs_", "xoxb-", "xoxp-", "akia", "aiza", "glpat-",
    ];
    if PREFIXES.iter().any(|prefix| lower.contains(prefix)) {
        return true;
    }
    // A long token with digits and both cases is a credential far more often than a word.
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .any(|token| {
            token.len() >= 32
                && token.chars().any(|c| c.is_ascii_digit())
                && token.chars().any(|c| c.is_ascii_uppercase())
                && token.chars().any(|c| c.is_ascii_lowercase())
        })
}
