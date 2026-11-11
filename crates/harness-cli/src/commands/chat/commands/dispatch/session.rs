//! The session arms of the dispatcher: switch, summarize, new, fork, name, delete.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_name(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        if let Some(a) = arg {
            if let Err(e) =
                handle_name_command(self.ui, self.store.as_ref(), self.session_id, Some(a)).await
            {
                self.ui.emit_above(&format!(
                    "{} Failed to set session name: {}",
                    "✖".red().bold(),
                    e
                ));
            }
        } else {
            if let Ok(mut buf) = self.ui.input_buf.lock() {
                buf.clear();
                buf.push_str("/name ");
            }
            self.ui.redraw_input_line();
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_sessions(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = handle_sessions_switch(
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
            self.compact_provider,
            self.compact_model,
            self.turns,
            self.process_expanded,
            self.todo_scope,
            arg,
        )
        .await
        {
            self.ui.emit_above(&format!(
                "{} Failed to switch session: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_summarize(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.turns.clear();
        self.ui.clear_box_on_exit();
        if let Err(e) = handle_summarize_and_new_command(
            self.ui,
            self.store,
            self.config,
            self.agent,
            self.session_id,
            self.current_provider,
            self.current_model,
            self.todo_scope,
            arg,
        )
        .await
        {
            self.ui.draw_initial_box();
            self.ui.emit_above(&format!(
                "{} Failed to summarize session: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_new(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.turns.clear();
        let (is_summarize, remaining_arg) = if let Some(a) = arg {
            if a.starts_with("-s ") || a == "-s" {
                (true, a.strip_prefix("-s").unwrap().trim())
            } else if a.starts_with("--summarize ") || a == "--summarize" {
                (true, a.strip_prefix("--summarize").unwrap().trim())
            } else if a.starts_with("--summary ") || a == "--summary" {
                (true, a.strip_prefix("--summary").unwrap().trim())
            } else {
                (false, a)
            }
        } else {
            (false, "")
        };
        let rem_opt = if remaining_arg.is_empty() {
            None
        } else {
            Some(remaining_arg)
        };
        self.ui.clear_box_on_exit();
        if is_summarize {
            if let Err(e) = handle_summarize_and_new_command(
                self.ui,
                self.store,
                self.config,
                self.agent,
                self.session_id,
                self.current_provider,
                self.current_model,
                self.todo_scope,
                rem_opt,
            )
            .await
            {
                self.ui.draw_initial_box();
                self.ui.emit_above(&format!(
                    "{} Failed to summarize session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
        } else {
            if let Err(e) = handle_new_command(
                self.ui,
                self.store,
                self.config,
                self.agent,
                self.session_id,
                self.current_provider,
                self.current_model,
                self.todo_scope,
                rem_opt,
            )
            .await
            {
                self.ui.draw_initial_box();
                self.ui.emit_above(&format!(
                    "{} Failed to start new session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_fork(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        handle_fork_command(
            self.ui,
            self.store,
            self.session_id,
            self.todo_scope,
            self.config,
            arg,
        )
        .await?;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_delete(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.turns.clear();
        self.ui.clear_box_on_exit();
        if let Err(e) = handle_delete_command(
            self.ui,
            self.store,
            self.config,
            self.agent,
            self.session_id,
            self.current_provider,
            self.current_model,
        )
        .await
        {
            self.ui.draw_initial_box();
            self.ui.emit_above(&format!(
                "{} Failed to delete session: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }
}
