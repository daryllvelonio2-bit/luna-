//! The view arms of the dispatcher: expand, collapse, todo, clear, plan, heartbeat, usage, help.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_expand(&mut self) -> Result<SlashOutcome, HarnessError> {
        *self.process_expanded = true;
        if self.ui.is_attended && !self.ui.is_tui_mode() {
            crate::commands::clear_screen();
            draw_transcript(self.turns, *self.process_expanded);
            self.ui.draw_initial_box();
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_collapse(&mut self) -> Result<SlashOutcome, HarnessError> {
        *self.process_expanded = false;
        if self.ui.is_attended && !self.ui.is_tui_mode() {
            crate::commands::clear_screen();
            draw_transcript(self.turns, *self.process_expanded);
            self.ui.draw_initial_box();
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_todo(&mut self) -> Result<SlashOutcome, HarnessError> {
        // The same fold Ctrl+Shift+O does, for the terminals that cannot tell the
        // two apart. The panel is chrome, so it is redrawn in place.
        self.ui.toggle_todo_collapsed();
        todo::refresh_panel(self.ui, self.todo_scope);
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_clear(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.turns.clear();
        let _ = std::fs::remove_file(self.todo_scope.path());
        self.ui.set_todo_panel(None);
        if !self.ui.is_tui_mode() {
            crate::commands::clear_screen();
            self.ui.draw_initial_box();
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_heartbeat(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        crate::commands::heartbeat::handle_chat_heartbeat(
            arg,
            self.ui,
            self.config,
            self.agent,
            self.session_id,
            self.current_provider,
            self.current_model,
            self.turns,
        )
        .await;
        if self.ui.is_attended && !self.ui.is_tui_mode() {
            self.ui.draw_initial_box();
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_usage(&mut self) -> Result<SlashOutcome, HarnessError> {
        if let Err(e) = handle_usage_command(
            self.ui,
            self.config,
            self.agent,
            self.session_id.as_str(),
            self.current_provider.as_str(),
            self.current_model.as_str(),
        )
        .await
        {
            self.ui
                .emit_above(&format!("{} Failed to read usage: {}", "✖".red().bold(), e));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_plan(&mut self) -> Result<SlashOutcome, HarnessError> {
        let current = self.plan_mode.load(std::sync::atomic::Ordering::SeqCst);
        let next = !current;
        self.plan_mode
            .store(next, std::sync::atomic::Ordering::SeqCst);
        self.ui.set_plan_mode(next);
        self.ui.emit_above(&format!(
            "{} Plan mode {}",
            "•".cyan(),
            if next {
                "ON (read-only planning, mutating tools gated)"
                    .cyan()
                    .bold()
            } else {
                "OFF".dimmed()
            }
        ));
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_help(&mut self) -> Result<SlashOutcome, HarnessError> {
        let mut help_text = String::from("\nAvailable commands:\n");
        for cmd in command_entries(
            self.config,
            self.skills,
            self.toggles.enabled(Feature::Skills),
        ) {
            help_text.push_str(&format!(
                "  {:<16} {}\n",
                cmd.name.cyan(),
                cmd.details.dimmed()
            ));
        }
        self.ui.emit_above(&help_text);
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_settings(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        handle_settings_command(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.display_settings,
            self.process_expanded,
            arg,
        )
        .await?;
        Ok(SlashOutcome::Continue)
    }
}
