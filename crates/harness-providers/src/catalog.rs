use serde::{Deserialize, Serialize};

use harness_core::config::ProviderOverride;
use harness_core::HarnessError;

/// Built-in per-request HTTP timeout for provider calls. Overridable per provider through
/// `providers.<id>.timeout_secs` in `~/.luna/config.yaml`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub default_base_url: String,
    pub models_endpoint: String,
    pub timeout_secs: u64,
    pub auth_header_format: AuthHeaderFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthHeaderFormat {
    Bearer,               // Authorization: Bearer <key>
    AnthropicApiKey,      // x-api-key: <key>
    QueryParam(String),   // ?key=<key> (e.g. Gemini)
    None,                 // Ollama default local
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
        },
        ProviderDescriptor {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            default_base_url: "https://api.anthropic.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::AnthropicApiKey,
        },
        ProviderDescriptor {
            id: "gemini".to_string(),
            display_name: "Google Gemini".to_string(),
            default_base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::QueryParam("key".to_string()),
        },
        ProviderDescriptor {
            id: "deepseek".to_string(),
            display_name: "DeepSeek".to_string(),
            default_base_url: "https://api.deepseek.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "groq".to_string(),
            display_name: "Groq".to_string(),
            default_base_url: "https://api.groq.com/openai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "ollama".to_string(),
            display_name: "Ollama (Local)".to_string(),
            default_base_url: "http://localhost:11434/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::None,
        },
        ProviderDescriptor {
            id: "openrouter".to_string(),
            display_name: "OpenRouter".to_string(),
            default_base_url: "https://openrouter.ai/api/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "together".to_string(),
            display_name: "Together AI".to_string(),
            default_base_url: "https://api.together.xyz/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "mistral".to_string(),
            display_name: "Mistral AI".to_string(),
            default_base_url: "https://api.mistral.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "nvidia".to_string(),
            display_name: "NVIDIA NIM".to_string(),
            default_base_url: "https://integrate.api.nvidia.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "xai".to_string(),
            display_name: "xAI (Grok)".to_string(),
            default_base_url: "https://api.x.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "zai".to_string(),
            display_name: "Z.AI / GLM".to_string(),
            default_base_url: "https://api.z.ai/api/paas/v4".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "alibaba".to_string(),
            display_name: "Alibaba Cloud / Qwen".to_string(),
            default_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "moonshot".to_string(),
            display_name: "Moonshot / Kimi".to_string(),
            default_base_url: "https://api.moonshot.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "minimax".to_string(),
            display_name: "MiniMax".to_string(),
            default_base_url: "https://api.minimax.io/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "huggingface".to_string(),
            display_name: "Hugging Face".to_string(),
            default_base_url: "https://router.huggingface.co/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "copilot".to_string(),
            display_name: "GitHub Models".to_string(),
            default_base_url: "https://models.inference.ai.azure.com".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "lmstudio".to_string(),
            display_name: "LM Studio (Local)".to_string(),
            default_base_url: "http://127.0.0.1:1234/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "stepfun".to_string(),
            display_name: "StepFun".to_string(),
            default_base_url: "https://api.stepfun.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "arcee".to_string(),
            display_name: "Arcee AI".to_string(),
            default_base_url: "https://api.arcee.ai/api/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "gmi".to_string(),
            display_name: "GMI Cloud".to_string(),
            default_base_url: "https://api.gmi-serving.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "kilocode".to_string(),
            display_name: "Kilo Code".to_string(),
            default_base_url: "https://api.kilo.ai/api/gateway".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "ai-gateway".to_string(),
            display_name: "Vercel AI Gateway".to_string(),
            default_base_url: "https://ai-gateway.vercel.sh/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "custom".to_string(),
            display_name: "Custom (OpenAI-compatible)".to_string(),
            default_base_url: "http://localhost:8000/v1".to_string(),
            models_endpoint: "/models".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "antigravity".to_string(),
            display_name: "Google Antigravity".to_string(),
            default_base_url: "https://daily-cloudcode-pa.googleapis.com/v1internal".to_string(),
            models_endpoint: "/fetchAvailableModels".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            auth_header_format: AuthHeaderFormat::Bearer,
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
        "antigravity" | "agy" => "gemini-3.8-flash-tiered",
        _ => "default",
    }
}
