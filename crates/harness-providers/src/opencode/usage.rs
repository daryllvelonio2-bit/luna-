use harness_core::types::TokenUsage;

/// Normalize OpenCode's `tokens` object into the crate's one usage shape.
///
/// OpenCode reports `input` as the *uncached* remainder and puts the cache read beside it under
/// `cache.read` — so `input` and `cache.read` add up to the prompt, they do not overlap. Reasoning
/// is billed as output but counted separately, so it joins `completion`. Folding both here keeps a
/// later cost, cache-share or context check from having to know which route wrote the row.
pub(super) fn usage_from_tokens(tokens: &serde_json::Value) -> TokenUsage {
    let count = |keys: &[&str]| -> u32 {
        for k in keys {
            if let Some(v) = tokens.get(*k).and_then(|v| v.as_u64()) {
                return v as u32;
            }
        }
        0
    };
    let prompt_tokens = count(&["input", "prompt", "prompt_tokens", "input_tokens"]);
    let completion_tokens = count(&["output", "completion", "completion_tokens", "output_tokens"])
        .saturating_add(count(&["reasoning", "reasoning_tokens"]));
    let total_tokens = count(&["total", "total_tokens"]);
    let cached_tokens = tokens
        .get("cache")
        .and_then(|c| c.get("read").or_else(|| c.get("read_tokens")))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or_else(|| count(&["cache_read", "cached_tokens", "cache_read_input_tokens"]));

    TokenUsage::with_separate_cache(
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
    )
}

/// Add one step's report to a call total.
///
/// The headless route emits one `step_finish` per inner step and no cumulative event, so the
/// ledger has to sum them itself. The context gauge keeps the per-step figure (only the newest
/// prompt is the context); only the bill is cumulative.
pub(super) fn add_usage(total: &mut Option<TokenUsage>, usage: &TokenUsage) {
    match total {
        Some(sum) => {
            sum.prompt_tokens = sum.prompt_tokens.saturating_add(usage.prompt_tokens);
            sum.completion_tokens = sum
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            sum.total_tokens = sum.total_tokens.saturating_add(usage.total_tokens);
            sum.cached_tokens = sum.cached_tokens.saturating_add(usage.cached_tokens);
        }
        None => *total = Some(usage.clone()),
    }
}

/// The size of the prompt one native step sent: what the context gauge may show.
///
/// Only `session.step.ended` carries this. `session.usage.updated` looks similar but is the
/// session's running total, so treating it as a context size made the gauge climb past the model
/// window as OpenCode's inner loop ran.
pub(super) fn step_context_usage(ev_type: &str, data: &serde_json::Value) -> Option<TokenUsage> {
    matches!(ev_type, "session.step.ended" | "session.next.step.ended")
        .then(|| parse_tokens(data))
        .flatten()
}

/// The usage to bill for the whole native call. This *is* the cumulative figure, and the session
/// is created per call, so the newest value describes every step OpenCode ran for it.
pub(super) fn session_total_usage(ev_type: &str, data: &serde_json::Value) -> Option<TokenUsage> {
    matches!(
        ev_type,
        "session.usage.updated" | "session.next.usage.updated"
    )
    .then(|| parse_tokens(data))
    .flatten()
}

pub(super) fn parse_tokens(part: &serde_json::Value) -> Option<TokenUsage> {
    if let Some(tokens) = part
        .get("tokens")
        .or_else(|| part.get("usage"))
        .or_else(|| part.get("tokenUsage"))
    {
        Some(usage_from_tokens(tokens))
    } else if part.get("input").is_some()
        || part.get("prompt_tokens").is_some()
        || part.get("total").is_some()
    {
        Some(usage_from_tokens(part))
    } else {
        None
    }
}
