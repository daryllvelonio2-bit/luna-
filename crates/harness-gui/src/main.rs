//! Optional Tauri v2 native desktop application binary (`luna-gui`) embedding the Luna core.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use tauri::Manager;

use state::GuiState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let gui_state = GuiState::initialize()
        .await
        .expect("failed to initialize Luna desktop core");

    tauri::Builder::default()
        .manage(gui_state.clone())
        .setup(move |app| {
            gui_state.attach_app(app.handle().clone());
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_title("Luna");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap_state,
            commands::list_sessions,
            commands::create_session,
            commands::get_session_transcript,
            commands::rename_session,
            commands::fork_session,
            commands::delete_session,
            commands::send_message,
            commands::interrupt_turn,
            commands::answer_approval,
            commands::answer_clarify,
            commands::answer_sudo,
            commands::list_providers_and_models,
            commands::switch_session_model,
            commands::list_credentials,
            commands::set_credential_priority,
            commands::list_plugins,
            commands::toggle_plugin,
            commands::reload_plugin,
            commands::delete_plugin,
            commands::read_plugin_source,
            commands::create_plugin_from_builder,
            commands::update_plugin_settings,
            commands::invoke_plugin_command,
            commands::list_schedules,
            commands::create_schedule,
            commands::update_schedule,
            commands::run_schedule_now,
            commands::search_memory,
            commands::list_notes,
            commands::read_note,
        ])
        .run(tauri::generate_context!())
        .expect("error running Luna native application");
}
