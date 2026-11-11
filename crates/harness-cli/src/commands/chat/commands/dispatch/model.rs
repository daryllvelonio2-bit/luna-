//! The model, compaction-model, and reasoning arms of the dispatcher.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_default_model(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = handle_default_model_switch(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.cred_manager,
            self.discovery,
            self.agent,
            self.store,
            self.session_id,
            self.current_provider,
            self.current_model,
            self.compact_model,
            arg,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to set default model: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_model(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = handle_model_switch(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.cred_manager,
            self.discovery,
            self.agent,
            self.store,
            self.session_id,
            self.current_provider,
            self.current_model,
            self.compact_model,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to switch model: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_compact_model(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = handle_compact_model_switch(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.cred_manager,
            self.discovery,
            self.agent,
            self.store.as_ref(),
            self.session_id,
            self.current_provider,
            self.current_model,
            self.compact_provider,
            self.compact_model,
            arg,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to set compaction model: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_bgsaver_model(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = handle_bgsaver_model_command(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.cred_manager,
            self.discovery,
            self.agent,
            self.toggles,
            self.current_provider.as_str(),
            self.current_model.as_str(),
            arg,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to set background saver model: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_reasoning(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) =
            handle_reasoning_switch(Some(&mut *self.key_rx), self.ui, self.agent, arg).await
        {
            self.ui.emit_above(&format!(
                "{} Failed to set reasoning effort: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_thinking(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) =
            handle_thinking_switch(Some(&mut *self.key_rx), self.ui, self.agent, arg).await
        {
            self.ui.emit_above(&format!(
                "{} Failed to set thinking level: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }
}
