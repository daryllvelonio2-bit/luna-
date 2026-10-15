//! The offline backend: a provider that replays the case's canned turns instead of calling a model,
//! so the suite runs in CI with no credential and no quota.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde_json::json;

use harness_core::{
    CompletionOptions, HarnessError, Message, ModelProvider, StreamEvent, TokenUsage, ToolCall,
    ToolDefinition,
};

use super::scenario::ScriptTurn;

/// Provider/model names the offline backend reports. Both are constants, not literals, because the
/// report and the integration test assert on them.
pub const OFFLINE_PROVIDER_ID: &str = "offline";
pub const OFFLINE_MODEL_ID: &str = "scripted-v1";

/// Word counts of a string — the offline backend's deterministic, documented stand-in for tokens.
/// The harness drops the API's usage field on the non-streaming path, so no backend can report real
/// model tokens here; the difference is that this number is at least reproducible.
fn words(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

pub struct ScriptedProvider {
    script: Vec<ScriptTurn>,
    cursor: AtomicUsize,
    usage: Arc<Mutex<TokenUsage>>,
}

impl ScriptedProvider {
    pub fn new(script: &[ScriptTurn], usage: Arc<Mutex<TokenUsage>>) -> Self {
        Self {
            script: script.to_vec(),
            cursor: AtomicUsize::new(0),
            usage,
        }
    }

    /// Files this turn's word counts into the shared counter the report reads.
    fn account(&self, prompt: u32, completion: u32) {
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        usage.prompt_tokens += prompt;
        usage.completion_tokens += completion;
        usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn provider_id(&self) -> &str {
        OFFLINE_PROVIDER_ID
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec![OFFLINE_MODEL_ID.to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let prompt_words = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .map(words)
            .sum();

        // Past the end of the script the loop gets a plain answer with no tool calls, which is how a
        // turn ends. A script that runs out early therefore fails the case loudly on its answer
        // assertion instead of hanging.
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        let Some(turn) = self.script.get(index) else {
            self.account(prompt_words, 0);
            return Ok(Message::assistant(""));
        };

        if turn.tool_calls.is_empty() {
            let answer = turn.answer.clone().unwrap_or_default();
            self.account(prompt_words, words(&answer));
            return Ok(Message::assistant(answer));
        }

        let calls: Vec<ToolCall> = turn
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, call)| ToolCall {
                id: format!("call_{}_{}", index, i),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();

        let preamble = turn.answer.clone().unwrap_or_default();
        let completion_words: u32 = preamble_words(&preamble, &calls);
        self.account(prompt_words, completion_words);

        let mut message = Message::assistant_with_tools(calls);
        message.content = turn.answer.clone();
        Ok(message)
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        Err(HarnessError::ProviderError {
            provider: OFFLINE_PROVIDER_ID.to_string(),
            message: "the offline scripted provider only implements complete()".to_string(),
        })
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

fn preamble_words(preamble: &str, calls: &[ToolCall]) -> u32 {
    words(preamble)
        + calls
            .iter()
            .map(|c| words(&json!(c.arguments).to_string()))
            .sum::<u32>()
}
