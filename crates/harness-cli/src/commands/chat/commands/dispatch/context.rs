//! The transcript/workspace arms of the dispatcher: rewind, diff, compact, auto-compact.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_rewind(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        if let Err(e) = handle_rewind_command(self.ui, self.agent, self.session_id, arg).await {
            self.ui
                .emit_above(&format!("{} Failed to rewind: {}", "✖".red().bold(), e));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_diff(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        if let Err(e) = handle_diff_command(self.ui, self.agent, self.session_id, arg).await {
            self.ui
                .emit_above(&format!("{} Failed to diff: {}", "✖".red().bold(), e));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_compact(&mut self) -> Result<SlashOutcome, HarnessError> {
        if let Err(e) = handle_compact_command(
            self.ui,
            self.config,
            self.agent,
            self.current_provider,
            self.current_model,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to compact session: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_auto_compact(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        // The wizard reads the composer's own keys, so it is handed the channel the menu and
        // this dispatch already drained.
        if let Err(e) =
            handle_auto_compact_command(self.ui, self.agent, arg, Some(&mut *self.key_rx)).await
        {
            self.ui.emit_above(&format!(
                "{} Failed to update auto-compact: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }
}
