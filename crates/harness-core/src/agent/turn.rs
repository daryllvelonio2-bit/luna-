//! The turn loop: replay, model calls with retry, tool dispatch through the gate, steering, and
//! the streaming-to-message assembly.

use futures_util::StreamExt;
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::dispatch::DispatchState;
use super::{
    retry_delay_with_jitter, Agent, AgentEvent, FoldPolicy, ReplayNotice, TurnOutcome, TurnToolCall,
};
use crate::error::HarnessError;
use crate::hooks::HookEvent;
use crate::session::{estimate_tokens, last_fold_seq, StoredMessage};
use crate::traits::*;
use crate::types::*;

/// What is written at the end of a reply the user's next message cut short.
///
/// It is stored, not just displayed: the next request carries the partial assistant message, and a
/// resumed transcript should say why the sentence stops where it does.
const INTERRUPTED_MARKER: &str = "\n\n[interrupted by your next message]";

/// How many times one model call is re-sent after a retryable failure.
///
/// Three retries covers a short 429/503 burst (the provider's own `Retry-After` still wins) while
/// bounding how long a turn can sit in backoff before the error reaches the user.
const MODEL_RETRY_LIMIT: u32 = 3;

/// One model call's output, and whether a soft interrupt cut it short.
struct Streamed {
    message: Message,
    interrupted: bool,
    /// The newest usage a provider reported per step. OpenCode runs an inner loop inside one
    /// model call: its per-step usage is the context size, while `message.usage` is the call's
    /// cumulative bill. The gauge and the stored row keep this, the ledger takes the total.
    /// `None` when the provider reported no per-step usage (the message's own usage is the
    /// context then).
    context_usage: Option<TokenUsage>,
}

impl Streamed {
    fn done(message: Message) -> Self {
        Self {
            message,
            interrupted: false,
            context_usage: None,
        }
    }

    fn done_with_context(message: Message, context_usage: Option<TokenUsage>) -> Self {
        Self {
            message,
            interrupted: false,
            context_usage,
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
            context_usage: None,
        }
    }
}

/// How a streamed model call stopped.
enum StreamEnd {
    /// The stream ended on its own.
    Ended,
    /// A soft interrupt stopped it; the accumulator holds whatever arrived.
    Interrupted,
    /// The stream errored before producing anything usable; the buffered call can retry.
    Fallback,
    /// The turn itself was cancelled.
    Cancelled,
}

/// One model call's stream, assembled as it arrives: prose, reasoning, usage, finish reason, and
/// the tool-call fragments keyed by the index the provider assigned them.
#[derive(Default)]
struct StreamAccumulator {
    content: String,
    reasoning: String,
    usage: Option<TokenUsage>,
    /// The newest per-step usage. OpenCode's cumulative `Completed` usage must not overwrite it,
    /// or the post-call gauge update jumps past the window.
    context_usage: Option<TokenUsage>,
    finish_reason: Option<String>,
    calls: Vec<(usize, ToolCall)>,
}

impl StreamAccumulator {
    /// Fold one stream event into the accumulator, forwarding it to the observer as it arrives.
    fn apply(
        &mut self,
        observer: Option<&super::AgentObserver>,
        event: StreamEvent,
        executed_tools: &mut Vec<TurnToolCall>,
    ) {
        match event {
            StreamEvent::ReasoningDelta(text) => {
                if let Some(observer) = observer {
                    observer(AgentEvent::ReasoningDelta(text.clone()));
                }
                self.reasoning.push_str(&text);
            }
            StreamEvent::ContentDelta(text) => {
                if let Some(observer) = observer {
                    observer(AgentEvent::ContentDelta(text.clone()));
                }
                self.content.push_str(&text);
            }
            StreamEvent::ToolExecuting { name, arguments } => {
                if let Some(observer) = observer {
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
                if let Some(observer) = observer {
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
            } => self.merge_call_delta(index, id, name, arguments_delta, signature),
            StreamEvent::Usage(token_usage) => {
                let actual_tokens = token_usage
                    .prompt_tokens
                    .saturating_add(token_usage.completion_tokens);
                self.context_usage = Some(token_usage.clone());
                self.usage = Some(token_usage);
                if let Some(observer) = observer {
                    observer(AgentEvent::ContextTokensUpdated {
                        tokens_used: actual_tokens,
                        reset: false,
                    });
                }
            }
            StreamEvent::Completed {
                usage: event_usage,
                finish_reason: event_finish,
            } => {
                if event_usage.is_some() {
                    self.usage = event_usage;
                }
                // Keep the last reason that names one: the usage frame that closes a Code Assist
                // stream carries none and must not erase `MAX_TOKENS`.
                if event_finish.is_some() {
                    self.finish_reason = event_finish;
                }
            }
        }
    }

    /// Stitch one `ToolCallDelta` onto the fragment slot its index names.
    ///
    /// If a provider omits per-call indices (reporting every call at `0`) and sends a second call
    /// whose `id` or `name` differs from the slot already at `index`, open a fresh slot rather than
    /// overwriting the name onto the previous call's arguments.
    fn merge_call_delta(
        &mut self,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
        signature: Option<String>,
    ) {
        let pos = id
            .as_deref()
            .filter(|incoming| !incoming.is_empty())
            .and_then(|incoming| self.calls.iter().position(|(_, call)| call.id == incoming))
            .or_else(|| {
                let pos = self.calls.iter().position(|(i, _)| *i == index)?;
                let existing = &self.calls[pos].1;
                let id_conflict = id
                    .as_deref()
                    .filter(|incoming| !incoming.is_empty())
                    .is_some_and(|incoming| !existing.id.is_empty() && existing.id != incoming);
                let name_conflict = name
                    .as_deref()
                    .filter(|incoming| !incoming.is_empty())
                    .is_some_and(|incoming| !existing.name.is_empty() && existing.name != incoming);
                (!id_conflict && !name_conflict).then_some(pos)
            });

        let slot = match pos {
            Some(pos) => &mut self.calls[pos].1,
            None => {
                let slot_index = if self.calls.iter().any(|(i, _)| *i == index) {
                    self.calls
                        .iter()
                        .map(|(i, _)| *i)
                        .max()
                        .map_or(0, |m| m + 1)
                        .max(index + 1)
                } else {
                    index
                };
                self.calls.push((
                    slot_index,
                    ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: serde_json::Value::Null,
                        signature: None,
                    },
                ));
                let last_idx = self.calls.len() - 1;
                &mut self.calls[last_idx].1
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
            // Arguments stream as JSON text, so each fragment is stitched onto the raw string and
            // parsed once at the end.
            let mut raw = match slot.arguments.take() {
                serde_json::Value::String(raw) => raw,
                _ => String::new(),
            };
            raw.push_str(&fragment);
            slot.arguments = serde_json::Value::String(raw);
        }
    }

    /// Finish the collected call fragments into usable calls, in the order the model issued them.
    ///
    /// A tool call whose arguments are not valid JSON was never finished by the provider — the
    /// stream was cut, or the output cap landed mid-object. It must not be run with `{}`: a
    /// truncated `read_file` then reports a misleading missing argument, and a truncated
    /// `write_file`/`patch` would apply the wrong edit. The call is failed so the turn loop's
    /// retry asks again.
    fn assemble_tool_calls(&mut self, provider: &str) -> Result<Vec<ToolCall>, HarnessError> {
        self.calls.sort_by_key(|(index, _)| *index);
        let mut incomplete: Option<(String, String)> = None;
        let tool_calls: Vec<ToolCall> = self
            .calls
            .drain(..)
            .enumerate()
            .map(|(position, (_, mut call))| {
                call.arguments = match call.arguments {
                    // An empty fragment means "no arguments". Trailing junk after a whole value is
                    // ignored rather than treated as truncation; only a value that ends early is
                    // an incomplete call. See `crate::json::parse_tool_arguments`.
                    serde_json::Value::String(raw) => {
                        match crate::json::parse_tool_arguments(&raw) {
                            Ok(value) => value,
                            Err(error) => {
                                incomplete = Some((call.name.clone(), error.to_string()));
                                json!({})
                            }
                        }
                    }
                    value => value,
                };
                if call.id.is_empty() {
                    call.id = format!("call_{}", position);
                }
                call
            })
            .collect();

        if let Some((name, error)) = incomplete {
            warn!(
                tool = %name,
                "dropping incomplete tool call: arguments were not valid JSON ({error})"
            );
            return Err(HarnessError::ProviderError {
                provider: provider.to_string(),
                message: format!(
                    "Model returned an incomplete tool call for '{name}': its arguments were not \
                     valid JSON ({error}). The call was not run."
                ),
            });
        }
        Ok(tool_calls)
    }

    /// The reply for a call a soft interrupt stopped: prose only, never a half-received call.
    fn into_interrupted(self) -> Streamed {
        let content = crate::text::strip_dsml_artifacts(&self.content);
        let message = Message {
            role: Role::Assistant,
            content: (!content.trim().is_empty()).then_some(content),
            reasoning: (!self.reasoning.trim().is_empty()).then_some(self.reasoning),
            tool_calls: Vec::new(),
            tool_result: None,
            usage: self.usage,
        };
        Streamed {
            message,
            interrupted: true,
            context_usage: self.context_usage,
        }
    }

    /// The reply for a completed stream with at least some text or at least one tool call.
    fn into_streamed(mut self, tool_calls: Vec<ToolCall>) -> Streamed {
        let mut reasoning = std::mem::take(&mut self.reasoning);
        if reasoning.is_empty() {
            for (open_tag, close_tag) in &[("<think>", "</think>"), ("<thought>", "</thought>")] {
                if let Some(start) = self.content.find(open_tag) {
                    if let Some(end) = self.content.find(close_tag) {
                        let think_text =
                            self.content[start + open_tag.len()..end].trim().to_string();
                        let rest = format!(
                            "{}{}",
                            &self.content[..start],
                            &self.content[end + close_tag.len()..]
                        )
                        .trim()
                        .to_string();
                        if !think_text.is_empty() {
                            reasoning = think_text;
                        }
                        self.content = rest;
                        break;
                    }
                }
            }
        }

        // A gateway can echo DeepSeek's DSML scaffolding into the text channel *next to* a perfectly
        // good tool call. That is protocol noise rather than the model speaking, so it must not be
        // stored: otherwise a resumed session replays pages of `…DSML… calls>` that never showed
        // up during the live turn.
        let content = crate::text::strip_dsml_artifacts(&self.content);

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
        response.usage = self.usage;
        Streamed::done_with_context(response, self.context_usage)
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

/// What replaying the session produced for the first request of a turn.
struct OpenedTurn {
    system_text: String,
    history: Vec<StoredMessage>,
    messages: Vec<Message>,
    replay_notice: Option<ReplayNotice>,
}

/// The transcript and tallies a turn's loop carries from round to round.
struct TurnState {
    messages: Vec<Message>,
    user_message: Message,
    user_appended: bool,
    executed_tools: Vec<TurnToolCall>,
    consecutive_failures: usize,
    last_reasoning: Option<String>,
    /// A turn can be several model calls (tool loop, retries); the ledger wants the turn's total,
    /// so every reported usage is folded in here rather than kept per call.
    turn_usage: Option<TokenUsage>,
    loop_rounds: usize,
    /// The newest fold this turn starts from. A fold that lands while the turn runs rewrites the
    /// stored transcript, and the checks use this to tell "still over the bar" from "over the bar
    /// a fold has already answered".
    fold_watermark: Option<i64>,
    fold_policy: FoldPolicy,
}

/// One model call's reply and how much of `executed_tools` that call is responsible for.
struct ModelCall {
    response: Message,
    interrupted: bool,
    context_usage: Option<TokenUsage>,
    tools_before_call: usize,
}

/// Everything a turn needs once its opening work is done.
struct TurnSetup<'a> {
    system_text: String,
    state: TurnState,
    fold_spawned_at_start: bool,
    turn_started: std::time::Instant,
    replay_notice: Option<ReplayNotice>,
    tools: Vec<ToolDefinition>,
    /// Closes the turn's checkpoint however the turn ends.
    checkpoint: super::CheckpointGuard<'a>,
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
        // 1. Replay, bound the context, open the checkpoint and collect the turn's tools.
        let setup = self.begin_turn(user_input).await?;
        let system_text = setup.system_text;
        let mut state = setup.state;
        let fold_spawned_at_start = setup.fold_spawned_at_start;
        let turn_started = setup.turn_started;
        let replay_notice = setup.replay_notice;
        let tools = setup.tools;
        let _checkpoint_guard = setup.checkpoint;

        // Unbounded task loop: runs as long as productive progress is made
        loop {
            if let Some(outcome) = self
                .run_round(
                    &mut state,
                    &tools,
                    &system_text,
                    fold_spawned_at_start,
                    turn_started,
                    &replay_notice,
                    &cancel_token,
                )
                .await?
            {
                return Ok(outcome);
            }
        }
    }

    /// The turn's opening: replay the session, bound the context, open the checkpoint, and collect
    /// the tools the first request may send.
    ///
    /// The fold is started here and waited for by nobody: this turn runs on the transcript it has,
    /// and the folded transcript is what the next turn replays. The fold is a bound on the *next*
    /// request, so making the user wait out a summariser before their own prompt is even sent buys
    /// nothing. `fold_policy` is the only place the bar is decided (the auto-compact settings when
    /// they are on, else `compact_at_tokens`), and `run_fold` is the only place a fold is written —
    /// a fold that marked rows without writing a summary would replace the turns it removed with
    /// nothing, which reads as the agent losing the conversation.
    async fn begin_turn(&self, user_input: &str) -> Result<TurnSetup<'_>, HarnessError> {
        let opened = self.open_turn(user_input).await?;
        let system_text = opened.system_text;
        let history = opened.history;
        let replay_notice = opened.replay_notice;
        let mut messages = opened.messages;

        let fold_policy = self.fold_policy();
        // The newest fold this turn starts from. A fold that lands while the turn runs rewrites the
        // stored transcript, and the checks use this to tell "still over the bar" from "over the
        // bar a fold has already answered".
        let mut fold_watermark = last_fold_seq(&history);
        let fold_spawned_at_start = self
            .begin_fold(
                &mut messages,
                &history,
                &system_text,
                fold_policy,
                &mut fold_watermark,
            )
            .await;

        // Open this turn's file snapshot after the fold: every return path closes it, including
        // the error ones, so a later turn cannot silently capture into the wrong checkpoint.
        let checkpoint = self.begin_checkpoint_guard(user_input);

        let user_message = Message::user(user_input);
        // A turn can be several model calls (tool loop, retries); the ledger wants the turn's
        // total, so every reported usage is folded in later rather than kept per call.
        let turn_started = std::time::Instant::now();
        // A model that cannot call tools is not sent their schemas: every provider that accepts a
        // `tools` array for a model without tool use charges for the tokens, and some reject the
        // request outright.
        let tools = self.turn_tools().await?;

        debug!("Starting agent task loop for query: '{}'", user_input);

        Ok(TurnSetup {
            system_text,
            state: TurnState {
                messages,
                user_message,
                user_appended: false,
                executed_tools: Vec::new(),
                consecutive_failures: 0,
                last_reasoning: None,
                turn_usage: None,
                loop_rounds: 0,
                fold_watermark,
                fold_policy,
            },
            fold_spawned_at_start,
            turn_started,
            replay_notice,
            tools,
            checkpoint,
        })
    }

    /// One pass of the task loop: adopt a landed fold, re-ask the model, and either act on its tool
    /// calls or conclude the turn. `Ok(None)` means the loop goes round again.
    #[allow(clippy::too_many_arguments)]
    async fn run_round(
        &self,
        state: &mut TurnState,
        tools: &[ToolDefinition],
        system_text: &str,
        fold_spawned_at_start: bool,
        turn_started: std::time::Instant,
        replay_notice: &Option<ReplayNotice>,
        cancel_token: &CancellationToken,
    ) -> Result<Option<TurnOutcome>, HarnessError> {
        if cancel_token.is_cancelled() {
            warn!("Task loop cancelled cooperatively");
            return Err(HarnessError::Cancelled);
        }

        // A message that arrived while the previous batch of tools was running is taken here,
        // after the tools have finished and before the model is asked again. This is the one place
        // that deliberately waits: killing a tool the user did not ask to cancel would lose work,
        // and the next model call is the first moment the message can be used.
        self.deliver_queued_interrupts(state).await?;

        let rebound = if state.loop_rounds > 0 {
            self.try_rebind_completed_fold(
                &mut state.messages,
                &mut state.fold_watermark,
                system_text,
            )
            .await
        } else {
            false
        };

        let current_tokens = estimate_tokens(&state.messages).tokens;
        // A fold that landed while this turn ran has just been adopted: the messages below are the
        // folded transcript, so the smaller figure is a measurement of what will be sent, not a
        // stale report the turn will contradict. Without the reset the gauge held its pre-fold
        // high-water mark until the turn boundary, which read as "compaction did nothing".
        self.report_context_tokens(current_tokens, rebound);

        self.fold_if_over_threshold(state, system_text, current_tokens)
            .await;

        debug!(
            "Invoking model completion with {} messages and {} tools",
            state.messages.len(),
            tools.len()
        );
        let options = CompletionOptions {
            reasoning_effort: self.reasoning_effort,
            ..Default::default()
        };

        // Retrying on transient / overloaded errors with backoff
        let call = self
            .complete_with_retry(state, tools, &options, cancel_token)
            .await?;
        let ModelCall {
            mut response,
            interrupted,
            context_usage,
            tools_before_call,
        } = call;

        // A rebound replaced `messages` with the folded transcript and dropped the usage of the
        // prompt that was sent before the fold, so the gauge takes the honest size of what the turn
        // will send next instead of holding the pre-fold high-water mark.
        self.settle_model_call(
            state,
            &mut response,
            context_usage,
            tools_before_call,
            system_text,
        )
        .await?;

        if interrupted {
            self.deliver_interrupted_reply(state, response).await?;
            return Ok(None);
        }

        if let Some(ref r) = response.reasoning {
            state.last_reasoning = Some(r.clone());
        }

        // If no tools requested, assistant concluded task
        if response.tool_calls.is_empty() {
            return self
                .conclude_turn(
                    state,
                    &response,
                    fold_spawned_at_start,
                    turn_started,
                    replay_notice.clone(),
                )
                .await;
        }

        self.dispatch_round_tools(state, response, system_text, cancel_token)
            .await?;
        Ok(None)
    }

    /// Append an assistant row that asked for tools, run them through the gate, then take any
    /// steering queued while they ran.
    async fn dispatch_round_tools(
        &self,
        state: &mut TurnState,
        response: Message,
        system_text: &str,
        cancel_token: &CancellationToken,
    ) -> Result<(), HarnessError> {
        self.ensure_user_prompt(
            &mut state.messages,
            &state.user_message,
            &mut state.user_appended,
        )
        .await?;

        // Append assistant response with tool calls to conversation history
        self.append(&response, false).await?;
        state.messages.push(response.clone());

        // Execute each requested tool call, in the order the model asked for them: consecutive
        // read-only calls run together, and anything the human gate may touch stays serial (see
        // `dispatch.rs` for what that costs and why the boundary is where it is).
        {
            let mut dispatch_state = DispatchState {
                messages: &mut state.messages,
                fold_watermark: &mut state.fold_watermark,
                system_text,
                executed_tools: &mut state.executed_tools,
                consecutive_failures: &mut state.consecutive_failures,
                cancel_token,
            };
            self.dispatch_tool_calls(response.tool_calls, &mut dispatch_state)
                .await?;
        }

        // Check for mid-turn steering queued while tools were executing
        self.deliver_pending_steers(state).await?;
        state.loop_rounds = state.loop_rounds.saturating_add(1);
        Ok(())
    }

    /// Replay the session's recent transcript and assemble the first request's vector.
    ///
    /// 1. Replay the session's recent transcript, bounded by `session.resume_max_messages`. Size
    ///    reports a fold has retired are dropped here: they measure a prompt that is no longer
    ///    sent, and every estimate taken from this vector — the gauge, the compaction threshold —
    ///    would otherwise be the size of the folded transcript.
    ///
    /// 1.5 Hooks see the prompt before the model does; a refusal ends the turn unspent.
    async fn open_turn(&self, user_input: &str) -> Result<OpenedTurn, HarnessError> {
        let system_text = self.system_prompt(user_input).await?;
        let (history, replay_notice) = self.load_history().await?;
        let mut messages = Vec::with_capacity(history.len() + 3);
        messages.push(Message::system(system_text.clone()));
        messages.extend(self.replay_messages(&history));
        messages.push(Message::user(user_input));
        self.emit_hook_event(HookEvent::UserPromptSubmit, Some(user_input))
            .await?;
        Ok(OpenedTurn {
            system_text,
            history,
            messages,
            replay_notice,
        })
    }

    /// Bound the context before the first request: preflight fold at the hard ceiling, otherwise
    /// start the ordinary background fold. Returns whether a fold was spawned at turn start.
    ///
    /// Past the hard ceiling the request may be refused, so the fold is not left behind: wait for
    /// it and continue on the bounded vector. Below it the ordinary background path runs.
    async fn begin_fold(
        &self,
        messages: &mut Vec<Message>,
        history: &[StoredMessage],
        system_text: &str,
        fold_policy: FoldPolicy,
        fold_watermark: &mut Option<i64>,
    ) -> bool {
        let preflight_folded = self
            .preflight_fold_if_needed(messages, history, system_text, fold_policy, fold_watermark)
            .await;
        if preflight_folded {
            false
        } else {
            self.spawn_fold_if_needed(messages, history, fold_policy)
        }
    }

    /// Open this turn's file snapshot. Every return path closes it, including the error ones, so a
    /// later turn cannot silently capture into the wrong checkpoint.
    fn begin_checkpoint_guard(&self, user_input: &str) -> super::CheckpointGuard<'_> {
        if let Some(controller) = self.checkpoints.as_ref() {
            controller.begin(
                self.session
                    .as_ref()
                    .map(|binding| binding.session_id.as_str())
                    .unwrap_or("unbound"),
                Some(user_input),
            );
        }
        super::CheckpointGuard {
            controller: self.checkpoints.as_deref(),
        }
    }

    /// The tools this turn may send: the registry surface minus the runtime toggles, and none at
    /// all for a model that cannot call tools.
    ///
    /// Every provider that accepts a `tools` array for a model without tool use charges for the
    /// tokens, and some reject the request outright. The capability is resolved from the
    /// provider's own model, so this is the model the loop is about to ask.
    async fn turn_tools(&self) -> Result<Vec<ToolDefinition>, HarnessError> {
        let capabilities = self.provider.capabilities();
        let mut tools = self.registry.list_tools().await?;
        // Runtime feature toggles (`/memory off`, `/notes off`, `/skills off`, `/mcp off`) remove
        // the owned tools from the request entirely: the model cannot ask for what it cannot see.
        tools.retain(|tool| self.config.tool_enabled(&tool.name));
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
        Ok(tools)
    }

    /// Publish the context gauge; `reset` says the transcript it measures was rebuilt mid-turn.
    pub(super) fn report_context_tokens(&self, tokens_used: u32, reset: bool) {
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::ContextTokensUpdated { tokens_used, reset });
        }
    }

    /// Fold the transcript when it is over the policy's bar, waiting for the fold at the ceiling.
    async fn fold_if_over_threshold(
        &self,
        state: &mut TurnState,
        system_text: &str,
        current_tokens: u32,
    ) {
        if current_tokens as usize > state.fold_policy.threshold {
            let urgent = state
                .fold_policy
                .ceiling
                .is_some_and(|ceiling| current_tokens as usize > ceiling);
            if urgent {
                self.fold_now_from_stored(
                    &mut state.messages,
                    system_text,
                    state.fold_policy,
                    &mut state.fold_watermark,
                )
                .await;
            } else {
                self.spawn_fold_from_stored(state.fold_policy, &mut state.fold_watermark)
                    .await;
            }
        }
    }

    /// Take a message that arrived while tools were running, at the one point the loop
    /// deliberately waits for it: after the tools finished, before the model is asked again.
    async fn deliver_queued_interrupts(&self, state: &mut TurnState) -> Result<(), HarnessError> {
        let pending_interrupts = self.interrupt.drain();
        if pending_interrupts.is_empty() {
            return Ok(());
        }
        self.ensure_user_prompt(
            &mut state.messages,
            &state.user_message,
            &mut state.user_appended,
        )
        .await?;
        let joined = pending_interrupts.join("\n\n");
        let follow_up = Message::user(joined.clone());
        self.append(&follow_up, false).await?;
        state.messages.push(follow_up);
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::Interrupted {
                text: joined,
                reasoning: String::new(),
                partial: String::new(),
            });
        }
        Ok(())
    }

    /// Take the steering queue, if anything is waiting, and render it as the out-of-band message
    /// that is appended at this position.
    fn drain_steers(&self) -> Option<(String, Message)> {
        let pending_steers: Vec<String> = {
            let mut sq = self.steer_queue.lock().unwrap_or_else(|e| e.into_inner());
            sq.drain(..).collect()
        };
        if pending_steers.is_empty() {
            return None;
        }
        let joined_steer = pending_steers.join("\n\n");
        let steer_prompt = format!(
            "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\n{}\n[/OUT-OF-BAND USER MESSAGE]",
            joined_steer
        );
        Some((joined_steer, Message::user(steer_prompt)))
    }

    /// Deliver steering that was queued while the batch of tools was executing.
    async fn deliver_pending_steers(&self, state: &mut TurnState) -> Result<(), HarnessError> {
        let Some((joined_steer, steer_msg)) = self.drain_steers() else {
            return Ok(());
        };
        self.append(&steer_msg, false).await?;
        state.messages.push(steer_msg);
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::Steered { text: joined_steer });
        }
        Ok(())
    }

    /// Keep the prose a soft interrupt cut short, then deliver the message that stopped the call.
    async fn deliver_interrupted_reply(
        &self,
        state: &mut TurnState,
        mut response: Message,
    ) -> Result<(), HarnessError> {
        // The prompt has to precede the answer it produced, even though the normal path appends it
        // only after the first response.
        let partial_content = response.content.clone().unwrap_or_default();
        let partial_reasoning = response.reasoning.clone().unwrap_or_default();
        self.ensure_user_prompt(
            &mut state.messages,
            &state.user_message,
            &mut state.user_appended,
        )
        .await?;
        // Incomplete tool-call deltas were never assembled into a `Message`, so nothing invalid is
        // stored. A call the provider had already executed is kept on `executed_tools` and
        // reported as usual.
        if response.content.is_some() || response.reasoning.is_some() {
            if response.content.is_some() {
                response.content = Some(format!("{partial_content}{INTERRUPTED_MARKER}"));
            }
            self.append(&response, false).await?;
            state.messages.push(response.clone());
        }
        let pending = self.interrupt.drain();
        if pending.is_empty() {
            // The token was cancelled just as the call ended and the message raced ahead to the
            // post-call drain. Nothing to deliver here.
            return Ok(());
        }
        let joined = pending.join("\n\n");
        let follow_up = Message::user(joined.clone());
        self.append(&follow_up, false).await?;
        state.messages.push(follow_up);
        if let Some(ref observer) = self.observer {
            observer(AgentEvent::Interrupted {
                text: joined,
                reasoning: partial_reasoning,
                partial: response.content.clone().unwrap_or_default(),
            });
        }
        state.loop_rounds = state.loop_rounds.saturating_add(1);
        Ok(())
    }

    /// One model call with retries on transient / overloaded errors, backing off between attempts.
    ///
    /// The retry conditions are deliberately narrow: a call is only re-sent when nothing in the
    /// turn's tool history changed, so a provider that had already executed a call is never asked
    /// to repeat work this loop cannot account for.
    async fn complete_with_retry(
        &self,
        state: &mut TurnState,
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        cancel_token: &CancellationToken,
    ) -> Result<ModelCall, HarnessError> {
        let max_retries = MODEL_RETRY_LIMIT;
        let tools_before_call = state.executed_tools.len();
        let mut response = None;
        let mut interrupted = false;
        let mut context_usage = None;
        for attempt in 0..=max_retries {
            match self
                .stream_completion(
                    &state.messages,
                    tools,
                    options,
                    &mut state.executed_tools,
                    cancel_token,
                )
                .await
            {
                Ok(streamed) => {
                    interrupted = streamed.interrupted;
                    context_usage = streamed.context_usage;
                    response = Some(streamed.message);
                    break;
                }
                Err(e)
                    if e.is_retryable()
                        && attempt < max_retries
                        && state.executed_tools.len() == tools_before_call =>
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
                Err(e) => {
                    // A call the provider had already executed is still real work: write its
                    // calls and results before the error unwinds the turn, so a stop/continue
                    // does not lose the steps that did run.
                    if !state.executed_tools[tools_before_call..].is_empty() {
                        self.ensure_user_prompt(
                            &mut state.messages,
                            &state.user_message,
                            &mut state.user_appended,
                        )
                        .await
                        .ok();
                        if let Err(persist_error) = self
                            .persist_provider_tools(
                                &mut state.messages,
                                &state.executed_tools[tools_before_call..],
                            )
                            .await
                        {
                            warn!("Failed to persist provider-executed tools: {persist_error}");
                        }
                    }
                    return Err(e);
                }
            }
        }

        let response = response.ok_or_else(|| HarnessError::ProviderError {
            provider: self.provider.provider_id().to_string(),
            message: "Model request failed after retries".to_string(),
        })?;
        Ok(ModelCall {
            response,
            interrupted,
            context_usage,
            tools_before_call,
        })
    }

    /// Book-keep a model call's result before the turn acts on it: adopt a fold that landed
    /// mid-call, fold the call's usage into the turn's ledger, keep the newest per-step usage on
    /// the row, persist provider-executed tools, and update the gauge.
    async fn settle_model_call(
        &self,
        state: &mut TurnState,
        response: &mut Message,
        mut context_usage: Option<TokenUsage>,
        tools_before_call: usize,
        system_text: &str,
    ) -> Result<(), HarnessError> {
        let rebound = self
            .try_rebind_completed_fold(&mut state.messages, &mut state.fold_watermark, system_text)
            .await;

        if let Some(ref usage) = response.usage {
            let accumulated = state.turn_usage.get_or_insert_with(TokenUsage::default);
            accumulated.prompt_tokens = accumulated
                .prompt_tokens
                .saturating_add(usage.prompt_tokens);
            accumulated.completion_tokens = accumulated
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            accumulated.total_tokens = accumulated
                .prompt_tokens
                .saturating_add(accumulated.completion_tokens);
            // Cache reads are summed like the rest: the ledger prices a turn's cache hits, and a
            // turn is several model calls.
            accumulated.cached_tokens = accumulated
                .cached_tokens
                .saturating_add(usage.cached_tokens);
        }

        if rebound {
            response.usage = None;
            context_usage = None;
        } else if let Some(context) = context_usage.as_ref() {
            // The ledger already took the call's cumulative total above. The row keeps the newest
            // per-step usage: replay and compaction measure a prompt with it, and a
            // session-cumulative figure there would inflate every later estimate.
            response.usage = Some(context.clone());
        }

        // The provider ran these tools inside its own loop (OpenCode). They are not on
        // `response.tool_calls`, so the dispatch will never see them; persist the calls and
        // results now. Otherwise the next request, and a resumed session, replay only the final
        // prose and the model cannot tell what it already ran.
        if response.tool_calls.is_empty() && !state.executed_tools[tools_before_call..].is_empty() {
            self.ensure_user_prompt(
                &mut state.messages,
                &state.user_message,
                &mut state.user_appended,
            )
            .await?;
            self.persist_provider_tools(
                &mut state.messages,
                &state.executed_tools[tools_before_call..],
            )
            .await?;
        }

        let actual_tokens = match context_usage {
            Some(usage) => usage.prompt_tokens.saturating_add(usage.completion_tokens),
            None => match &response.usage {
                Some(usage) => usage.prompt_tokens.saturating_add(usage.completion_tokens),
                None => estimate_tokens(&state.messages).tokens,
            },
        };
        // A rebound replaced `messages` with the folded transcript and dropped the usage of the
        // prompt that was sent before the fold, so the figure above is the honest size of what the
        // turn will send next. Let the gauge take it now instead of holding the pre-fold
        // high-water mark to the end of the turn.
        self.report_context_tokens(actual_tokens, rebound);
        Ok(())
    }

    /// The reply-side of the loop: a tool-free answer concludes the turn unless a message or a
    /// steer arrived first. `Ok(None)` asks the loop to go round again.
    async fn conclude_turn(
        &self,
        state: &mut TurnState,
        response: &Message,
        fold_spawned_at_start: bool,
        turn_started: std::time::Instant,
        replay_notice: Option<ReplayNotice>,
    ) -> Result<Option<TurnOutcome>, HarnessError> {
        // A message can land in the instant between the last chunk and this branch. The answer is
        // complete, but the user has moved on: treat the answer as an interim one and ask again
        // with their message, exactly as an interrupted call does.
        let pending_interrupts = self.interrupt.drain();
        if !pending_interrupts.is_empty() {
            let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
            if !reply_text.is_empty() {
                let mut interim = response.clone();
                interim.content = Some(reply_text.clone());
                self.append(&interim, false).await?;
                state.messages.push(interim);
            }
            self.ensure_user_prompt(
                &mut state.messages,
                &state.user_message,
                &mut state.user_appended,
            )
            .await?;
            let joined = pending_interrupts.join("\n\n");
            let follow_up = Message::user(joined.clone());
            self.append(&follow_up, false).await?;
            state.messages.push(follow_up);
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::Interrupted {
                    text: joined,
                    reasoning: response.reasoning.clone().unwrap_or_default(),
                    partial: reply_text,
                });
            }
            state.loop_rounds = state.loop_rounds.saturating_add(1);
            return Ok(None);
        }

        if let Some((joined_steer, steer_msg)) = self.drain_steers() {
            let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
            if !reply_text.is_empty() {
                let mut interim_resp = response.clone();
                interim_resp.content = Some(reply_text);
                self.append(&interim_resp, false).await?;
                state.messages.push(interim_resp);
            }
            self.ensure_user_prompt(
                &mut state.messages,
                &state.user_message,
                &mut state.user_appended,
            )
            .await?;
            self.append(&steer_msg, false).await?;
            state.messages.push(steer_msg);
            if let Some(ref observer) = self.observer {
                observer(AgentEvent::Steered { text: joined_steer });
            }
            return Ok(None);
        }

        debug!("Task successfully completed without further tool calls");
        let reply_text = response.content.as_deref().unwrap_or("").trim().to_string();
        if reply_text.is_empty()
            && state.last_reasoning.is_none()
            && state.executed_tools.is_empty()
        {
            return Err(HarnessError::ProviderError {
                provider: self.provider.provider_id().to_string(),
                message: "Model returned an empty response. The model may be temporarily overloaded or unavailable.".to_string(),
            });
        }
        let final_reply = if reply_text.is_empty() {
            if !state.executed_tools.is_empty() {
                String::new()
            } else {
                // A neutral placeholder: `Ctrl+O` is a CLI binding the core cannot know about, and
                // this string is appended to the transcript. The CLI adds its own hint when it
                // renders the reply.
                crate::types::NO_TEXT_RESPONSE.to_string()
            }
        } else {
            reply_text
        };

        self.ensure_user_prompt(
            &mut state.messages,
            &state.user_message,
            &mut state.user_appended,
        )
        .await?;

        let mut final_response = response.clone();
        final_response.content = Some(final_reply.clone());
        self.append(&final_response, false).await?;
        self.emit_hook_event(HookEvent::Stop, Some(&final_reply))
            .await?;
        // The saver reads the turn as the user saw it, reply included. The transcript is dropped
        // when this function returns, so appending the final message here changes nothing for the
        // next turn (which replays from the session store).
        state.messages.push(final_response);
        // The reply is final: anything the saver does now is off the critical path.
        self.spawn_background_saver(&state.messages);
        // The turn just grew the transcript, so look at the fold bar before returning: a session
        // that only folded on the *next* prompt reported compaction as something the user had to
        // come back for.
        if !fold_spawned_at_start
            && estimate_tokens(&state.messages).tokens as usize > state.fold_policy.threshold
        {
            self.spawn_fold_from_stored(state.fold_policy, &mut state.fold_watermark)
                .await;
        }
        Ok(Some(TurnOutcome {
            reply: final_reply,
            reasoning: state.last_reasoning.clone(),
            tool_calls: state.executed_tools.clone(),
            replayed: replay_notice,
            usage: state.turn_usage.clone(),
            duration_ms: turn_started.elapsed().as_millis() as u64,
        }))
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

    /// Write tools a provider executed inside its own model call as ordinary transcript rows.
    ///
    /// OpenCode runs the tool loop on its side and reports each call as `ToolExecuting` /
    /// `ToolExecuted`, with no `tool_calls` left for this loop to dispatch. Replaying only the
    /// final prose loses that trajectory: a later "continue" cannot see what already ran, and a
    /// stop/continue starts the step over. The synthetic ids are local to this transcript; the
    /// assistant row and its tool rows share them so the shape stays a legal call/result pair.
    async fn persist_provider_tools(
        &self,
        messages: &mut Vec<Message>,
        tools: &[TurnToolCall],
    ) -> Result<(), HarnessError> {
        if tools.is_empty() {
            return Ok(());
        }
        let base = messages.len();
        let calls: Vec<ToolCall> = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| ToolCall {
                id: format!("call_oc_{base}_{index}"),
                name: tool.name.clone(),
                arguments: tool.arguments.clone(),
                signature: None,
            })
            .collect();
        let assistant = Message::assistant_with_tools(calls);
        self.append(&assistant, false).await?;
        messages.push(assistant);
        for (index, tool) in tools.iter().enumerate() {
            let result = ToolResult {
                call_id: format!("call_oc_{base}_{index}"),
                content: tool.output.clone(),
                is_error: tool.is_error,
            };
            let message = Message::tool(result);
            self.append(&message, false).await?;
            messages.push(message);
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

        let mut acc = StreamAccumulator::default();
        match self
            .consume_stream(
                &mut stream,
                &mut acc,
                executed_tools,
                tools_before,
                cancel_token,
                &call_token,
            )
            .await?
        {
            StreamEnd::Ended => {}
            // A tool call is only usable once its arguments are complete; fragments collected so
            // far describe a request the provider never finished. Drop them and keep the prose.
            StreamEnd::Interrupted => return Ok(acc.into_interrupted()),
            StreamEnd::Fallback => {
                return self
                    .buffered_completion(messages, tools, options, cancel_token, &call_token)
                    .await;
            }
            StreamEnd::Cancelled => return Err(HarnessError::Cancelled),
        }

        let tool_calls = acc.assemble_tool_calls(self.provider.provider_id())?;

        if acc.content.trim().is_empty()
            && tool_calls.is_empty()
            && executed_tools.len() == tools_before
        {
            return self
                .empty_stream_reply(acc, messages, tools, options, cancel_token, &call_token)
                .await;
        }

        Ok(acc.into_streamed(tool_calls))
    }

    /// Drain one model stream into `acc`, forwarding every event to the observer as it arrives.
    ///
    /// A stream that errors before producing anything is not the turn's failure: the caller gets
    /// `Fallback` and asks the buffered path instead.
    async fn consume_stream(
        &self,
        stream: &mut futures_util::stream::BoxStream<'static, Result<StreamEvent, HarnessError>>,
        acc: &mut StreamAccumulator,
        executed_tools: &mut Vec<TurnToolCall>,
        tools_before: usize,
        cancel_token: &CancellationToken,
        call_token: &CancellationToken,
    ) -> Result<StreamEnd, HarnessError> {
        loop {
            let event = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => return Ok(StreamEnd::Cancelled),
                _ = call_token.cancelled() => return Ok(StreamEnd::Interrupted),
                ev = stream.next() => match ev {
                    Some(event) => event,
                    None => return Ok(StreamEnd::Ended),
                }
            };

            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    // Nothing arrived, or only reasoning arrived without content/tools:
                    // the buffered path or a clean retry can recover without failing the turn.
                    if acc.content.is_empty()
                        && acc.calls.is_empty()
                        && executed_tools.len() == tools_before
                    {
                        debug!(
                            "Stream failed before any content/tool output ({}), retrying buffered",
                            error
                        );
                        return Ok(StreamEnd::Fallback);
                    }
                    return Err(error);
                }
            };

            acc.apply(self.observer.as_ref(), event, executed_tools);
        }
    }

    /// The reply for a stream that produced no content and no calls of its own.
    ///
    /// A usage or finish frame means the stream ran to the end and the model answered with
    /// nothing; an output-cap finish is diagnosed rather than retried. Only a stream with neither
    /// frame is worth re-asking through the buffered path.
    async fn empty_stream_reply(
        &self,
        acc: StreamAccumulator,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
        cancel_token: &CancellationToken,
        call_token: &CancellationToken,
    ) -> Result<Streamed, HarnessError> {
        let StreamAccumulator {
            reasoning,
            usage,
            finish_reason,
            context_usage,
            ..
        } = acc;

        // A reply cut off by the output cap is not the transient empty shape the buffered retry
        // exists for: the identical request with the identical cap returns the identical nothing,
        // and re-sending it doubles the wait for a diagnosis. Reproduced against Code Assist:
        // `maxOutputTokens=4096` on a long tool-heavy turn spent 3,932 tokens thinking and
        // returned one `MAX_TOKENS` frame with no text.
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
            return Ok(Streamed::done_with_context(
                Message {
                    role: Role::Assistant,
                    content: Some(crate::types::NO_TEXT_RESPONSE.to_string()),
                    reasoning: Some(reasoning),
                    tool_calls: Vec::new(),
                    tool_result: None,
                    usage,
                },
                context_usage,
            ));
        }
        debug!(
            "Stream produced empty content and no tool calls, falling back to buffered completion"
        );
        let buffered = self
            .buffered_completion(messages, tools, options, cancel_token, call_token)
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
                return Ok(Streamed::done_with_context(
                    Message {
                        role: Role::Assistant,
                        content: Some(crate::types::NO_TEXT_RESPONSE.to_string()),
                        reasoning: Some(r),
                        tool_calls: Vec::new(),
                        tool_result: None,
                        usage,
                    },
                    context_usage,
                ));
            }
            return Err(empty_answer_error(self.provider.provider_id()));
        }
        Ok(Streamed::done(buffered))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_accumulator_merges_fragments_and_separates_colliding_calls() {
        let mut acc = StreamAccumulator::default();
        // Normal OpenAI-style streamed call at index 0.
        acc.merge_call_delta(
            0,
            Some("call_1".to_string()),
            Some("skill_view".to_string()),
            Some("{\"name\":".to_string()),
            None,
        );
        acc.merge_call_delta(0, None, None, Some("\"brainstorming\"}".to_string()), None);
        // Second call emitted at index 0 by a provider that omits per-call indices.
        acc.merge_call_delta(
            0,
            Some("call_2".to_string()),
            Some("read_file".to_string()),
            Some("{\"path\":\"Cargo.toml\"}".to_string()),
            None,
        );

        let calls = acc
            .assemble_tool_calls("test")
            .expect("both calls should parse cleanly");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "skill_view");
        assert_eq!(calls[0].arguments, json!({ "name": "brainstorming" }));
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].name, "read_file");
        assert_eq!(calls[1].arguments, json!({ "path": "Cargo.toml" }));
    }
}
