//! What a model can actually be asked to do.
//!
//! The catalog has always known how much context a model takes; it said nothing about *kind*. So
//! a prompt carrying pasted screenshots was serialized into image content parts for every model
//! alike, and the tools list went to every model alike — a text-only model was handed an image
//! URL and a function schema it could not use, which at best wastes tokens and at worst fails the
//! request on the provider's own validation.
//!
//! Capabilities are therefore a property of the resolved model, resolved where the model is known
//! (`harness_providers::capability`), and consumed where the payload is built (the tool list in
//! the turn loop, the image parts in the provider client).
//!
//! **The defaults are conservative on purpose.** Vision and reasoning default to `false`: a model
//! nobody described is not assumed to see images, and an image sent to a blind model is the
//! failure that costs a turn. `tool_use` defaults to `true` only because it is the status quo —
//! every provider in the catalog has been fed tool schemas since the loop existed, and flipping
//! that default off would silently disable tools for every provider the resolver does not name.

use serde::{Deserialize, Serialize};

/// The four questions the harness asks before it builds a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    /// Accepts image content parts (vision).
    pub vision: bool,
    /// Accepts a `tools` array and can emit tool calls.
    pub tool_use: bool,
    /// Has a separate reasoning channel or reasoning-effort control.
    pub reasoning: bool,
    /// Caches the prompt prefix, so a stable prefix is worth building and cache hits are worth
    /// pricing. See `harness_core::usage` for the accounting side.
    pub prompt_cache: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            vision: false,
            tool_use: true,
            reasoning: false,
            prompt_cache: false,
        }
    }
}

impl ModelCapabilities {
    /// Nothing but text in, nothing but text out: no tools, no images.
    pub const fn text_only() -> Self {
        Self {
            vision: false,
            tool_use: false,
            reasoning: false,
            prompt_cache: false,
        }
    }

    /// The flags, for a log line or a status bar. Empty string when there are none, so a caller
    /// never has to render a dangling pair of empty parens.
    pub fn describe(&self) -> String {
        let mut flags = Vec::new();
        if self.vision {
            flags.push("vision");
        }
        if self.tool_use {
            flags.push("tools");
        }
        if self.reasoning {
            flags.push("reasoning");
        }
        if self.prompt_cache {
            flags.push("cache");
        }
        flags.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undescribed_model_is_not_assumed_to_see() {
        let caps = ModelCapabilities::default();
        assert!(!caps.vision, "vision must be an explicit claim");
        assert!(!caps.reasoning, "reasoning must be an explicit claim");
        assert!(!caps.prompt_cache, "caching must be an explicit claim");
        // Tools are the status quo: every provider the catalog ships has been given schemas.
        assert!(caps.tool_use);
    }

    #[test]
    fn describe_lists_only_the_flags_that_are_set() {
        assert_eq!(ModelCapabilities::text_only().describe(), "");
        assert_eq!(
            ModelCapabilities {
                vision: true,
                tool_use: true,
                ..ModelCapabilities::default()
            }
            .describe(),
            "vision, tools"
        );
        assert_eq!(
            ModelCapabilities {
                vision: true,
                tool_use: true,
                reasoning: true,
                prompt_cache: true,
            }
            .describe(),
            "vision, tools, reasoning, cache"
        );
    }

    #[test]
    fn capabilities_round_trip_through_json() {
        let caps = ModelCapabilities {
            vision: true,
            tool_use: false,
            reasoning: true,
            prompt_cache: true,
        };
        let encoded = serde_json::to_string(&caps).expect("capabilities serialize");
        let decoded: ModelCapabilities =
            serde_json::from_str(&encoded).expect("capabilities deserialize");
        assert_eq!(decoded, caps);
    }
}
