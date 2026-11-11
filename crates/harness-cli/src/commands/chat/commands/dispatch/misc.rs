//! The remaining arms: connect, dir, tasks, profile, worker, exit, and the fallback.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_connect(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        // handle_connect_command restores the composer on every exit path; drawing it again
        // here would print a second box below the first.
        if let Err(e) =
            connect::handle_connect_command(self.key_rx, self.ui, self.cred_manager).await
        {
            self.ui.emit_above(&format!(
                "{} Failed to open provider setup: {}",
                "✖".red().bold(),
                e
            ));
        }
        refresh_antigravity_status_if_needed(
            self.ui,
            self.current_provider,
            self.current_model,
            self.cred_manager,
        )
        .await;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_dir(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        if let Err(e) = handle_dir_command(self.key_rx, self.ui, self.config, self.agent, arg).await
        {
            self.ui.emit_above(&format!(
                "{} Failed to switch workspace: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_tasks(&mut self) -> Result<SlashOutcome, HarnessError> {
        viewer::show_background_task_viewer(
            self.key_rx,
            self.bg_queue,
            self.delegate_tool,
            self.store.as_ref(),
            self.ui,
            self.turns,
            *self.process_expanded,
        )
        .await;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_profile(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        let Some(command) = parse_profile_command(arg) else {
            self.ui.emit_above(&format!(
        "{} Usage: /profile list | show [name] [--full] | path [name] | create [name] | delete <name>",
        "•".cyan()
    ));
            return Ok(SlashOutcome::Continue);
        };
        let global = match crate::commands::profile::global_config() {
            Ok(config) => config,
            Err(error) => {
                self.ui.emit_above(&format!(
                    "{} Could not load the global config: {}",
                    "✖".red().bold(),
                    error
                ));
                return Ok(SlashOutcome::Continue);
            }
        };
        // Attend an interactive chat with the chat's own modal wizard: it reads the same key
        // channel as approvals and pickers, so the terminal stays in raw mode and no second
        // reader can race the first for a keystroke.
        let result = if self.ui.is_attended {
            profile_wizard::handle_profile_command(
                command,
                self.config,
                &global,
                self.key_rx,
                self.ui,
            )
            .await
        } else {
            // Non-interactive fallback: the plain CLI flow. The reader is paused because
            // `dialoguer` reads stdin directly whenever a terminal is attached.
            while self.key_rx.try_recv().is_ok() {}
            let _pause = keys::pause_input();
            let raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
            if raw {
                let _ = crossterm::terminal::disable_raw_mode();
            }
            let result =
                crate::commands::profile::handle_profile_command(command, self.config, &global);
            if raw {
                let _ = crossterm::terminal::enable_raw_mode();
            }
            result
        };
        if let Err(error) = result {
            self.ui.emit_above(&format!(
                "{} Profile command failed: {}",
                "✖".red().bold(),
                error
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_worker(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        if let Err(e) = worker::handle_worker_command(
            Some(&mut *self.key_rx),
            self.ui,
            self.config,
            self.cred_manager,
            self.discovery,
            self.current_provider,
            self.current_model,
            arg,
        )
        .await
        {
            self.ui
                .emit_above(&format!("{} Worker setup failed: {}", "✖".red().bold(), e));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_exit(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.ui.clear_box_on_exit();
        clear_on_exit();
        Ok(SlashOutcome::Exit)
    }

    pub(super) async fn cmd_fallback(
        &mut self,
        cmd: &str,
        name: &str,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        if let Some(worker_name) = name.strip_prefix('/') {
            let worker_name = worker_name.to_ascii_lowercase();
            if self.config.workers.contains_key(&worker_name) {
                match arg {
                    Some(task) => {
                        if let Err(e) = worker::handle_worker_invocation(
                            &worker_name,
                            task,
                            self.ui,
                            self.config,
                            self.cred_manager,
                            self.discovery,
                            self.agent,
                            self.session_id,
                            self.delegate_tool,
                            self.pending_images,
                        )
                        .await
                        {
                            self.ui.emit_above(&format!(
                                "{} Worker {} failed to start: {}",
                                "✖".red().bold(),
                                worker_name.cyan().bold(),
                                e
                            ));
                        }
                    }
                    None => self.ui.emit_above(&format!(
                        "{} Usage: {}",
                        "•".cyan(),
                        format!("/{worker_name} <task>").yellow()
                    )),
                }
                return Ok(SlashOutcome::Continue);
            }
        }
        // An installed skill is a command too. The caller turns this prompt into a normal
        // turn, so the skill runs with the session's tools, history, and approvals. With
        // `/skills off` the names are not commands, so a typed one falls through to the
        // unrecognized-command message instead of running.
        if self.toggles.enabled(Feature::Skills) {
            if let Some(prompt) = crate::commands::skills::invocation_prompt(cmd, self.skills) {
                return Ok(SlashOutcome::Turn {
                    prompt,
                    display: cmd.trim().to_string(),
                });
            }
        }
        if name.starts_with('/') {
            self.ui.emit_above(&format!(
                "{} Unrecognized command '{}'. Type {} for available commands.",
                "✖".red().bold(),
                name,
                "/help".cyan()
            ));
        }
        Ok(SlashOutcome::Continue)
    }
}
