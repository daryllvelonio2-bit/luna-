//! Which models can see, call tools, think, or cache — resolved from `provider` + `model`.
//!
//! The catalog answers "how much context does this model take" by looking at the provider's own
//! entry first, then any other provider's entry for the same model, then a family heuristic.
//! Capabilities are resolved the same way and for the same reason: an aggregator's deployment of
//! a model is not a different model, and the leaf name (`anthropic/claude-3.5-sonnet` →
//! `claude-3.5-sonnet`) is the most reliable part of the string.
//!
//! Two rules keep this honest:
//!
//! * **A capability is claimed, never assumed.** Unknown models fall back to
//!   [`ModelCapabilities::default`] — no vision, no reasoning, no caching — so an image is only
//!   ever sent to a model that is known to accept one.
//! * **The cache is a billing property, not just a feature.** `prompt_cache` is claimed only where
//!   the provider named is known to cache, because a provider that charges for a marker it does
//!   not honour is worse than one that does not advertise it.
//!
//! Everything here is a claim about the *model*, so the tests read like a specification: extend
//! them with the model that proved you wrong.

use harness_core::ModelCapabilities;

use crate::catalog::get_default_model;

/// Everything the resolver knows about one model, from its leaf name.
fn from_leaf(leaf: &str) -> ModelCapabilities {
    let mut caps = ModelCapabilities {
        tool_use: true,
        ..ModelCapabilities::default()
    };

    // --- Vision -------------------------------------------------------------------------------
    // Named multimodal lines. `-vl`/`vision` cover the open-weight vision models (`*-vl`,
    // `phi-3-vision`, `llama-3.2-*-vision`); the vendor lines below are multimodal across the
    // board, which is why they are matched by family rather than one model at a time.
    let vision = leaf.contains("gpt-4o")
        || leaf.contains("gpt-4.1")
        || leaf.contains("gpt-4-turbo")
        || leaf.contains("gpt-5")
        || leaf.contains("o1")
        || leaf.contains("o3")
        || leaf.contains("o4")
        || leaf.contains("gemini")
        || leaf.contains("claude")
        || leaf.contains("pixtral")
        || leaf.contains("llava")
        || leaf.contains("vision")
        || leaf.contains("-vl")
        || leaf.contains("vl-")
        || leaf.contains("qwen-vl");
    caps.vision = vision;

    // --- Tool use -----------------------------------------------------------------------------
    // Disabling exceptions only: the classifier/completion-only models that were never trained to
    // emit a function call. A model that is merely *unverified* keeps the status-quo default.
    let no_tools = leaf.contains("llama-guard")
        || leaf.contains("starcoder")
        || leaf.contains("llama2")
        || leaf.contains("codellama")
        || leaf.contains("granite-3.0-3b-a800m")
        || leaf.contains("deepseek-coder-6.7b");
    if no_tools {
        caps.tool_use = false;
    }

    // --- Reasoning ----------------------------------------------------------------------------
    // A separate reasoning channel (or a reasoning-effort control) exists on the o-series and the
    // explicitly-reasoning open models. `-thinking`/`-reasoner`/`r1` are the naming conventions.
    let reasoning = leaf.contains("o1")
        || leaf.contains("o3")
        || leaf.contains("o4")
        || leaf.contains("gpt-5")
        || leaf.contains("deepseek-r1")
        || leaf.contains("deepseek-v4")
        || leaf.contains("deepseek-v4.1")
        || leaf.contains("reasoner")
        || leaf.contains("reasoning")
        || leaf.contains("-thinking")
        || leaf.contains("qwen3")
        || leaf.contains("glm-z1")
        || leaf.contains("gemini-2.5")
        || leaf.contains("gemini-3");
    caps.reasoning = reasoning;

    // --- Prompt caching -----------------------------------------------------------------------
    // Only where the model line is documented to cache a stable prefix (implicitly, or through
    // Anthropic's explicit breakpoints). Resolved against the provider below as well.
    let cache_family = leaf.contains("gpt-4o")
        || leaf.contains("gpt-4.1")
        || leaf.contains("gpt-5")
        || leaf.contains("o1")
        || leaf.contains("o3")
        || leaf.contains("o4")
        || leaf.contains("gemini")
        || leaf.contains("claude")
        || leaf.contains("deepseek")
        // GPT-OSS is served behind the same implicit cache on Code Assist: two identical 8.8k
        // prompts billed 64 cached tokens on the first turn and 8,768 on the second. The route
        // gate below keeps this honest elsewhere — a non-caching route still clears the flag.
        || leaf.contains("gpt-oss");
    caps.prompt_cache = cache_family;

    caps
}

/// Resolve the capabilities of `model` as served by `provider`.
pub fn resolve(provider: &str, model: &str) -> ModelCapabilities {
    let leaf = model
        .trim()
        .to_lowercase()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if leaf.is_empty() {
        return ModelCapabilities::default();
    }

    let mut caps = from_leaf(&leaf);

    // The model family caching is not the same claim as the *route* serving it caching. The
    // route's answer lives with the provider descriptors (`catalog::route_caches_prompt_prefix`)
    // so that a new provider is described in one place.
    if !crate::catalog::route_caches_prompt_prefix(provider) {
        caps.prompt_cache = false;
    }

    caps
}

/// Capabilities of the model a provider defaults to, for callers that hold only a provider id.
pub fn resolve_default(provider: &str) -> ModelCapabilities {
    resolve(provider, get_default_model(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vision_model_is_told_apart_from_a_text_only_one() {
        let sees = resolve("openai", "gpt-4o");
        assert!(sees.vision && sees.tool_use && sees.prompt_cache);
        // 4o is not a reasoning model, whatever its sibling o-series is.
        assert!(!sees.reasoning);

        let deepseek = resolve("deepseek", "deepseek-chat");
        assert!(!deepseek.vision, "the deepseek chat line is text-only");
        assert!(deepseek.tool_use);

        // The open-weight vision lines are recognized by name, not by vendor.
        assert!(resolve("nvidia", "meta/llama-3.2-90b-vision-instruct").vision);
        assert!(resolve("mistral", "pixtral-large-latest").vision);
        assert!(resolve("nvidia", "nvidia/nemotron-3.5-super-vl-120b-a12b").vision);
    }

    #[test]
    fn aggregator_leaf_names_resolve_like_the_model_they_serve() {
        let through_openrouter = resolve("openrouter", "anthropic/claude-3.5-sonnet");
        let direct = resolve("anthropic", "claude-3-5-sonnet-20241022");
        assert_eq!(through_openrouter.vision, direct.vision);
        assert_eq!(through_openrouter.tool_use, direct.tool_use);
    }

    #[test]
    fn classifier_and_completion_models_are_not_given_tools() {
        assert!(!resolve("nvidia", "meta/llama-guard-4-12b").tool_use);
        assert!(!resolve("nvidia", "bigcode/starcoder2-15b").tool_use);
        assert!(!resolve("nvidia", "meta/codellama-70b").tool_use);
    }

    #[test]
    fn reasoning_models_are_named_not_guessed() {
        assert!(resolve("openai", "o3").reasoning);
        assert!(resolve("deepseek", "deepseek-reasoner").reasoning);
        assert!(resolve("google", "gemini-2.5-pro").reasoning);
        // A plain chat model is not credited with a reasoning channel.
        assert!(!resolve("openai", "gpt-4o-mini").reasoning);
        assert!(!resolve("anthropic", "claude-3-5-sonnet-20241022").reasoning);
    }

    #[test]
    fn an_unknown_model_claims_nothing_but_keeps_tools() {
        let unknown = resolve("custom", "some-house-model");
        assert_eq!(unknown, ModelCapabilities::default());
        assert!(!unknown.vision);
        assert!(!unknown.reasoning);
        assert!(!unknown.prompt_cache);

        // No model at all is not a licence to assume anything.
        assert_eq!(resolve("openai", ""), ModelCapabilities::default());
    }

    #[test]
    fn caching_is_only_claimed_where_the_route_honours_it() {
        assert!(resolve("openai", "gpt-4o").prompt_cache);
        assert!(resolve("deepseek", "deepseek-chat").prompt_cache);
        // The Claude models cache, but the route this crate uses for them does not: Anthropic's
        // OpenAI-compatible layer documents prompt caching as unsupported.
        assert!(!resolve("anthropic", "claude-sonnet-4").prompt_cache);
        // Same model, aggregator route: the family caches, the route may not, so we do not claim it.
        assert!(!resolve("openrouter", "openai/gpt-4o").prompt_cache);
    }

    /// A route that reports cache hits must not be denied the flag for living under a different
    /// id than the one that was written down. Antigravity reports `cachedContentTokenCount` and
    /// Supercode reports `cachedTokens`; both were absent from the old list, so the status bar
    /// said "no cache" for a provider whose ledger already recorded hits.
    #[test]
    fn every_route_that_reports_cache_reads_is_credited_with_them() {
        for (provider, model) in [
            ("antigravity", "gemini-3.8-flash"),
            // Supercode serves several families; the point is that the *route* does not deny the
            // claim its own usage payload already made by reporting `cachedTokens`.
            ("supercode", "claude-sonnet-4"),
            ("gemini", "gemini-2.5-flash"),
            // OpenCode's native service reports `cache.read` and the ledger records it; both ids
            // (native and the headless CLI) share that streaming path.
            ("opencode", "deepseek-v4.1-flash-free"),
            ("opencode-cli", "opencode/deepseek-v4.1-flash-free"),
        ] {
            assert!(
                resolve(provider, model).prompt_cache,
                "{provider} reports cache reads, so the flag must not contradict the ledger"
            );
        }

        // The alias is the same route, so it answers the same way — without a second list entry.
        assert_eq!(
            resolve("agy", "gemini-2.5-pro").prompt_cache,
            resolve("antigravity", "gemini-2.5-pro").prompt_cache
        );
        assert_eq!(
            resolve("kiro-ai", "claude-sonnet-4").prompt_cache,
            resolve("kiro", "claude-sonnet-4").prompt_cache
        );
    }

    /// Every model the Antigravity route publishes is served behind the same implicit prefix
    /// cache, so no entry may be denied the flag its own ledger already proves. `gpt-oss-120b`
    /// was: a live probe recorded 8,768 cached of 8,790 prompt tokens while the family list knew
    /// only about Gemini and Claude.
    #[test]
    fn every_model_on_the_antigravity_route_is_credited_with_the_cache() {
        for model in crate::antigravity::ANTIGRAVITY_MODELS {
            assert!(
                resolve("antigravity", model).prompt_cache,
                "{model} is served by Code Assist, which caches the prefix"
            );
        }
    }

    #[test]
    fn the_provider_default_model_resolves_to_something_sensible() {
        assert!(resolve_default("openai").vision);
        assert!(resolve_default("anthropic").tool_use);
    }
}
