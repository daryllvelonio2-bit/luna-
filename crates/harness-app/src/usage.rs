//! Turn usage accounting shared between CLI and GUI.

use std::sync::Arc;

use harness_core::usage::{PriceTable, UsageLedger, SOURCE_CHAT};

/// What a finished turn contributes, resolved once from the config.
pub struct UsageRecorder {
    ledger: Option<Arc<UsageLedger>>,
    prices: PriceTable,
}

impl UsageRecorder {
    pub fn from_config(config: &harness_core::Config) -> Self {
        let ledger = if config.usage.enabled {
            match UsageLedger::open(Some(config.paths.usage_db.clone())) {
                Ok(ledger) => Some(Arc::new(ledger)),
                Err(error) => {
                    tracing::warn!("usage recording is off ({error})");
                    None
                }
            }
        } else {
            None
        };
        Self {
            ledger,
            prices: config.usage.price_table(),
        }
    }

    pub async fn record(
        &self,
        provider: &str,
        model: &str,
        session_id: Option<&str>,
        outcome: &harness_core::TurnOutcome,
        source: &str,
    ) {
        let Some(ledger) = self.ledger.as_ref() else {
            return;
        };
        if let Err(error) = ledger
            .record_turn(provider, model, session_id, outcome, source, &self.prices)
            .await
        {
            tracing::warn!("could not record usage ({error})");
        }
    }

    pub async fn record_chat(
        &self,
        provider: &str,
        model: &str,
        session_id: Option<&str>,
        outcome: &harness_core::TurnOutcome,
    ) {
        self.record(provider, model, session_id, outcome, SOURCE_CHAT)
            .await;
    }

    pub fn ledger(&self) -> Option<&Arc<UsageLedger>> {
        self.ledger.as_ref()
    }
}
