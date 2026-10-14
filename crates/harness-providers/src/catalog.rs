use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub default_base_url: String,
    pub models_endpoint: String,
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
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            default_base_url: "https://api.anthropic.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::AnthropicApiKey,
        },
        ProviderDescriptor {
            id: "gemini".to_string(),
            display_name: "Google Gemini".to_string(),
            default_base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::QueryParam("key".to_string()),
        },
        ProviderDescriptor {
            id: "deepseek".to_string(),
            display_name: "DeepSeek".to_string(),
            default_base_url: "https://api.deepseek.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "groq".to_string(),
            display_name: "Groq".to_string(),
            default_base_url: "https://api.groq.com/openai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "ollama".to_string(),
            display_name: "Ollama (Local)".to_string(),
            default_base_url: "http://localhost:11434/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::None,
        },
        ProviderDescriptor {
            id: "openrouter".to_string(),
            display_name: "OpenRouter".to_string(),
            default_base_url: "https://openrouter.ai/api/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "together".to_string(),
            display_name: "Together AI".to_string(),
            default_base_url: "https://api.together.xyz/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "mistral".to_string(),
            display_name: "Mistral AI".to_string(),
            default_base_url: "https://api.mistral.ai/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
        ProviderDescriptor {
            id: "nvidia".to_string(),
            display_name: "NVIDIA NIM".to_string(),
            default_base_url: "https://integrate.api.nvidia.com/v1".to_string(),
            models_endpoint: "/models".to_string(),
            auth_header_format: AuthHeaderFormat::Bearer,
        },
    ]
}

pub fn find_provider(id: &str) -> Option<ProviderDescriptor> {
    get_provider_catalog().into_iter().find(|p| p.id.eq_ignore_ascii_case(id))
}
