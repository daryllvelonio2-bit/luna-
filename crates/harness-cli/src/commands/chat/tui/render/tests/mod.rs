use super::*;
use std::collections::HashSet;

pub(crate) fn test_dummy_tui_state() -> TuiState {
    let (picker_tx, picker_rx) = tokio::sync::mpsc::unbounded_channel();
    let (approval_reply_tx, approval_reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (voice_tx, voice_rx) = tokio::sync::mpsc::unbounded_channel();
    let tmp = tempfile::tempdir().unwrap();
    let memory = std::sync::Arc::new(harness_memory::MemoryEngineKind::Sqlite(
        harness_memory::SqliteMemoryEngine::in_memory().unwrap(),
    ));
    let notes = std::sync::Arc::new(
        harness_notes::FileNotesEngine::new(tmp.path().join("notes"), None).unwrap(),
    );
    let registry = std::sync::Arc::new(harness_core::DefaultToolRegistry::new());
    let creds = harness_providers::CredentialManager::new(None).unwrap();
    let discovery = std::sync::Arc::new(harness_providers::ModelDiscovery::new(
        None,
        0,
        Default::default(),
    ));
    let provider = std::sync::Arc::new(
        harness_providers::GenericProviderClient::new(
            "openai".to_string(),
            "gpt-4o".to_string(),
            creds,
            discovery,
            None,
        )
        .unwrap(),
    );
    let agent = std::sync::Arc::new(harness_core::Agent::new(
        provider,
        memory,
        notes,
        registry,
        harness_core::AgentConfig::default(),
    ));

    TuiState {
        turns: Vec::new(),
        turn_meta: Vec::new(),
        system_notices: Vec::new(),
        process_expanded: false,
        auto_hide_thinking: true,
        block_limit: None,
        expanded_thinking: HashSet::new(),
        expanded_tools: HashSet::new(),
        expanded_live_thinking: HashSet::new(),
        expanded_live_tools: HashSet::new(),
        live_thinking_view: LiveThinkingView::Tail,
        reasoning_effort: None,
        pending_model_choice: None,
        mode: crate::commands::chat::tui::state::AgentMode::Build,
        composer: crate::commands::chat::tui::state::ComposerState::default(),
        tabs: Vec::new(),
        total_sessions_count: 1,
        hit_boxes: Vec::new(),
        transcript_hit_boxes: Vec::new(),
        composer_hit_boxes: Vec::new(),
        pointer: None,
        last_click: None,
        click_count: 0,
        modal_rect: None,
        composer_area: (0, 0, 0, 0),
        composer_input_area: (0, 0, 0, 0),
        composer_scroll_off: 0,
        transcript_area: (0, 0, 0, 0),
        transcript_start: 0,
        transcript_visible_text: Vec::new(),
        selection: None,
        pending_transcript_click: None,
        selection_notice: None,
        scroll_from_bottom: 0,
        scroll_anchor_top: None,
        max_scroll: 0,
        modal: ActiveModal::None,
        slash_popup: crate::commands::chat::tui::state::SlashPopupState::default(),
        queue: std::collections::VecDeque::new(),
        current_cancel: tokio_util::sync::CancellationToken::new(),
        active_turn: None,
        active_goal: None,
        current_query: String::new(),
        turn_start: std::time::Instant::now(),
        quota_refreshed_at: std::time::Instant::now(),
        idle_pass: crate::commands::chat::helpers::IdleMemoryPass::new(1),
        pending_job_turn: None,
        pending_images: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        history_path: None,
        ctrl_c_armed: None,
        spinner_tick: 0,
        cwd_label: "/workspace".to_string(),
        version: "0.1.8".to_string(),
        update_notice: None,
        last_rendered_rect: (100, 30),
        provider_items: Vec::new(),
        picker_loading: None,
        picker_tx,
        picker_rx,
        approval_reply_tx,
        approval_reply_rx,
        voice_active: false,
        voice_cancel: None,
        voice_speech_queue: crate::commands::voice::VoiceSpeechQueue::new(),
        voice_is_playing: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_speaking_detected: false,
        voice_transcribing: false,
        voice_audio_level: 0.0,
        voice_auto_send: true,
        voice_pending_autosend_at: None,
        voice_stream_offset: 0,
        voice_stream_in_code: false,
        voice_spoken_any: false,
        voice_ack_sent: false,
        voice_reply_cancel: None,
        voice_tx,
        voice_rx,
        settled_cache: crate::commands::chat::tui::state::SettledTranscriptCache::default(),
        live_content_cache: crate::commands::chat::tui::state::LiveContentCache::default(),
        live_reasoning_cache: crate::commands::chat::tui::state::LiveReasoningCache::default(),
        template_agent: agent,
        background_sessions: std::collections::HashMap::new(),
        memory_project_filter: None,
    }
}

mod modal_tests;
mod render_tests;
