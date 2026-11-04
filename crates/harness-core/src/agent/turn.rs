//! The turn loop: replay, model calls with retry, tool dispatch through the gate, steering, and
//! the streaming-to-message assembly.

use futures_util::StreamExt;
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::dispatch::DispatchState;
use super::{retry_delay_with_jitter, Agent, AgentEvent, TurnOutcome, TurnToolCall};
use crate::error::HarnessError;
use crate::hooks::HookEvent;
use crate::session::{estimate_tokens, last_fold_seq, sendable_messages};
use crate::traits::*;
use crate::types::*;

/// What is written at the end of a reply the user's next message cut short.
///
/// It is stored, not just displayed: the next request carries the partial assistant message, and a
/// resumed transcript should say why the sentence stops where it does.
const INTERRUPTED_MARKER: &str = "\n\n[interrupted by your next message]";

/// One model call's output, and whether a soft interrupt cut it short.
struct Streamed {
    message: Message,
    interrupted: bool,
}

impl Streamed {
    fn done(message: Message) -> Self {
        Self {
            message,
            interrupted: false,
        }
    }

    /// A call stopped before it produced anything usable.
    fn cut() -> Self {
        Self {
            message: Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: None,
                usage: None,
            },
            interrupted: true,
        }
    }
}

/// Whether a finish reason means the model ran out of output budget rather than finished.
///
/// Gemini spells it `max_tokens` and the OpenAI-compatible routes spell it `length`. Either way
/// the answer was cut off, not withheld, and the same request with the same cap hits it again.
fn is_output_limit(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| {
        reason.eq_ignore_ascii_case("max_tokens") || reason.eq_ignore_ascii_case("length")
    })
}

/// The turn-loop error for a model that answered with nothing at all.
fn empty_answer_error(provider: &str) -> HarnessError {
    HarnessError::ProviderError {
        provider: provider.to_string(),
        message: "Model returned an empty response. The model may be overloaded or unavailable."
            .to_string(),
    }
}

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// The turn's reply only. Kept for callers with no console to report to (the voice pipeline);
    /// [`Agent::run_turn`] is the same run with the compaction and replay facts attached.
    pub async fn run_task(
        &self,
        user_input: &str,
        cancel_token: CancellationToken,
    ) -> Result<String, HarnessError> {
        self.run_turn(user_input, cancel_token)
            .await
            .map(|outcome| outcome.reply)
    }

    /// Run one turn: replay the session, bound the context, call the model until it stops asking
    /// for tools, and append everything to the session store as it happens.
    pub async fn run_turn(
        &self,
        user_input: &str,
        cancel_token: CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        self.completed_fold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.fold_disabled_this_turn.store(false, Ordering::SeqCst);
        let _reaper_guard = super::FoldReaperGuard(&self.completed_fold);
        // From here until the guard drops, a message can reach the turn without taking the agent
        // lock. Set before the first `await` so a message typed into a just-started turn is
        // queued for it rather than reported as idle.
        let _interrupt_guard = self.interrupt.begin_turn();
        let system_text = self.system_prompt(user_input).await?;

        // 1. Replay the session's recent transcript, bounded by `session.resume_max_messages`.
        //    Size reports a fold has retired are dropped here: they measure a prompt that is no
        //    longer sent, and every estimate taken from this vector — the gauge, the compaction
        //    threshold — would otherwise be the size of the folded transcript.
        let (history, replay_notice) = self.load_history().await?;
        let mut messages = Vec::with_capacity(history.len() + 3);
        messages.push(Message::system(system_text.clone()));
        messages.extend(sendable_messages(&history));
        messages.push(Message::user(user_input));

        // 1.5 Hooks see the prompt before the model does; a refusal ends the turn unspent.
        self.emit_hook_event(HookEvent::UserPromptSubmit, Some(user_input))
            .await?;

        // 2. Bound the context — without holding the turn. An automatic fold is started here and
        //    waited for by nobody: this turn runs on the transcript it has, and the folded
        //    transcript is what the next turn replays. The fold is a bound on the *next* request,
        //    so making the user wait out a summariser before their own prompt is even sent buys
        //    nothing.
        //
        // `fold_policy` is the only place the bar is decided (the auto-compact settings when they
        // are on, else `compact_at_tokens`), and `run_fold` is the only place a fold is written —
        // a fold that marked rows without writing a summary would replace the turns it removed
        // with nothing, which reads as the agent losing the conversation.
        let fold_policy = self.fold_policy();
        // The newest fold this turn starts from. A fold that lands while the turn runs rewrites the
        // stored transcript, and the checks below use this to tell "still over the bar" from "over
        // the bar a fold has already answered".
        let mut fold_watermark = last_fold_seq(&history);
        let fold_spawned_at_start = self.spawn_fold_if_needed(&messages, &history, fold_policy);

        // 2.5 Open this turn's file snapshot. Every return path closes it, including the error
        //    ones, so a later turn cannot silently capture into the wrong checkpoint.
        if let Some(controller) = self.checkpoints.as_ref() {
            controller.begin(
                self.session
                    .as_ref()
                    .map(|binding| binding.session_id.as_str())
                    .unwrap_or("unbound"),
                Some(user_input),
            );
        }
        let _checkpoint_guard = super::CheckpointGuard {
            controller: self.checkpoints.as_deref(),
        };

        let user_message = Message::user(user_input);
        let mut user_appended = false;

        let mut consecutive_failures = 0;
        let mut executed_tools = Vec::new();
        let mut last_reasoning = None;
        // A turn can be several model calls (tool loop, retries); the ledger wants the turn's
        // total, so every reported usage is folded in here rather than kept per call.
        let turn_started = std::time::Instant::now();
        let mut turn_usage: Option<TokenUsage> = None;
        // A model that cannot call tools is not sent their schemas: every provider that accepts a
        // `tools` array for a model without tool use charges for the tokens, and some reject the
        // request outright. The capability is resolved from the provider's own model, so this is
        // the model the loop is about to ask.
        let capabilities = self.provider.capabilities();
        let mut tools = self.registry.list_tools().await?;
        if !capabilities.tool_use {
            if !tools.is_empty() {
                warn!(
                    provider = %self.provider.provider_id(),
                    withheld_tools = tools.len(),
                    "model has no tool-use capability: sending the turn without tool definitions"
                );
            }
            tools.clear();
        }

        debug!("Starting agent task loop for query: '{}'", user_input);

        let mut loop_rounds: usize = 0;
        // Unbounded task loop: runs as long as productive progress is made
        loop {
            if cancel_token.is_cancelled() {
                warn!("Task loop cancelled cooperatively");
                return Err(HarnessError::Cancelled);
            }

            // A message that arrived while the previous batch of tools was running is taken here,
            // after the tools have finished and before the model is asked again. This is the one
            // place that deliberately waits: killing a tool the user did not ask to cancel would
            // lose work, and the next model call is the first moment the message can be used.
            let pending_interrupts = self.interrupt.drain();
            if !pending_interrupts.is_empty() {
                self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                    .await?;
                let joined = pending_interrupts.join("\n\n");
                let follow_up = Message::user(joined.clone());
                self.append(&follow_up, false).await?;
                messages.push(follow_up);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::Interrupted {
                        text: joined,
                        reasoning: String::new(),
                        partial: String::new(),
                    });
                }
            }

            if loop_rounds > 0 {
                self.try_rebind_completed_fold(&mut messages, &mut fold_watermark, &system_text)
                    .await;
            }

            let current_tokens = estimate_tokens(&messages).tokens;
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::ContextTokensUpdated {
                    tokens_used: current_tokens,
                });
            }

            if current_tokens as usize > fold_policy.threshold {
                self.spawn_fold_from_stored(fold_policy, &mut fold_watermark)
                    .await;
            }

            debug!(
                "Invoking model completion with {} messages and {} tools",
                messages.len(),
                tools.len()
            );
            let options = CompletionOptions {
                reasoning_effort: self.reasoning_effort,
                ..Default::default()
            };

            // Retrying on transient / overloaded errors with backoff
            let max_retries = 3;
            let mut response = None;
            let mut interrupted = false;
            for attempt in 0..=max_retries {
                match self
                    .stream_completion(
                        &messages,
                        &tools,
                        &options,
                        &mut executed_tools,
                        &cancel_token,
                    )
                    .await
                {
                    Ok(streamed) => {
                        interrupted = streamed.interrupted;
                        response = Some(streamed.message);
                        break;
                    }
                    Err(e)
                        if e.is_retryable()
                            && attempt < max_retries
                            && executed_tools.is_empty() =>
                    {
                        let delay = retry_delay_with_jitter(&e, attempt);
                        let delay_secs = (delay.as_secs_f64()).ceil() as u64;
                        warn!(
                            "Model request failed ({}), retrying attempt {}/{} in {:.2}s...",
                            e,
                            attempt + 1,
                            max_retries,
                            delay.as_secs_f64()
                        );
                        if let Some(ref observer) = self.observer {
                            observer(AgentEvent::ReasoningDelta(format!(
                                "\n[Model busy/overloaded: {}. Retrying in {}s (attempt {}/{})]…\n",
                                e,
                                delay_secs,
                                attempt + 1,
                                max_retries
                            )));
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {},
                            _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
                        }
                    }
                    Err(e) => return Err(e),
                }
            }

            let mut response = response.ok_or_else(|| HarnessError::ProviderError {
                provider: self.provider.provider_id().to_string(),
                message: "Model request failed after retries".to_string(),
            })?;

            let rebound = self
                .try_rebind_completed_fold(&mut messages, &mut fold_watermark, &system_text)
                .await;

            if let Some(ref usage) = response.usage {
                let accumulated = turn_usage.get_or_insert_with(TokenUsage::default);
                accumulated.prompt_tokens = accumulated
                    .prompt_tokens
                    .saturating_add(usage.prompt_tokens);
                accumulated.completion_tokens = accumulated
                    .completion_tokens
                    .saturating_add(usage.completion_tokens);
                accumulated.total_tokens = accumulated
                    .prompt_tokens
                    .saturating_add(accumulated.completion_tokens);
                // Cache reads are summed like the rest: the ledger prices a turn's cache hits, and
                // a turn is several model calls.
                accumulated.cached_tokens = accumulated
                    .cached_tokens
                    .saturating_add(usage.cached_tokens);
            }

            if rebound {
                response.usage = None;
            }

            let actual_tokens = match &response.usage {
                Some(usage) => usage.prompt_tokens.saturating_add(usage.completion_tokens),
                None => estimate_tokens(&messages).tokens,
            };
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::ContextTokensUpdated {
                    tokens_used: actual_tokens,
                });
            }

            if interrupted {
                // The prompt has to precede the answer it produced, even though the normal path
                // appends it only after the first response.
                let partial_content = response.content.clone().unwrap_or_default();
                let partial_reasoning = response.reasoning.clone().unwrap_or_default();
                self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                    .await?;
                // Incomplete tool-call deltas were never assembled into a `Message`, so nothing
                // invalid is stored. A call the provider had already executed is kept on
                // `executed_tools` and reported as usual.
                if response.content.is_some() || response.reasoning.is_some() {
                    if response.content.is_some() {
                        response.content = Some(format!("{partial_content}{INTERRUPTED_MARKER}"));
                    }
                    self.append(&response, false).await?;
                    messages.push(response.clone());
                }
                let pending = self.interrupt.drain();
                if pending.is_empty() {
                    // The token was cancelled just as the call ended and the message raced ahead
                    // to the post-call drain. Nothing to deliver here.
                    continue;
                }
                let joined = pending.join("\n\n");
                let follow_up = Message::user(joined.clone());
                self.append(&follow_up, false).await?;
                messages.push(follow_up);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::Interrupted {
                        text: joined,
                        reasoning: partial_reasoning,
                        partial: response.content.clone().unwrap_or_default(),
                    });
                }
                loop_rounds = loop_rounds.saturating_add(1);
                continue;
            }

            if let Some(ref r) = response.reasoning {
                last_reasoning = Some(r.clone());
            }

            // If no tools requested, assistant concluded task
            if response.tool_calls.is_empty() {
                // A message can land in the instant between the last chunk and this branch. The
                // answer is complete, but the user has moved on: treat the answer as an interim
                // one and ask again with their message, exactly as an interrupted call does.
                let pending_interrupts = self.interrupt.drain();
                if !pending_interrupts.is_empty() {
                    let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
                    if !reply_text.is_empty() {
                        let mut interim = response.clone();
                        interim.content = Some(reply_text.clone());
                        self.append(&interim, false).await?;
                        messages.push(interim);
                    }
                    self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                        .await?;
                    let joined = pending_interrupts.join("\n\n");
                    let follow_up = Message::user(joined.clone());
                    self.append(&follow_up, false).await?;
                    messages.push(follow_up);
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::Interrupted {
                            text: joined,
                            reasoning: response.reasoning.clone().unwrap_or_default(),
                            partial: reply_text,
                        });
                    }
                    loop_rounds = loop_rounds.saturating_add(1);
                    continue;
                }

                let pending_steers: Vec<String> = {
                    let mut sq = self.steer_queue.lock().unwrap_or_else(|e| e.into_inner());
                    sq.drain(..).collect()
                };
                if !pending_steers.is_empty() {
                    let joined_steer = pending_steers.join("\n\n");
                    let steer_prompt = format!(
                        "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\n{}\n[/OUT-OF-BAND USER MESSAGE]",
                        joined_steer
                    );
                    let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
                    if !reply_text.is_empty() {
                        let mut interim_resp = response.clone();
                        interim_resp.content = Some(reply_text);
                        self.append(&interim_resp, false).await?;
                        messages.push(interim_resp);
                    }
                    self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                        .await?;
                    let steer_msg = Message::user(steer_prompt);
                    self.append(&steer_msg, false).await?;
                    messages.push(steer_msg);
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::Steered { text: joined_steer });
                    }
                    continue;
                }

                debug!("Task successfully completed without further tool calls");
                let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
                if reply_text.is_empty() && last_reasoning.is_none() && executed_tools.is_empty() {
                    return Err(HarnessError::ProviderError {
                        provider: self.provider.provider_id().to_string(),
                        message: "Model returned an empty response. The model may be temporarily overloaded or unavailable.".to_string(),
                    });
                }
                let final_reply = if reply_text.is_empty() {
                    if !executed_tools.is_empty() {
                        String::new()
                    } else {
                        "(The model completed reasoning but produced no text output. Press Ctrl+O to inspect reasoning.)".to_string()
                    }
                } else {
                    reply_text
                };

                self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                    .await?;

                let mut final_response = response.clone();
                final_response.content = Some(final_reply.clone());
                self.append(&final_response, false).await?;
                self.emit_hook_event(HookEvent::Stop, Some(&final_reply))
                    .await?;
                // The saver reads the turn as the user saw it, reply included. The transcript is
                // dropped when this function returns, so appending the final message here changes
                // nothing for the next turn (which replays from the session store).
                messages.push(final_response);
                // The reply is final: anything the saver does now is off the critical path.
                self.spawn_background_saver(&messages);
                // The turn just grew the transcript, so look at the fold bar before returning: a
                // session that only folded on the *next* prompt reported compaction as something
                // the user had to come back for.
                if !fold_spawned_at_start
                    && estimate_tokens(&messages).tokens as usize > fold_policy.threshold
                {
                    self.spawn_fold_from_stored(fold_policy, &mut fold_watermark)
                        .await;
                }
                return Ok(TurnOutcome {
                    reply: final_reply,
                    reasoning: last_reasoning,
                    tool_calls: executed_tools,
                    replayed: replay_notice,
                    usage: turn_usage,
                    duration_ms: turn_started.elapsed().as_millis() as u64,
                });
            }

            self.ensure_user_prompt(&mut messages, &user_message, &mut user_appended)
                .await?;

            // Append assistant response with tool calls to conversation history
            self.append(&response, false).await?;
            messages.push(response.clone());

            // Execute each requested tool call, in the order the model asked for them: consecutive
            // read-only calls run together, and anything the human gate may touch stays serial
            // (see `dispatch.rs` for what that costs and why the boundary is where it is).
            let mut dispatch_state = DispatchState {
                messages: &mut messages,
                fold_watermark: &mut fold_watermark,
                system_text: &system_text,
                executed_tools: &mut executed_tools,
                consecutive_failures: &mut consecutive_failures,
                cancel_token: &cancel_token,
            };
            self.dispatch_tool_calls(response.tool_calls, &mut dispatch_state)
                .await?;

            // Check for mid-turn steering queued while tools were executing
            let pending_steers: Vec<String> = {
                let mut sq = self.steer_queue.lock().unwrap_or_else(|e| e.into_inner());
                sq.drain(..).collect()
            };
            if !pending_steers.is_empty() {
                let joined_steer = pending_steers.join("\n\n");
                let steer_prompt = format!(
                    "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\n{}\n[/OUT-OF-BAND USER MESSAGE]",
                    joined_steer
                );
                let steer_msg = Message::user(steer_prompt);
                self.append(&steer_msg, false).await?;
                messages.push(steer_msg);
                if let Some(ref observer) = self.observer {
                    observer(AgentEvent::Steered { text: joined_steer });
                }
            }
            loop_rounds = loop_rounds.saturating_add(1);
        }
    }

    /// Persist the original prompt, and make sure the in-memory request still carries it.
    ///
    /// It normally sits at the end of `messages` already; only the turn loop's own appending is
    /// deferred. But adopting a fold mid-turn rebuilds `messages` from the stored transcript, so a
    /// prompt that had not been written yet could disappear from the request even as it is appended
    /// here. Checking the tail covers both cases without adding a duplicate.
    async fn ensure_user_prompt(
        &self,
        messages: &mut Vec<Message>,
        user_message: &Message,
        user_appended: &mut bool,
    ) -> Result<(), HarnessError> {
        if *user_appended {
            return Ok(());
        }
        self.append(user_message, false).await?;
        *user_appended = true;
        let present = messages.last().is_some_and(|message| {
            message.role == Role::User && message.content == user_message.content
        });
        if !present {
            messages.push(user_message.clone());
        }
        Ok(())
    }

    /// One model call, taken through the provider's streaming API so reasoning reaches the
    /// observer while the model is still producing it. The stream is assembled into the same
    /// `Message` the buffered call returns, so tool execution and persistence are unchanged.
    ///
    /// A provider that cannot stream (or fails before producing anything) falls back to the
    /// buffered call — the turn is worth more than the live view of it.
    async fn stream_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        executed_tools: &mut Vec<TurnToolCall>,
        cancel_token: &CancellationToken,
    ) -> Result<Streamed, HarnessError> {
        // Count only the tools this call reports. `executed_tools` accumulates across the whole
        // turn, so testing it for emptiness would make every round after the first look as if it
        // had already produced output — and skip the buffered retry on an empty stream.
        let tools_before = executed_tools.len();
        // The soft-interrupt token belongs to this one call. While it is published, a message
        // typed at the composer stops the call but not the turn.
        let call = self.interrupt.begin_call();
        let call_token = call.token();
        let mut stream = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
            _ = call_token.cancelled() => return Ok(Streamed::cut()),
            res = self.provider.stream(messages, tools, options) => match res {
                Ok(stream) => stream,
                Err(error) => {
                    debug!("Provider stream unavailable ({}), falling back to a buffered call", error);
                    return self
                        .buffered_completion(messages, tools, options, cancel_token, &call_token)
                        .await;
                }
            }
        };

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut usage = None;
        let mut finish_reason: Option<String> = None;
        // Tool-call fragments arrive keyed by the index the provider assigned them.
        let mut calls: Vec<(usize, ToolCall)> = Vec::new();
        let mut interrupted = false;

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => return Err(HarnessError::Cancelled),
                _ = call_token.cancelled() => {
                    interrupted = true;
                    break;
                }
                ev = stream.next() => match ev {
                    Some(event) => event,
                    None => break,
                }
            };

            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    // Nothing arrived, or only reasoning arrived without content/tools:
                    // the buffered path or a clean retry can recover without failing the turn.
                    if content.is_empty()
                        && calls.is_empty()
                        && executed_tools.len() == tools_before
                    {
                        debug!(
                            "Stream failed before any content/tool output ({}), retrying buffered",
                            error
                        );
                        return self
                            .buffered_completion(
                                messages,
                                tools,
                                options,
                                cancel_token,
                                &call_token,
                            )
                            .await;
                    }
                    return Err(error);
                }
            };

            match event {
                StreamEvent::ReasoningDelta(text) => {
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::ReasoningDelta(text.clone()));
                    }
                    reasoning.push_str(&text);
                }
                StreamEvent::ContentDelta(text) => content.push_str(&text),
                StreamEvent::ToolExecuting { name, arguments } => {
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::ToolExecuting {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        });
                    }
                    executed_tools.push(TurnToolCall {
                        name,
                        arguments,
                        output: String::new(),
                        is_error: false,
                        denied: false,
                    });
                }
                StreamEvent::ToolExecuted {
                    name,
                    output,
                    is_error,
                } => {
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::ToolExecuted {
                            name: name.clone(),
                            output: output.clone(),
                            is_error,
                        });
                    }
                    if let Some(call) = executed_tools
                        .iter_mut()
                        .rev()
                        .find(|c| c.name == name && c.output.is_empty())
                    {
                        call.output = output;
                        call.is_error = is_error;
                    } else {
                        executed_tools.push(TurnToolCall {
                            name,
                            arguments: serde_json::Value::Null,
                            output,
                            is_error,
                            denied: false,
                        });
                    }
                }
                StreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                    signature,
                } => {
                    let slot = match calls.iter_mut().find(|(i, _)| *i == index) {
                        Some((_, call)) => call,
                        None => {
                            calls.push((
                                index,
                                ToolCall {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: serde_json::Value::Null,
                                    signature: None,
                                },
                            ));
                            let last_idx = calls.len() - 1;
                            &mut calls[last_idx].1
                        }
                    };
                    if let Some(id) = id {
                        slot.id = id;
                    }
                    if let Some(name) = name {
                        slot.name = name;
                    }
                    if let Some(signature) = signature {
                        slot.signature = Some(signature);
                    }
                    if let Some(fragment) = arguments_delta {
                        // Arguments stream as JSON text, so each fragment is stitched onto the raw
                        // string and parsed once at the end.
                        let mut raw = match slot.arguments.take() {
                            serde_json::Value::String(raw) => raw,
                            _ => String::new(),
                        };
                        raw.push_str(&fragment);
                        slot.arguments = serde_json::Value::String(raw);
                    }
                }
                StreamEvent::Usage(token_usage) => {
                    let actual_tokens = token_usage
                        .prompt_tokens
                        .saturating_add(token_usage.completion_tokens);
                    usage = Some(token_usage);
                    if let Some(ref observer) = self.observer {
                        observer(AgentEvent::ContextTokensUpdated {
                            tokens_used: actual_tokens,
                        });
                    }
                }
                StreamEvent::Completed {
                    usage: event_usage,
                    finish_reason: event_finish,
                } => {
                    if event_usage.is_some() {
                        usage = event_usage;
                    }
                    // Keep the last reason that names one: the usage frame that closes a Code
                    // Assist stream carries none and must not erase `MAX_TOKENS`.
                    if event_finish.is_some() {
                        finish_reason = event_finish;
                    }
                }
            }
        }

        if interrupted {
            // A tool call is only usable once its arguments are complete; fragments collected so
            // far describe a request the provider never finished. Drop them and keep the prose.
            let content = crate::text::strip_dsml_artifacts(&content);
            let message = Message {
                role: Role::Assistant,
                content: (!content.trim().is_empty()).then_some(content),
                reasoning: (!reasoning.trim().is_empty()).then_some(reasoning),
                tool_calls: Vec::new(),
                tool_result: None,
                usage,
            };
            return Ok(Streamed {
                message,
                interrupted: true,
            });
        }

        calls.sort_by_key(|(index, _)| *index);
        let tool_calls: Vec<ToolCall> = calls
            .into_iter()
            .enumerate()
            .map(|(position, (_, mut call))| {
                call.arguments = match call.arguments {
                    serde_json::Value::String(raw) => {
                        serde_json::from_str(&raw).unwrap_or(json!({}))
                    }
                    value => value,
                };
                if call.id.is_empty() {
                    call.id = format!("call_{}", position);
                }
                call
            })
            .collect();

        if content.trim().is_empty()
            && tool_calls.is_empty()
            && executed_tools.len() == tools_before
        {
            // A reply cut off by the output cap is not the transient empty shape the buffered
            // retry exists for: the identical request with the identical cap returns the identical
            // nothing, and re-sending it doubles the wait for a diagnosis. Reproduced against Code
            // Assist: `maxOutputTokens=4096` on a long tool-heavy turn spent 3,932 tokens thinking
            // and returned one `MAX_TOKENS` frame with no text.
            if is_output_limit(finish_reason.as_deref()) {
                return Err(HarnessError::output_limit_exhausted(
                    self.provider.provider_id(),
                    options.max_tokens,
                ));
            }
            // A usage or finish frame means the stream ran to the end and the model answered with
            // nothing. Posting the identical request again through the buffered path asks the same
            // question at the same moment; the turn loop's backed-off retry is the attempt that can
            // actually differ, and it no longer has to wait out a duplicate first.
            if usage.is_some() || finish_reason.is_some() {
                if reasoning.is_empty() {
                    return Err(empty_answer_error(self.provider.provider_id()));
                }
                return Ok(Streamed::done(Message {
                    role: Role::Assistant,
                    content: Some(crate::types::NO_TEXT_RESPONSE.to_string()),
                    reasoning: Some(reasoning),
                    tool_calls: Vec::new(),
                    tool_result: None,
                    usage,
                }));
            }
            debug!("Stream produced empty content and no tool calls, falling back to buffered completion");
            let buffered = self
                .buffered_completion(messages, tools, options, cancel_token, &call_token)
                .await?;
            let buffered = buffered.message;
            if buffered.content.as_deref().unwrap_or("").trim().is_empty()
                && buffered.tool_calls.is_empty()
            {
                if let Some(r) = buffered.reasoning.or(if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                }) {
                    return Ok(Streamed::done(Message {
                        role: Role::Assistant,
                        content: Some(crate::types::NO_TEXT_RESPONSE.to_string()),
                        reasoning: Some(r),
                        tool_calls: Vec::new(),
                        tool_result: None,
                        usage,
                    }));
                }
                return Err(empty_answer_error(self.provider.provider_id()));
            }
            return Ok(Streamed::done(buffered));
        }

        if reasoning.is_empty() {
            for (open_tag, close_tag) in &[("<think>", "</think>"), ("<thought>", "</thought>")] {
                if let Some(start) = content.find(open_tag) {
                    if let Some(end) = content.find(close_tag) {
                        let think_text = content[start + open_tag.len()..end].trim().to_string();
                        let rest =
                            format!("{}{}", &content[..start], &content[end + close_tag.len()..])
                                .trim()
                                .to_string();
                        if !think_text.is_empty() {
                            reasoning = think_text;
                        }
                        content = rest;
                        break;
                    }
                }
            }
        }

        // A gateway can echo DeepSeek's DSML scaffolding into the text channel *next to* a perfectly
        // good tool call. That is protocol noise rather than the model speaking, so it must not be
        // stored: otherwise a resumed session replays pages of `…DSML… calls>` that never showed
        // up during the live turn.
        content = crate::text::strip_dsml_artifacts(&content);

        let mut response = Message::assistant(content);
        if response.content.as_deref().unwrap_or("").trim().is_empty() {
            // Nothing but scaffolding (or nothing at all): the turn has no text, only calls.
            response.content = None;
        }
        response.reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
        response.tool_calls = tool_calls;
        response.usage = usage;
        Ok(Streamed::done(response))
    }

    /// The buffered call, under the same two stop signals as the streamed one.
    ///
    /// It is entered when a provider cannot stream (or fails before producing anything usable).
    /// A soft interrupt here returns the same empty cut result, so the turn loop treats it exactly
    /// like a stopped stream.
    async fn buffered_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        cancel_token: &CancellationToken,
        call_token: &CancellationToken,
    ) -> Result<Streamed, HarnessError> {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => Err(HarnessError::Cancelled),
            _ = call_token.cancelled() => Ok(Streamed::cut()),
            result = self.provider.complete(messages, tools, options) => {
                result.map(Streamed::done)
            }
        }
    }
}
