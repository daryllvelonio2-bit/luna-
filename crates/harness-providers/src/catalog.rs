use serde::{Deserialize, Serialize};

use harness_core::config::ProviderOverride;
use harness_core::HarnessError;

/// Built-in per-request HTTP timeout for provider calls. Overridable per provider through
/// `providers.<id>.timeout_secs` in `~/.luna/config.yaml`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub default_base_url: String,
    pub models_endpoint: String,
    pub timeout_secs: u64,
    pub auth_header_format: AuthHeaderFormat,
    /// Per-model context window (max tokens). Key = model id, value = max tokens.
    /// If a model isn't listed, the caller should fall back to a sensible default.
    pub context_windows: std::collections::HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthHeaderFormat {
    Bearer,             // Authorization: Bearer <key>
    AnthropicApiKey,    // x-api-key: <key>
    QueryParam(String), // ?key=<key> (e.g. Gemini)
    None,               // Ollama default local
}

fn ctx_windows(map: &[(&str, u32)]) -> std::collections::HashMap<String, u32> {
    map.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

pub fn get_provider_catalog() -> Vec<ProviderDescriptor> {
    vec![
        ProviderDescriptor {
            id: "openai".to_string(),
            display_name: "OpenAI".to_string(),
            default_base_url: "https://api.openai.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("gpt-4o", 128_000),
                ("gpt-4o-mini", 128_000),
                ("gpt-4-turbo", 128_000),
                ("gpt-4", 8_192),
                ("gpt-4-32k", 32_768),
                ("gpt-3.5-turbo", 16_385),
                ("gpt-3.5-turbo-16k", 16_385),
                ("o1-preview", 128_000),
                ("o1-mini", 128_000),
                ("o3-mini", 200_000),
            ]),
        },
        ProviderDescriptor {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            default_base_url: "https://api.anthropic.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::AnthropicApiKey,
            context_windows: ctx_windows(&[
                ("claude-3-5-sonnet-20241022", 200_000),
                ("claude-3-5-haiku-20241022", 200_000),
                ("claude-3-opus-20240229", 200_000),
                ("claude-3-sonnet-20240229", 200_000),
                ("claude-3-haiku-20240307", 200_000),
            ]),
        },
        ProviderDescriptor {
            id: "gemini".to_string(),
            display_name: "Google Gemini".to_string(),
            default_base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::QueryParam("key".to_string()),
            context_windows: ctx_windows(&[
                ("gemini-1.5-pro", 2_000_000),
                ("gemini-1.5-flash", 1_000_000),
                ("gemini-1.0-pro", 32_768),
                ("gemini-2.0-flash-exp", 1_000_000),
            ]),
        },
        ProviderDescriptor {
            id: "deepseek".to_string(),
            display_name: "DeepSeek".to_string(),
            default_base_url: "https://api.deepseek.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("deepseek-chat", 1_000_000),
                ("deepseek-reasoner", 1_000_000),
                ("deepseek-v4.1-flash", 1_000_000),
                ("deepseek-v4-flash", 1_000_000),
                ("deepseek-coder", 128_000),
            ]),
        },
        ProviderDescriptor {
            id: "groq".to_string(),
            display_name: "Groq".to_string(),
            default_base_url: "https://api.groq.com/openai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("llama-3.3-70b-versatile", 128_000),
                ("llama-3.1-70b-versatile", 128_000),
                ("llama-3.1-8b-instant", 128_000),
                ("mixtral-8x7b-32768", 32_768),
                ("gemma2-9b-it", 8_192),
            ]),
        },
        ProviderDescriptor {
            id: "ollama".to_string(),
            display_name: "Ollama (Local)".to_string(),
            default_base_url: "http://localhost:11434/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::None,
            context_windows: ctx_windows(&[
                ("llama3.2", 128_000),
                ("llama3.1", 128_000),
                ("llama3", 8_192),
                ("mistral", 32_768),
                ("codellama", 16_384),
                ("qwen2.5", 128_000),
                ("phi3", 16_384),
                ("gemma2", 8_192),
            ]),
        },
        ProviderDescriptor {
            id: "openrouter".to_string(),
            display_name: "OpenRouter".to_string(),
            default_base_url: "https://openrouter.ai/api/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("deepseek/deepseek-v4.1-flash", 1_000_000),
                ("deepseek/deepseek-v4-flash", 1_000_000),
                ("deepseek/deepseek-v4-pro", 1_000_000),
                ("anthropic/claude-3.5-sonnet", 200_000),
                ("openai/gpt-4o", 128_000),
                ("google/gemini-pro-1.5", 2_000_000),
                ("meta-llama/llama-3.3-70b-instruct", 128_000),
            ]),
        },
        ProviderDescriptor {
            id: "together".to_string(),
            display_name: "Together AI".to_string(),
            default_base_url: "https://api.together.xyz/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("meta-llama/Llama-3.3-70B-Instruct-Turbo", 128_000),
                ("meta-llama/Llama-3.1-70B-Instruct-Turbo", 128_000),
                ("Qwen/Qwen2.5-72B-Instruct", 128_000),
            ]),
        },
        ProviderDescriptor {
            id: "mistral".to_string(),
            display_name: "Mistral AI".to_string(),
            default_base_url: "https://api.mistral.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("mistral-large-latest", 128_000),
                ("pixtral-large-latest", 128_000),
                ("codestral-latest", 32_768),
                ("ministral-8b-latest", 128_000),
            ]),
        },
        ProviderDescriptor {
            id: "codestral".to_string(),
            display_name: "Mistral Codestral".to_string(),
            default_base_url: "https://codestral.mistral.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("codestral-latest", 256_000),
                ("codestral-2501", 256_000),
            ]),
        },
        ProviderDescriptor {
            id: "nvidia".to_string(),
            display_name: "NVIDIA NIM".to_string(),
            default_base_url: "https://integrate.api.nvidia.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("deepseek-ai/deepseek-v4.1-flash", 1_000_000),
                ("deepseek-ai/deepseek-v4-flash", 1_000_000),
                ("deepseek-ai/deepseek-v4-pro", 262_144),
                ("z-ai/glm-5.3-flash", 1_000_000),
                ("z-ai/glm-4-9b-chat", 1_000_000),
                ("stepfun-ai/step-5-preview", 1_000_000),
                ("meta/llama-3.1-70b-instruct", 128_000),
                ("meta/llama-3.1-8b-instruct", 128_000),
                ("nvidia/nemotron-3-ultra", 128_000),
                ("mistralai/mistral-large", 128_000),
            ]),
        },
        ProviderDescriptor {
            id: "xai".to_string(),
            display_name: "xAI (Grok)".to_string(),
            default_base_url: "https://api.x.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("grok-2-latest", 131_072),
                ("grok-beta", 131_072),
            ]),
        },
        ProviderDescriptor {
            id: "zai".to_string(),
            display_name: "Z.AI / GLM".to_string(),
            default_base_url: "https://api.z.ai/api/paas/v4".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("glm-4-plus", 1_000_000),
                ("glm-4-long", 1_000_000),
                ("glm-5.3-flash", 1_000_000),
            ]),
        },
        ProviderDescriptor {
            id: "alibaba".to_string(),
            display_name: "Alibaba Cloud / Qwen".to_string(),
            default_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[("qwen-plus", 128_000)]),
        },
        ProviderDescriptor {
            id: "moonshot".to_string(),
            display_name: "Moonshot / Kimi".to_string(),
            default_base_url: "https://api.moonshot.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[("moonshot-v1-8k", 8_192)]),
        },
        ProviderDescriptor {
            id: "minimax".to_string(),
            display_name: "MiniMax".to_string(),
            default_base_url: "https://api.minimax.io/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[("abab6.5s-chat", 245_760)]),
        },
        ProviderDescriptor {
            id: "huggingface".to_string(),
            display_name: "Hugging Face".to_string(),
            default_base_url: "https://router.huggingface.co/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[("meta-llama/Llama-3.1-70B-Instruct", 128_000)]),
        },
        ProviderDescriptor {
            id: "copilot".to_string(),
            display_name: "GitHub Models".to_string(),
            default_base_url: "https://models.inference.ai.azure.com".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[("gpt-4o", 128_000)]),
        },
        ProviderDescriptor {
            id: "lmstudio".to_string(),
            display_name: "LM Studio (Local)".to_string(),
            default_base_url: "http://127.0.0.1:1234/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "stepfun".to_string(),
            display_name: "StepFun".to_string(),
            default_base_url: "https://api.stepfun.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "arcee".to_string(),
            display_name: "Arcee AI".to_string(),
            default_base_url: "https://api.arcee.ai/api/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "gmi".to_string(),
            display_name: "GMI Cloud".to_string(),
            default_base_url: "https://api.gmi-serving.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "kilocode".to_string(),
            display_name: "Kilo Code".to_string(),
            default_base_url: "https://api.kilo.ai/api/gateway".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "ai-gateway".to_string(),
            display_name: "Vercel AI Gateway".to_string(),
            default_base_url: "https://ai-gateway.vercel.sh/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "custom".to_string(),
            display_name: "Custom (OpenAI-compatible)".to_string(),
            default_base_url: "http://localhost:8000/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: std::collections::HashMap::new(),
        },
        ProviderDescriptor {
            id: "antigravity".to_string(),
            display_name: "Google Antigravity".to_string(),
            default_base_url: "https://daily-cloudcode-pa.googleapis.com/v1internal".to_string(),
            models_endpoint: "/fetchAvailableModels".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
            context_windows: ctx_windows(&[
                ("gemini-3.8-flash", 1_000_000),
                ("claude-sonnet-4-6", 200_000),
                ("claude-opus-4-6", 200_000),
                ("gpt-oss-120b", 128_000),
            ]),
        },
    ]
}

pub fn find_provider(id: &str) -> Option<ProviderDescriptor> {
    get_provider_catalog().into_iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// Every provider id the catalog knows, for error messages.
pub fn known_provider_ids() -> Vec<String> {
    get_provider_catalog().into_iter().map(|p| p.id).collect()
}

/// The catalog entry for `id` with the user's `providers.<id>.*` config applied.
///
/// An unknown id is an error instead of a silent fallback to another vendor: a typo in
/// `model.provider` used to send the request — and the API key — to OpenAI's base URL. Provider
/// ids stay case-insensitive, as `find_provider` has always treated them.
pub fn resolve_descriptor(
    id: &str,
    overrides: Option<&ProviderOverride>,
) -> Result<ProviderDescriptor, HarnessError> {
    let mut descriptor = find_provider(id).ok_or_else(|| {
        HarnessError::ConfigurationError(format!(
            "unknown provider '{}': known providers are {}",
            id,
            known_provider_ids().join(", ")
        ))
    })?;

    if let Some(overrides) = overrides {
        if let Some(base_url) = &overrides.base_url {
            descriptor.default_base_url = base_url.clone();
        }
        if let Some(models_endpoint) = &overrides.models_endpoint {
            descriptor.models_endpoint = models_endpoint.clone();
        }
        if let Some(timeout_secs) = overrides.timeout_secs {
            descriptor.timeout_secs = timeout_secs;
        }
    }

    Ok(descriptor)
}

/// Returns the conventional default model for a given provider.
pub fn get_default_model(provider: &str) -> &'static str {
    match provider.to_lowercase().as_str() {
        "nvidia" => "deepseek-ai/deepseek-v4.1-flash",
        "deepseek" => "deepseek-chat",
        "anthropic" => "claude-3-5-sonnet-20241022",
        "gemini" => "gemini-1.5-pro",
        "groq" => "llama-3.3-70b-versatile",
        "ollama" => "llama3.2",
        "openrouter" => "anthropic/claude-3.5-sonnet",
        "together" => "meta-llama/Llama-3-70b-chat-hf",
        "mistral" => "mistral-large-latest",
        "xai" => "grok-2-latest",
        "zai" => "glm-4-plus",
        "alibaba" => "qwen-plus",
        "moonshot" => "moonshot-v1-8k",
        "minimax" => "abab6.5s-chat",
        "huggingface" => "meta-llama/Llama-3.1-70B-Instruct",
        "copilot" => "gpt-4o",
        "openai" => "gpt-4o",
        "antigravity" | "agy" => "gemini-3.8-flash",
        _ => "default",
    }
}

/// Get the context window for a provider+model combination.
/// Resolves hierarchically:
/// 1. Exact or leaf match in the provider's declared context_windows
/// 2. Cross-provider catalog lookup
/// 3. Universal model family architecture heuristics (DeepSeek V4 = 1M, Gemini = 1M/2M, Claude = 200k)
pub fn get_context_window(provider: &str, model: &str) -> Option<u32> {
    let model_lower = model.to_lowercase();
    let model_trimmed = model_lower.trim();

    // 1. Direct match or leaf match in the chosen provider descriptor
    if let Some(desc) = find_provider(provider) {
        for (k, v) in &desc.context_windows {
            if k.eq_ignore_ascii_case(model_trimmed) {
                return Some(*v);
            }
        }
        let leaf = model_trimmed.rsplit('/').next().unwrap_or(model_trimmed);
        for (k, v) in &desc.context_windows {
            let k_leaf = k.rsplit('/').next().unwrap_or(k);
            if k.eq_ignore_ascii_case(leaf) || k_leaf.eq_ignore_ascii_case(leaf) {
                return Some(*v);
            }
        }
    }

    // 2. Cross-provider catalog lookup
    for desc in get_provider_catalog() {
        for (k, v) in &desc.context_windows {
            if k.eq_ignore_ascii_case(model_trimmed) {
                return Some(*v);
            }
            let k_leaf = k.rsplit('/').next().unwrap_or(k);
            let leaf = model_trimmed.rsplit('/').next().unwrap_or(model_trimmed);
            if k_leaf.eq_ignore_ascii_case(leaf) {
                return Some(*v);
            }
        }
    }

    // 3. Known architectural model families & token heuristics (matching Shiina model_metadata)
    let leaf = model_trimmed.rsplit('/').next().unwrap_or(model_trimmed);

    // DeepSeek family (V4 = 1M, V3/R1 = 128k)
    if leaf.contains("deepseek") {
        if leaf.contains("v4") || leaf.contains("flash") || leaf.contains("chat") {
            if leaf.contains("deepseek-v4-pro") && provider.eq_ignore_ascii_case("nvidia") {
                return Some(262_144);
            }
            return Some(1_000_000);
        }
        return Some(128_000);
    }

    // Gemini 1.5/2.x/3.x family
    if leaf.contains("gemini") {
        if leaf.contains("pro") {
            return Some(2_000_000);
        }
        return Some(1_000_000);
    }

    // Claude family
    if leaf.contains("claude") {
        return Some(200_000);
    }

    // GLM family (GLM-4 / GLM-5, z-ai/glm-5.3-flash, etc.) is 1,000,000 (1M tokens)
    if leaf.contains("glm") {
        return Some(1_000_000);
    }

    // StepFun family is 1,000,000 (1M tokens)
    if leaf.contains("step-") || leaf.contains("stepfun") {
        return Some(1_000_000);
    }

    // Llama 3 / GPT-4o / Grok / Qwen / Mistral
    if leaf.contains("gpt-4o")
        || leaf.contains("o1")
        || leaf.contains("o3")
        || leaf.contains("llama-3")
        || leaf.contains("llama3")
        || leaf.contains("llama-4")
        || leaf.contains("grok")
        || leaf.contains("qwen")
        || leaf.contains("mistral")
        || leaf.contains("codestral")
    {
        return Some(128_000);
    }

    if leaf.contains("kimi") || leaf.contains("moonshot") {
        return Some(1_000_000);
    }

    if leaf.contains("minimax") {
        return Some(245_760);
    }

    Some(128_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_window_1m_models() {
        assert_eq!(get_context_window("nvidia", "z-ai/glm-5.3-flash"), Some(1_000_000));
        assert_eq!(get_context_window("nvidia", "stepfun-ai/step-5-preview"), Some(1_000_000));
        assert_eq!(get_context_window("zai", "glm-4-plus"), Some(1_000_000));
        assert_eq!(get_context_window("other", "glm-4-long"), Some(1_000_000));
        assert_eq!(get_context_window("other", "step-2"), Some(1_000_000));
    }
}
