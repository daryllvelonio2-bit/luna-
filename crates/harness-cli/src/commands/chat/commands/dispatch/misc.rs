//! The remaining arms: connect, dir, tasks, profile, worker, exit, and the fallback.

use super::*;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_connect(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.ui.redraw_input_line();
        // Provider setup needs the composer's key channel. Without one (the TUI routes `/connect`
        // to its own modal) there is nothing to read from, so report instead of blocking.
        let Some(key_rx) = self.key_rx.as_deref_mut() else {
            self.ui.emit_above(&format!(
                "{} Provider setup needs an interactive terminal",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
        };
        // handle_connect_command restores the composer on every exit path; drawing it again
        // here would print a second box below the first.
        if let Err(e) = connect::handle_connect_command(
            key_rx,
            self.ui,
            self.cred_manager,
            Some(self.session_id),
        )
        .await
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
            Some(self.session_id),
        )
        .await;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_dir(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let Some(key_rx) = self.key_rx.as_deref_mut() else {
            self.ui.emit_above(&format!(
                "{} Workspace setup needs an interactive terminal",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
        };
        if let Err(e) = handle_dir_command(key_rx, self.ui, self.config, self.agent, arg).await {
            self.ui.emit_above(&format!(
                "{} Failed to switch workspace: {}",
                "✖".red().bold(),
                e
            ));
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_tasks(&mut self) -> Result<SlashOutcome, HarnessError> {
        let Some(key_rx) = self.key_rx.as_deref_mut() else {
            self.ui.emit_above(&format!(
                "{} Task viewer needs an interactive terminal",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
        };
        viewer::show_background_task_viewer(
            key_rx,
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
        let result = if self.ui.is_attended && !self.ui.is_tui_mode() {
            match self.key_rx.as_deref_mut() {
                Some(rx) => {
                    profile_wizard::handle_profile_command(
                        command,
                        self.config,
                        &global,
                        rx,
                        self.ui,
                    )
                    .await
                }
                None => {
                    crate::commands::profile::handle_profile_command(command, self.config, &global)
                }
            }
        } else {
            // Non-interactive fallback: the plain CLI flow. The reader is paused because
            // `dialoguer` reads stdin directly whenever a terminal is attached.
            if let Some(rx) = self.key_rx.as_deref_mut() {
                while rx.try_recv().is_ok() {}
            }
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
            self.key_rx.as_deref_mut(),
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

    pub(super) async fn cmd_goal(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let plan_mode = self.plan_mode.load(std::sync::atomic::Ordering::SeqCst);
        Ok(handle_goal_command(
            self.ui,
            self.config,
            plan_mode,
            self.active_goal,
            arg,
        ))
    }

    pub(super) async fn cmd_automation(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let Some(request) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            self.ui.emit_above(&format!(
                "{} Usage: {}",
                "•".cyan(),
                format!(
                    "{}<what you want to automate>",
                    harness_scheduler::AUTOMATION_PREFILL
                )
                .yellow()
            ));
            return Ok(SlashOutcome::Continue);
        };
        let prompt = harness_scheduler::build_automation_turn_prompt(request);
        let display = format!("/automation {request}");
        Ok(SlashOutcome::Turn { prompt, display })
    }

    pub(super) async fn cmd_plugin(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let Some(request) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            self.ui.emit_above(&format!(
                "{} Usage: {}",
                "•".cyan(),
                format!(
                    "{}<what plugin you want to build>",
                    harness_plugins::PLUGIN_PREFILL
                )
                .yellow()
            ));
            return Ok(SlashOutcome::Continue);
        };
        let prompt = harness_plugins::build_plugin_turn_prompt(request);
        let display = format!("/plugin {request}");
        Ok(SlashOutcome::Turn { prompt, display })
    }

    pub(super) async fn cmd_voice(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let sub = arg.map(str::trim).map(str::to_ascii_lowercase);
        match sub.as_deref() {
            Some("status") => {
                let is_on = self.agent.lock().await.voice_mode();
                let status = if is_on {
                    format!(
                        "{} Voice mode is ON (provider: {}, wake word: '{}', barge-in: {})",
                        "🎙".cyan(),
                        self.config.voice.provider,
                        self.config.voice.wake_word,
                        if self.config.voice.enable_barge_in {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    )
                } else {
                    format!(
                        "{} Voice mode is OFF (run /voice on to enable or /voice setup to configure)",
                        "🎙".dimmed()
                    )
                };
                self.ui.emit_above(&status);
            }
            Some("setup" | "config") => {
                self.ui.emit_above(&format!(
                    "{} Run {} from the shell or open {} in chat to configure voice settings",
                    "•".cyan(),
                    "luna voice setup".yellow().bold(),
                    "/settings".yellow().bold()
                ));
            }
            Some("off" | "stop" | "disable") => {
                self.agent.lock().await.set_voice_mode(false);
                self.ui
                    .emit_above(&format!("{} Voice mode turned OFF", "•".dimmed()));
            }
            Some("on" | "start" | "enable") => {
                self.agent.lock().await.set_voice_mode(true);
                self.ui.emit_above(&format!(
                    "{} Voice mode turned ON. Spoken voice-compatible summaries enabled (type or talk anytime!).",
                    "✔".green().bold()
                ));
            }
            None => {
                let current = self.agent.lock().await.voice_mode();
                let next = !current;
                self.agent.lock().await.set_voice_mode(next);
                if next {
                    self.ui.emit_above(&format!(
                        "{} Voice mode turned ON. Spoken voice-compatible summaries enabled (type or talk anytime!).",
                        "✔".green().bold()
                    ));
                } else {
                    self.ui
                        .emit_above(&format!("{} Voice mode turned OFF", "•".dimmed()));
                }
            }
            Some(other) => {
                self.ui.emit_above(&format!(
                    "{} Unknown voice option '{}'. Usage: /voice [on|off|status|setup]",
                    "•".cyan(),
                    other
                ));
            }
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
                    Some(task) => match worker::handle_worker_invocation(
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
                        Ok(Some(info)) => return Ok(SlashOutcome::WorkerSpawned(info)),
                        Ok(None) => {}
                        Err(e) => {
                            self.ui.emit_above(&format!(
                                "{} Worker {} failed to start: {}",
                                "✖".red().bold(),
                                worker_name.cyan().bold(),
                                e
                            ));
                        }
                    },
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

    pub(super) async fn cmd_update(&mut self) -> Result<SlashOutcome, HarnessError> {
        self.ui
            .emit_above(&format!("{} Checking for updates...", "•".cyan()));
        match crate::commands::update::run_update_headless(self.config, false).await {
            Ok(Some((from, to))) => {
                self.ui.emit_above(&format!(
                    "{} Successfully updated from {} to {}! Restart luna to apply.",
                    "✔".green().bold(),
                    from,
                    to
                ));
            }
            Ok(None) => {
                self.ui.emit_above(&format!(
                    "{} Luna is already up to date ({}).",
                    "✔".green().bold(),
                    crate::commands::update::CURRENT_DISPLAY
                ));
            }
            Err(e) => {
                self.ui
                    .emit_above(&format!("{} Update failed: {}", "✖".red().bold(), e));
            }
        }
        Ok(SlashOutcome::Continue)
    }
}
