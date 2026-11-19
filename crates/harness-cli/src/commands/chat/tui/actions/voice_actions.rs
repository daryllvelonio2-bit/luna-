//! voice_actions

use super::*;

pub async fn handle_voice_slash_command(
    env: &mut Env<'_>,
    st: &mut TuiState,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let sub = arg.map(str::trim).map(str::to_ascii_lowercase);
    match sub.as_deref() {
        Some("status") => {
            let status = if st.voice_active {
                format!(
                    "🎙 Voice mode is ON (provider: {}, wake word: '{}', barge-in: {})",
                    env.config.voice.provider,
                    env.config.voice.wake_word,
                    if env.config.voice.enable_barge_in {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )
            } else {
                "🎙 Voice mode is OFF (run `/voice on` to enable or `/voice setup` to configure)"
                    .to_string()
            };
            st.system_notices.push(SystemNotice::new(status));
        }
        Some("setup" | "config" | "settings") => {
            open_settings_menu(env, st, 10);
        }
        Some("off" | "stop" | "disable") => {
            if st.voice_active {
                deactivate_voice(env, st).await;
            } else {
                st.system_notices
                    .push(SystemNotice::new("• Voice mode is already off"));
            }
        }
        Some("on" | "start" | "enable") => {
            if !st.voice_active {
                activate_voice(env, st).await;
            } else {
                st.system_notices.push(SystemNotice::new(
                    "✔ Voice mode is already active and listening",
                ));
            }
        }
        None => {
            if st.voice_active {
                deactivate_voice(env, st).await;
            } else {
                activate_voice(env, st).await;
            }
        }
        Some(other) => {
            st.system_notices.push(SystemNotice::new(format!(
                "• Unknown voice option '{other}'. Usage: /voice [on|off|status|setup]"
            )));
        }
    }
    Ok(())
}

pub async fn activate_voice(env: &Env<'_>, st: &mut TuiState) {
    st.voice_active = true;
    env.agent.lock().await.set_voice_mode(true);

    if let Some(old) = st.voice_cancel.take() {
        old.cancel();
    }
    let cancel = CancellationToken::new();
    st.voice_cancel = Some(cancel.clone());

    let voice_tx = st.voice_tx.clone();
    let is_playing = st.voice_is_playing.clone();
    let config = env.config.clone();
    let cred_manager = env.cred_manager.clone();
    let discovery = env.discovery.clone();
    tokio::spawn(async move {
        crate::commands::voice::run_background_voice_listener(
            config,
            cred_manager,
            discovery,
            voice_tx,
            is_playing,
            cancel,
        )
        .await;
    });

    st.system_notices.push(SystemNotice::new(
        "✔ Voice mode activated. Listening in background (type and talk anytime!). Voice-compatible output summaries enabled.",
    ));
}

pub async fn deactivate_voice(env: &Env<'_>, st: &mut TuiState) {
    st.voice_active = false;
    env.agent.lock().await.set_voice_mode(false);

    if let Some(cancel) = st.voice_cancel.take() {
        cancel.cancel();
    }
    st.voice_speech_queue.interrupt();
    st.voice_stream_offset = 0;
    st.voice_stream_in_code = false;
    st.voice_spoken_any = false;
    st.voice_ack_sent = false;
    if let Some(c) = st.voice_reply_cancel.take() {
        c.cancel();
    }
    st.voice_speaking_detected = false;
    st.voice_transcribing = false;
    st.voice_audio_level = 0.0;
    st.voice_pending_autosend_at = None;

    st.system_notices
        .push(SystemNotice::new("• Voice mode deactivated."));
}

fn fast_tool_ack(tool_name: &str) -> &'static str {
    let lower = tool_name.to_ascii_lowercase();
    if lower.contains("search") || lower.contains("find") {
        "Searching now."
    } else if lower.contains("read") || lower.contains("view") || lower.contains("list") {
        "Checking that now."
    } else if lower.contains("write") || lower.contains("patch") {
        "Making changes now."
    } else if lower.contains("terminal") || lower.contains("run") || lower.contains("exec") {
        "Running that now."
    } else {
        "Working on that now."
    }
}

/// Stream newly completed sentences from the live assistant response into `VoiceSpeechQueue`
/// in real time while tokens are generating, matching Antigravity's instant voice feel.
pub fn pump_voice_streaming_tts(env: &Env<'_>, st: &mut TuiState) {
    if !st.voice_active || st.active_turn.is_none() || st.voice_speech_queue.is_turn_muted() {
        return;
    }

    let live_text = env.ui.live_content_snapshot();
    if live_text.len() < st.voice_stream_offset
        || !live_text.is_char_boundary(st.voice_stream_offset)
    {
        st.voice_stream_offset = 0;
        st.voice_stream_in_code = false;
    }

    if live_text.len() > st.voice_stream_offset {
        // Cancel any pending pre-task ack since live assistant response tokens are flowing!
        if let Some(c) = st.voice_reply_cancel.take() {
            c.cancel();
        }

        let (chunks, consumed) = crate::commands::voice::extract_ready_speech_chunks(
            &live_text[st.voice_stream_offset..],
            &mut st.voice_stream_in_code,
            !st.voice_spoken_any,
        );
        st.voice_stream_offset += consumed;
        for chunk in chunks {
            st.voice_speech_queue.enqueue(
                &chunk,
                env.config,
                &env.cred_manager,
                Some(st.voice_tx.clone()),
            );
            st.voice_spoken_any = true;
        }
        return;
    }

    // Only if no response text has arrived yet and tools started running or thinking ran long (>1500ms):
    if !st.voice_ack_sent && !st.voice_spoken_any {
        let (first_tool_hint, elapsed) = {
            let tools = env
                .in_flight_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let first_tool = tools
                .iter()
                .find(|it| !it.is_thinking() && !it.is_assistant_text() && !it.is_user_message())
                .map(|it| it.call_formatted.clone());
            (first_tool, st.turn_start.elapsed())
        };

        if let Some(tool_hint) = first_tool_hint {
            st.voice_ack_sent = true;
            let ack = fast_tool_ack(&tool_hint);
            st.voice_speech_queue.enqueue(
                ack,
                env.config,
                &env.cred_manager,
                Some(st.voice_tx.clone()),
            );
            st.voice_spoken_any = true;
        } else if elapsed >= std::time::Duration::from_millis(2000) {
            st.voice_ack_sent = true;
            let ack = "Working on that now.";
            st.voice_speech_queue.enqueue(
                ack,
                env.config,
                &env.cred_manager,
                Some(st.voice_tx.clone()),
            );
            st.voice_spoken_any = true;
        }
    }
}

pub async fn handle_voice_event(
    env: &Env<'_>,
    st: &mut TuiState,
    evt: crate::commands::voice::VoiceEvent,
) {
    match evt {
        crate::commands::voice::VoiceEvent::Transcript(query) => {
            st.voice_speaking_detected = false;
            st.voice_transcribing = false;
            st.voice_audio_level = 0.0;
            handle_incoming_voice_input(env, st, query).await;
        }
        crate::commands::voice::VoiceEvent::SpeechDetected(level) => {
            st.voice_speaking_detected = true;
            st.voice_transcribing = false;
            st.voice_audio_level = level;
        }
        crate::commands::voice::VoiceEvent::SpeechLevel(level) => {
            if st.voice_speaking_detected {
                st.voice_audio_level = level;
            }
        }
        crate::commands::voice::VoiceEvent::Transcribing => {
            st.voice_speaking_detected = false;
            st.voice_transcribing = true;
            st.voice_audio_level = 0.0;
        }
        crate::commands::voice::VoiceEvent::SpeechIdle => {
            st.voice_speaking_detected = false;
            st.voice_transcribing = false;
            st.voice_audio_level = 0.0;
        }
        crate::commands::voice::VoiceEvent::BargeIn => {
            // Immediate barge-in: stop TTS instantly, mute rest of interrupted turn until new input
            if let Some(c) = st.voice_reply_cancel.take() {
                c.cancel();
            }
            st.voice_speech_queue.interrupt();
            if st.active_turn.is_some() {
                st.voice_speech_queue.set_turn_muted(true);
            }
            st.voice_stream_offset = 0;
            st.voice_stream_in_code = false;
            st.voice_spoken_any = false;
            st.voice_ack_sent = false;
            st.voice_speaking_detected = true;
            st.voice_transcribing = false;
            st.voice_audio_level = 0.6;
            st.system_notices.push(SystemNotice::new("🎤 Listening..."));
        }
        crate::commands::voice::VoiceEvent::Notice(msg) => {
            st.system_notices.push(SystemNotice::new(msg));
        }
        crate::commands::voice::VoiceEvent::Error(err) => {
            st.voice_speaking_detected = false;
            st.voice_transcribing = false;
            st.voice_audio_level = 0.0;
            env.ui.set_transient_error(Some(err.clone()));
            st.system_notices
                .push(SystemNotice::new(format!("✖ {err}")));
        }
    }
}

pub async fn handle_incoming_voice_input(env: &Env<'_>, st: &mut TuiState, voice_query: String) {
    let trimmed = voice_query.trim();
    if trimmed.is_empty() {
        return;
    }
    // Barge-in: interrupt ongoing speech playback immediately and unmute for the new turn
    if let Some(c) = st.voice_reply_cancel.take() {
        c.cancel();
    }
    st.voice_reply_cancel = Some(CancellationToken::new());
    st.voice_speech_queue.interrupt();
    st.voice_speech_queue.set_turn_muted(false);
    st.voice_stream_offset = 0;
    st.voice_stream_in_code = false;
    st.voice_spoken_any = false;
    st.voice_ack_sent = false;

    if !env.config.voice.auto_send {
        // Verify before send: type detected transcript into the composer text box for review/editing.
        st.voice_pending_autosend_at = None;
        st.composer.move_end();
        if !st.composer.text.is_empty()
            && !st
                .composer
                .text
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            st.composer.insert_char(' ');
        }
        st.composer.insert_str(trimmed);
        sync_slash_popup(env, st);
        st.system_notices.push(SystemNotice::new(
            "🎤 Transcribed into input box — verify & press Enter to send",
        ));
        return;
    }

    // Auto-send (default): type into the text box and auto-submit after a brief visual beat
    // unless the user already has a manual draft typed in the composer.
    if st.composer.text.trim().is_empty() || st.voice_pending_autosend_at.is_some() {
        st.composer.move_end();
        if !st.composer.text.is_empty()
            && !st
                .composer
                .text
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            st.composer.insert_char(' ');
        }
        st.composer.insert_str(trimmed);
        sync_slash_popup(env, st);
        st.voice_pending_autosend_at =
            Some(tokio::time::Instant::now() + std::time::Duration::from_millis(160));
        return;
    }

    dispatch_voice_turn_direct(env, st, trimmed);
}

pub async fn flush_voice_pending_autosend(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<Flow, HarnessError> {
    st.voice_pending_autosend_at = None;
    if st.composer.text.trim().is_empty() {
        return Ok(Flow::Continue);
    }
    submit_composer(env, st, terminal).await
}

pub(crate) fn dispatch_voice_turn_direct(env: &Env<'_>, st: &mut TuiState, trimmed: &str) {
    st.system_notices
        .push(SystemNotice::new(format!("🎤 Heard: \"{trimmed}\"")));
    if st.active_turn.is_some() {
        match env.interrupt_handle.interrupt(trimmed) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices
                    .push(SystemNotice::new("↳ Voice interrupting current turn"));
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices
                    .push(SystemNotice::new("↳ Voice queued after current tool"));
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(trimmed.to_string());
                env.ui.set_queue_depth(st.queue.len());
            }
        }
    } else if env.compacting.load(Ordering::SeqCst) {
        st.queue.push_back(trimmed.to_string());
        env.ui.set_queue_depth(st.queue.len());
    } else {
        st.jump_to_latest();
        let display = format!("🎤 {trimmed}");
        launch_turn(
            trimmed,
            Some(&display),
            &env.ui,
            &st.turns,
            st.process_expanded,
            &st.pending_images,
            &env.plan_mode,
            &env.agent,
            &env.in_flight_tools,
            &mut st.current_cancel,
            &mut st.current_query,
            &mut st.turn_start,
            &mut st.active_turn,
        );
    }
}
