use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CostMetrics {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Default)]
pub struct TokenTracker {
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
}

impl TokenTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_usage(&self, prompt: u32, completion: u32) {
        self.prompt_tokens.fetch_add(prompt as u64, Ordering::Relaxed);
        self.completion_tokens.fetch_add(completion as u64, Ordering::Relaxed);
    }

    pub fn metrics(&self) -> CostMetrics {
        let p = self.prompt_tokens.load(Ordering::Relaxed);
        let c = self.completion_tokens.load(Ordering::Relaxed);
        let total = p + c;
        // Blended average estimate ($2.50 per 1M prompt, $10.00 per 1M completion)
        let est_cost = (p as f64 * 0.0000025) + (c as f64 * 0.000010);
        CostMetrics {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total,
            estimated_cost_usd: est_cost,
        }
    }

    pub fn reset(&self) {
        self.prompt_tokens.store(0, Ordering::Relaxed);
        self.completion_tokens.store(0, Ordering::Relaxed);
    }
}

pub fn init_telemetry(json_format: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("harness=info,harness_cli=info"));

    if json_format {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()
            .ok();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().compact())
            .try_init()
            .ok();
    }
}
