//! Reading the model's JSON: finding the object, repairing it, and closing a truncated one.

use harness_core::HarnessError;

use super::model::{CandidateSet, Distilled};
use super::transcript::truncate;

/// Parse the model's curation answer, tolerating a fenced code block around it.
///
/// A model that answers in prose is a real failure — there is nothing to apply — so this returns an
/// error naming what it saw rather than silently dropping the pass.
pub(super) fn parse_distilled(text: &str) -> Result<Distilled, HarnessError> {
    let candidate = json_object(text, "background-saver")?;
    tracing::debug!("background saver answered: {}", truncate(&candidate, 2_000));
    serde_json::from_str::<Distilled>(&candidate).map_err(|error| HarnessError::ProviderError {
        provider: "background-saver".to_string(),
        message: format!(
            "could not parse the saver's JSON: {error} (saw: {})",
            truncate(candidate.trim(), 240)
        ),
    })
}

pub(super) fn parse_candidates(text: &str) -> Result<CandidateSet, HarnessError> {
    let candidate = json_object(text, "background-saver")?;
    tracing::debug!(
        "background saver scout answered: {}",
        truncate(&candidate, 2_000)
    );
    serde_json::from_str::<CandidateSet>(&candidate).map_err(|error| HarnessError::ProviderError {
        provider: "background-saver".to_string(),
        message: format!("could not parse the scout's JSON: {error}"),
    })
}

/// Strip a fence, find the best valid JSON object, and name the failure when there is none.
fn json_object(text: &str, provider: &str) -> Result<String, HarnessError> {
    let cleaned = strip_fence(text);
    // 1. Try balanced_object first
    if let Some(object) = balanced_object(&cleaned) {
        return Ok(object);
    }
    // 2. If balanced_object did not find a valid operations/candidates object, try repaired_object
    // which handles truncated responses by closing open brackets or rewinding past truncated items.
    if let Some(repaired) = repaired_object(&cleaned) {
        return Ok(repaired);
    }
    // 3. Fallback to repaired syntax of the first candidate if any
    if let Some(salvaged) = salvage_first_object(&cleaned) {
        return Ok(salvaged);
    }
    Err(HarnessError::ProviderError {
        provider: provider.to_string(),
        message: format!(
            "the saver did not answer with JSON: {}",
            truncate(text.trim(), 200)
        ),
    })
}

/// The last fallback: repair the first `{…}` that balances on its own, so an answer whose only
/// complete object the earlier passes declined is still readable.
fn salvage_first_object(text: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel_start) = text[search_from..].find('{') {
        let start = search_from + rel_start;
        if let Some(end) = find_balanced_end(text, start) {
            return Some(repair_json_syntax(&text[start..end]));
        }
        search_from = start + 1;
    }
    None
}

/// Extract the best complete `{…}` in `text`, bracket- and brace-balanced and string-aware.
///
/// Reasoning models sometimes echo prompt schema fragments like `{subject, predicate, object}` or
/// `{"operations": [...]}` in chain-of-thought before emitting the real JSON object at the end.
/// We scan all balanced top-level `{…}` blocks, repair minor JS key/comma syntax issues if needed,
/// and prefer valid JSON objects carrying `"operations"`, `"candidates"`, `"memories"`, or `"notes"`.
fn balanced_object(text: &str) -> Option<String> {
    let mut candidates = Vec::new();
    let mut search_from = 0;
    let mut last_end = 0;
    while let Some(rel_start) = text[search_from..].find('{') {
        let start = search_from + rel_start;
        if let Some(end) = find_balanced_end(text, start) {
            candidates.push(text[start..end].to_string());
            search_from = end;
            last_end = end;
        } else {
            search_from = start + 1;
        }
    }

    // If there is unclosed trailing text with '{' after the last balanced candidate,
    // a truncated multi-operation response may be present; let repaired_object handle it
    // instead of returning an earlier inner balanced object (e.g., an individual operation).
    let has_trailing_unclosed = text[last_end..].contains('{');

    let mut fallback_valid: Option<String> = None;
    for candidate in &candidates {
        for attempt in [candidate.clone(), repair_json_syntax(candidate)] {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(&attempt)
            {
                if (map.contains_key("operations")
                    || map.contains_key("candidates")
                    || map.contains_key("memories")
                    || map.contains_key("notes"))
                    && !has_trailing_unclosed
                {
                    return Some(attempt);
                }
                if fallback_valid.is_none() {
                    fallback_valid = Some(attempt);
                }
            }
        }
    }

    if !has_trailing_unclosed {
        if fallback_valid.is_some() {
            return fallback_valid;
        }
        if candidates.len() == 1 {
            return Some(repair_json_syntax(&candidates[0]));
        }
    }

    None
}

fn find_balanced_end(text: &str, start: usize) -> Option<usize> {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(ch) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(start + offset + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

/// Repairs common model JSON slips outside string literals:
/// 1. Unquoted identifier keys after `{` or `,` (e.g. `{subject: "x"}` -> `{"subject": "x"}`).
/// 2. Trailing commas before `}` or `]`.
fn repair_json_syntax(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + 32);
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;

    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }

        // Skip trailing comma before `}` or `]`
        if ch == ',' {
            let mut lookahead = i + 1;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if lookahead < chars.len() && matches!(chars[lookahead], '}' | ']') {
                i += 1;
                continue;
            }
        }

        if matches!(ch, '{' | ',') {
            out.push(ch);
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && (chars[i].is_ascii_alphabetic() || chars[i] == '_') {
                let key_start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '?'))
                {
                    i += 1;
                }
                let mut colon_idx = i;
                while colon_idx < chars.len() && chars[colon_idx].is_whitespace() {
                    colon_idx += 1;
                }
                if colon_idx < chars.len() && chars[colon_idx] == ':' {
                    let raw_key: String =
                        chars[key_start..i].iter().filter(|&&c| c != '?').collect();
                    out.push('"');
                    out.push_str(&raw_key);
                    out.push('"');
                    while i < colon_idx {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(':');
                    i = colon_idx + 1;
                    continue;
                } else {
                    for &c in &chars[key_start..i] {
                        out.push(c);
                    }
                    continue;
                }
            }
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

/// Close a truncated JSON object and return it only if the result parses.
///
/// When `max_tokens` cuts off a multi-operation response mid-key or mid-second-operation, simple
/// bracket closing leaves a dangling key or colon. In that case, we progressively rewind to
/// earlier safe boundaries (`}`, `]`, `"`, or `,`) so all completed operations before the cut
/// survive.
fn repaired_object(text: &str) -> Option<String> {
    let starts: Vec<usize> = text.match_indices('{').map(|(idx, _)| idx).collect();
    for start in starts {
        let slice = repair_json_syntax(&text[start..]);
        if let Some(closed) = close_and_validate(&slice) {
            return Some(closed);
        }
        // Progressively rewind to earlier safe boundaries in `slice` to drop any half-written
        // trailing operation or key-value pair while keeping completed ones.
        let cut_points: Vec<usize> = slice
            .char_indices()
            .filter_map(|(idx, ch)| {
                matches!(ch, '}' | ']' | ',' | '"').then_some(idx + ch.len_utf8())
            })
            .collect();
        for &cut in cut_points.iter().rev() {
            let prefix = slice[..cut].trim_end().trim_end_matches(',').trim_end();
            if prefix.is_empty() {
                continue;
            }
            if let Some(closed) = close_and_validate(prefix) {
                if let Ok(serde_json::Value::Object(map)) =
                    serde_json::from_str::<serde_json::Value>(&closed)
                {
                    if map.contains_key("operations")
                        || map.contains_key("candidates")
                        || map.contains_key("memories")
                        || map.contains_key("notes")
                    {
                        return Some(closed);
                    }
                }
            }
        }
    }
    None
}

fn close_and_validate(fragment: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in fragment.chars() {
        out.push(ch);
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' if stack.pop() != Some(ch) => {
                return None;
            }
            _ => {}
        }
    }
    if in_string {
        out.push('"');
    }
    out = repair_json_syntax(&out);
    while let Some(close) = stack.pop() {
        out.push(close);
    }
    out = repair_json_syntax(&out);
    serde_json::from_str::<serde_json::Value>(&out).ok()?;
    Some(out)
}

/// Strip a ```json fence, which models add even when told not to.
fn strip_fence(text: &str) -> String {
    let trimmed = text.trim();
    let without_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_open
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
        .to_string()
}
