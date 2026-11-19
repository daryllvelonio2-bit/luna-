//! Conversational text cleaning, sentence chunking, pre-task acknowledgments, and separate voice summaries.

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::traits::ModelProvider;
use harness_core::types::{CompletionOptions, Message, NO_TEXT_RESPONSE};
use harness_core::{Config, HarnessError};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::playback::VoiceSpeechQueue;
use super::VoiceEvent;

/// Extract ready-to-speak sentences or initial clauses from a growing streamed reply buffer.
pub fn extract_ready_speech_chunks(
    pending: &str,
    in_code_block: &mut bool,
    first_chunk_of_turn: bool,
) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut consumed = 0usize;

    if let Some(last_nl) = pending.rfind('\n') {
        let complete_lines = &pending[..=last_nl];
        for line in complete_lines.split_inclusive('\n') {
            consumed += line.len();
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                *in_code_block = !*in_code_block;
                continue;
            }
            if *in_code_block || trimmed.starts_with('|') || trimmed.starts_with("---") {
                continue;
            }
            let cleaned = clean_single_line_for_speech(trimmed);
            if !cleaned.is_empty() {
                for s in split_sentences_for_tts(&cleaned) {
                    out.push(s);
                }
            }
        }
    }

    let tail = &pending[consumed..];
    if !*in_code_block
        && !tail.trim_start().starts_with("```")
        && !tail.trim_start().starts_with('|')
        && !tail.trim_start().starts_with("---")
    {
        let mut local_pos = 0usize;
        let bytes = tail.as_bytes();
        let mut is_first = first_chunk_of_turn && out.is_empty();

        while local_pos < tail.len() {
            let slice = &tail[local_pos..];
            let mut split_at: Option<usize> = None;

            for (i, ch) in slice.char_indices() {
                let next_idx = i + ch.len_utf8();
                let next_is_space = next_idx < slice.len() && bytes[local_pos + next_idx] == b' ';
                if !next_is_space {
                    continue;
                }
                if ch == '.' || ch == '!' || ch == '?' {
                    let prev_char = slice[..i].chars().next_back();
                    let is_short_number_dot =
                        ch == '.' && prev_char.is_some_and(|c| c.is_ascii_digit()) && i < 6;
                    if !is_short_number_dot && i >= 4 {
                        split_at = Some(next_idx + 1);
                        break;
                    }
                } else if is_first && (ch == ',' || ch == ';' || ch == ':') && i >= 45 {
                    split_at = Some(next_idx + 1);
                    break;
                }
            }

            let Some(end_rel) = split_at else {
                break;
            };
            let segment = &slice[..end_rel];
            let cleaned = clean_single_line_for_speech(segment);
            if !cleaned.is_empty() {
                out.push(cleaned);
                is_first = false;
            }
            local_pos += end_rel;
            consumed += end_rel;
        }
    }

    (out, consumed)
}

pub fn split_sentences_for_tts(cleaned: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let bytes = cleaned.as_bytes();
    for (i, ch) in cleaned.char_indices() {
        let next = i + ch.len_utf8();
        if (ch == '.' || ch == '!' || ch == '?') && next < cleaned.len() && bytes[next] == b' ' {
            let chunk = cleaned[start..next].trim();
            if chunk.len() >= 12 {
                sentences.push(chunk.to_string());
                start = next + 1;
            }
        }
    }
    let rest = cleaned[start..].trim();
    if !rest.is_empty() {
        sentences.push(rest.to_string());
    }
    sentences
}

pub fn clean_single_line_for_speech(trimmed: &str) -> String {
    let stripped = trimmed
        .trim_start_matches(['#', '*', '-', '•'])
        .trim()
        .replace("**", "")
        .replace("__", "")
        .replace('`', "");
    strip_markdown_links(&stripped).trim().to_string()
}

/// Clean conversational response text for speech synthesis.
pub fn clean_text_for_speech(text: &str) -> String {
    let mut cleaned = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if trimmed.starts_with('|') || trimmed.starts_with("---") {
            continue;
        }
        let stripped = clean_single_line_for_speech(trimmed);
        if !stripped.is_empty() {
            cleaned.push_str(&stripped);
            cleaned.push(' ');
        }
    }

    if cleaned.len() > 800 {
        if let Some(pos) = cleaned[..800].rfind(['.', '!', '?']) {
            cleaned.truncate(pos + 1);
        } else {
            cleaned.truncate(800);
        }
    }

    cleaned.trim().to_string()
}

pub fn strip_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open_bracket) = rest.find('[') {
        out.push_str(&rest[..open_bracket]);
        let after_bracket = &rest[open_bracket + 1..];
        if let Some(close_bracket) = after_bracket.find("](") {
            let label = &after_bracket[..close_bracket];
            let after_paren = &after_bracket[close_bracket + 2..];
            if let Some(close_paren) = after_paren.find(')') {
                out.push_str(label);
                rest = &after_paren[close_paren + 1..];
                continue;
            }
        }
        out.push('[');
        rest = after_bracket;
    }
    out.push_str(rest);
    out
}

/// Returns `true` when the full written reply contains structured formatting (Markdown tables,
/// code blocks, headings, lists, raw URLs) or exceeds short conversational length.
pub fn needs_separate_voice_summary(reply: &str) -> bool {
    let trimmed = reply.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("```") || trimmed.contains("http://") || trimmed.contains("https://") {
        return true;
    }
    let mut table_rows = 0usize;
    let mut list_rows = 0usize;
    for line in trimmed.lines() {
        let lt = line.trim_start();
        if lt.starts_with('|') && lt.matches('|').count() >= 2 {
            table_rows += 1;
            if table_rows >= 2 {
                return true;
            }
        }
        if lt.starts_with("# ") || lt.starts_with("## ") || lt.starts_with("### ") {
            return true;
        }
        if lt.starts_with("- ") || lt.starts_with("* ") || lt.starts_with("• ") {
            list_rows += 1;
            if list_rows >= 2 {
                return true;
            }
        } else if let Some(dot_pos) = lt.find(". ") {
            if dot_pos > 0 && dot_pos <= 3 && lt[..dot_pos].chars().all(|c| c.is_ascii_digit()) {
                list_rows += 1;
                if list_rows >= 2 {
                    return true;
                }
            }
        }
    }
    let cleaned = clean_text_for_speech(trimmed);
    if cleaned.chars().count() > 280 {
        return true;
    }
    split_sentences_for_tts(&cleaned).len() > 3
}

/// Check if a sentence is trivial conversational filler / acknowledgment chatter
/// (e.g. "Alright, I'll...", "Sure thing, let me...", "Working on that now...").
pub fn is_filler_speech(sentence: &str) -> bool {
    let s = sentence.trim().to_ascii_lowercase();
    let s = s.trim_start_matches(|c: char| !c.is_alphabetic());
    s.starts_with("alright, i'll")
        || s.starts_with("alright i'll")
        || s.starts_with("alright, let me")
        || s.starts_with("alright let me")
        || s.starts_with("alright, on it")
        || s.starts_with("alright on it")
        || s.starts_with("sure, i'll")
        || s.starts_with("sure i'll")
        || s.starts_with("sure thing, let me")
        || s.starts_with("sure thing, i'll")
        || s.starts_with("okay, i'll")
        || s.starts_with("okay i'll")
        || s.starts_with("ok, i'll")
        || s.starts_with("ok i'll")
        || s.starts_with("working on that")
        || s.starts_with("searching now")
        || s.starts_with("checking that now")
        || s.starts_with("running that now")
        || s.starts_with("making changes now")
        || s.starts_with("let me check")
        || s.starts_with("i'll check")
        || s.starts_with("i will check")
}

/// Deterministic fallback spoken summary when an LLM summary call is unavailable.
pub fn fallback_voice_summary(reply: &str) -> String {
    let cleaned = clean_text_for_speech(reply);
    if cleaned.is_empty() {
        return "Done! I've posted the full details in the chat.".to_string();
    }
    let sentences = split_sentences_for_tts(&cleaned);
    let mut out = String::new();
    for s in sentences.iter().filter(|s| !is_filler_speech(s)).take(2) {
        if out.len() + s.len() > 240 && !out.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(s);
    }
    if out.is_empty() {
        // If all initial sentences were filler, take the first non-filler clause or the substantive tail
        let non_filler: Vec<&String> = sentences.iter().filter(|s| !is_filler_speech(s)).collect();
        if let Some(first_real) = non_filler.first() {
            out.push_str(first_real);
        } else {
            out = cleaned.chars().take(220).collect();
        }
    }
    if reply.contains('|') || reply.contains("```") {
        if !out.ends_with(['.', '!', '?']) {
            out.push('.');
        }
        out.push_str(" I've put the full details in the chat.");
    }
    out
}

/// Fast lightweight text completion helper for pre-task voice acknowledgments and separate
/// spoken summaries using `GenericProviderClient` with a tight 5-second timeout per candidate.
pub async fn generate_fast_voice_text(
    system: &str,
    user_prompt: &str,
    max_tokens: u32,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Result<String, HarnessError> {
    let discovery = Arc::new(ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        24,
        config.providers.clone(),
    ));

    let fast_model_for = |prov: &str, active_default: &str| -> String {
        match prov.to_ascii_lowercase().as_str() {
            "antigravity" | "agy" => "gemini-3-flash".to_string(),
            "gemini" => "gemini-3-flash-preview".to_string(),
            "openai" => "gpt-4o-mini".to_string(),
            "groq" => "llama-3.1-8b-instant".to_string(),
            _ if !active_default.trim().is_empty() => active_default.trim().to_string(),
            _ => "gpt-4o-mini".to_string(),
        }
    };

    let target_prov = if !config.voice.provider.trim().is_empty()
        && !config.voice.provider.trim().eq_ignore_ascii_case("none")
    {
        config.voice.provider.trim().to_ascii_lowercase()
    } else {
        config.model.provider.trim().to_ascii_lowercase()
    };
    let target_model = fast_model_for(&target_prov, &config.model.default);
    let candidates = vec![(target_prov, target_model)];

    let messages = vec![Message::system(system), Message::user(user_prompt)];
    let mut last_err = String::new();

    for (prov, model) in candidates {
        let has_creds = !cred_manager.get_credentials(&prov).await.is_empty()
            || (prov == "antigravity" && !cred_manager.get_credentials("agy").await.is_empty());
        if !has_creds {
            continue;
        }
        let Ok(client) = GenericProviderClient::new(
            prov.clone(),
            model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&prov),
        ) else {
            continue;
        };
        let options = CompletionOptions {
            model: Some(model.clone()),
            temperature: Some(0.4),
            max_tokens: Some(max_tokens.max(512)),
            reasoning_effort: None,
            ..Default::default()
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.complete(&messages, &[], &options),
        )
        .await
        {
            Ok(Ok(msg)) => {
                if let Some(content) = msg.content {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() && trimmed != NO_TEXT_RESPONSE {
                        return Ok(trimmed.to_string());
                    }
                }
                last_err = format!("{prov}/{model} returned empty text");
            }
            Ok(Err(e)) => {
                tracing::debug!(provider = %prov, model = %model, "fast voice text failed: {e}");
                last_err = format!("{prov}/{model}: {e}");
            }
            Err(_) => {
                tracing::debug!(provider = %prov, model = %model, "fast voice text timed out");
                last_err = format!("{prov}/{model} timed out");
            }
        }
    }

    Err(HarnessError::VoiceError(if last_err.is_empty() {
        "No fast voice model credentials available".to_string()
    } else {
        last_err
    }))
}

/// Generate a brief, natural spoken acknowledgment before or as Luna begins executing tools.
#[allow(dead_code)]
pub async fn generate_pre_task_voice_ack(
    user_query: &str,
    tool_hint: Option<&str>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Option<String> {
    let trimmed_query = user_query.trim();
    if trimmed_query.is_empty() {
        return None;
    }

    let (system, prompt) = if let Some(hint) = tool_hint {
        (
            "You are Luna, a helpful AI voice assistant. Generate ONE ultra-brief, natural spoken acknowledgment (4 to 10 words) saying you're starting the user's task, like \"Alright, on it — cleaning up the build artifacts now.\" or \"Sure thing, let me check those files.\" Output ONLY the spoken line with no quotes, no markdown, no preamble.",
            format!("User request: {trimmed_query}\nFirst tool starting: {hint}"),
        )
    } else {
        (
            "You are Luna, a helpful AI voice assistant. If the user's message is an action, command, coding, file, or multi-step task request that takes a moment to execute, output ONE ultra-brief spoken acknowledgment (4 to 10 words, e.g. \"Alright, on it — checking that now.\"). If it is a simple greeting, chat, or direct factual question that doesn't run a task, output exactly SKIP. Output nothing else.",
            format!("User message: {trimmed_query}"),
        )
    };

    match generate_fast_voice_text(system, &prompt, 48, config, cred_manager).await {
        Ok(raw) => {
            let cleaned = clean_text_for_speech(raw.trim().trim_matches('"'));
            if cleaned.is_empty() || cleaned.eq_ignore_ascii_case("skip") {
                if tool_hint.is_some() {
                    Some("Alright, on it — working on that now.".to_string())
                } else {
                    None
                }
            } else {
                Some(cleaned)
            }
        }
        Err(_) => tool_hint.map(|_| "Alright, on it — working on that now.".to_string()),
    }
}

/// Generate a separate conversational spoken response for rich/long/tabular written answers.
pub async fn generate_separate_voice_summary(
    user_query: &str,
    full_reply: &str,
    tool_count: usize,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> String {
    let truncated_reply: String = full_reply.chars().take(5000).collect();
    let system = "You are Luna's spoken voice output. Luna's full written response (which may include Markdown tables, code blocks, lists, or long technical details) is already displayed in the chat UI. Write a SEPARATE, natural 1 to 3 sentence spoken response to say out loud to the user. Summarize the outcome, key answer, or main numbers conversationally using contractions. Never read raw tables, code syntax, file trees, or URLs aloud. If the written reply has a table, code, or detailed breakdown, mention briefly that the full details are in the chat. Output ONLY the spoken text.";
    let prompt = format!(
        "User asked: {}\nTools run: {}\n\nFull written reply shown in chat:\n{}",
        user_query.trim(),
        tool_count,
        truncated_reply
    );

    match generate_fast_voice_text(system, &prompt, 160, config, cred_manager).await {
        Ok(raw) => {
            let cleaned = clean_text_for_speech(&raw);
            if cleaned.is_empty() {
                fallback_voice_summary(full_reply)
            } else {
                cleaned
            }
        }
        Err(_) => fallback_voice_summary(full_reply),
    }
}

/// Spawn an immediate pre-task spoken acknowledgment without blocking the turn or UI loop.
#[allow(dead_code)]
pub fn spawn_pre_task_voice_ack(
    user_query: String,
    tool_hint: Option<String>,
    config: Config,
    cred_manager: Arc<CredentialManager>,
    speech_queue: VoiceSpeechQueue,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        let Some(ack) =
            generate_pre_task_voice_ack(&user_query, tool_hint.as_deref(), &config, &cred_manager)
                .await
        else {
            return;
        };
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        if let Some(ref tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Notice(format!("🔊 {ack}")));
        }
        speech_queue.enqueue(&ack, &config, &cred_manager, voice_tx);
    });
}

/// Speak Luna's final turn response: direct TTS for short conversational prose, or a separate
/// spoken summary when the written response is long or contains tables/code/lists.
#[allow(clippy::too_many_arguments)]
pub fn spawn_separate_voice_reply(
    user_query: String,
    full_reply: String,
    tool_count: usize,
    config: Config,
    cred_manager: Arc<CredentialManager>,
    speech_queue: VoiceSpeechQueue,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    cancel: CancellationToken,
) {
    if full_reply.trim().is_empty() || cancel.is_cancelled() || speech_queue.is_turn_muted() {
        return;
    }

    if !needs_separate_voice_summary(&full_reply) {
        let cleaned = clean_text_for_speech(&full_reply);
        for sentence in split_sentences_for_tts(&cleaned) {
            speech_queue.enqueue(&sentence, &config, &cred_manager, voice_tx.clone());
        }
        return;
    }

    tokio::spawn(async move {
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        let spoken = generate_separate_voice_summary(
            &user_query,
            &full_reply,
            tool_count,
            &config,
            &cred_manager,
        )
        .await;
        if cancel.is_cancelled() || speech_queue.is_turn_muted() || spoken.trim().is_empty() {
            return;
        }
        for sentence in split_sentences_for_tts(&spoken) {
            if cancel.is_cancelled() || speech_queue.is_turn_muted() {
                break;
            }
            speech_queue.enqueue(&sentence, &config, &cred_manager, voice_tx.clone());
        }
    });
}
