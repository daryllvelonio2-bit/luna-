//! The runtime-toggle arms of the dispatcher: skills, memory, notes, mcp, remember.

use super::*;
use harness_core::SessionStore;

impl<'a> SlashContext<'a> {
    pub(super) async fn cmd_skills(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        // `/steer`, `/queue` and `/interrupt` are not matched here: they act on the chat loop's own
        // state — the pending queue, the steer handle, the cancellation token of the turn in flight
        // — and `chat::turn::handle_composer_command` answers all three, typed or picked from the menu,
        // before dispatch ever reaches this function. A `/steer` arm here would have to take the
        // agent lock, which the running turn holds for its whole duration: the guidance would land
        // only once the turn it was meant to steer had already ended.
        // `/skills on|off` is a toggle; a bare `/skills` keeps its original job of listing.
        if let Some(arg_str) = arg {
            let parts: Vec<&str> = arg_str.split_whitespace().collect();
            match parts.as_slice() {
                ["pin", skill_name, scope] => {
                    self.pin_skill_direct(skill_name, Some(scope)).await;
                    return Ok(SlashOutcome::Continue);
                }
                ["pin", skill_name] => {
                    self.pin_skill_direct(skill_name, None).await;
                    return Ok(SlashOutcome::Continue);
                }
                ["pin"] => {
                    self.pin_skill_interactive().await;
                    return Ok(SlashOutcome::Continue);
                }
                ["unpin", skill_name] => {
                    self.unpin_skill_direct(skill_name, None).await;
                    return Ok(SlashOutcome::Continue);
                }
                ["unpin", skill_name, scope] => {
                    self.unpin_skill_direct(skill_name, Some(scope)).await;
                    return Ok(SlashOutcome::Continue);
                }
                _ => {
                    handle_feature_toggle(self.ui, self.toggles, self.agent, Feature::Skills, arg)
                        .await;
                    return Ok(SlashOutcome::Continue);
                }
            }
        }
        if !self.toggles.enabled(Feature::Skills) {
            self.ui.emit_above(&format!(
                "{} Skills are OFF for this session. Use /skills on to enable them.",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
        }

        let session_pinned = self
            .store
            .get_pinned_skills(self.session_id)
            .await
            .unwrap_or_default();
        let global_pinned = &self.config.agent.pinned_skills;
        let mut pinned_lines = Vec::new();
        for s in &session_pinned {
            pinned_lines.push(format!("  - {} (this session)", s.cyan().bold()));
        }
        for g in global_pinned {
            if !session_pinned
                .iter()
                .any(|sp| harness_plugins::names_match(sp, g))
            {
                pinned_lines.push(format!("  - {} (global)", g.yellow().bold()));
            }
        }
        if !pinned_lines.is_empty() {
            self.ui.emit_above(&format!(
                "\n{}\n{}\n",
                "Pinned skills (always active):".bold(),
                pinned_lines.join("\n")
            ));
        }
        // Every exit path answers something: a silent command reads as a dead key. Both of them
        // answer through `emit_above`, which restores the composer itself — drawing it again
        // here would print a second box below the first.
        match self.skills_readout {
            Some(readout) if !readout.is_empty() => {
                self.ui
                    .emit_above(&format!("\n{}\n{}\n", "Available skills:".bold(), readout));
            }
            _ => {
                self.ui.emit_above(&format!(
                    "{} No skills installed — drop SKILL.md directories into {} and restart",
                    "•".cyan(),
                    self.config.paths.skills_dir.display()
                ));
            }
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_memory(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        handle_memory_command(self.ui, self.toggles, self.agent, arg).await;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_notes(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        handle_feature_toggle(self.ui, self.toggles, self.agent, Feature::Notes, arg).await;
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_remember(&mut self) -> Result<SlashOutcome, HarnessError> {
        let report = {
            let agent = self.agent.lock().await;
            agent.run_memory_pass_now().await
        };
        match report {
            Ok(report) => {
                if report.is_empty() && report.skipped == 0 && report.declined == 0 {
                    self.ui
                        .emit_above(&format!("{} Nothing new worth remembering.", "•".cyan()));
                } else {
                    self.ui.emit_above(&format!(
                        "{} Memory pass: {}",
                        "•".cyan(),
                        report.summary()
                    ));
                }
            }
            Err(error) => self.ui.emit_above(&format!(
                "{} Memory pass failed: {}",
                "✖".red().bold(),
                error
            )),
        }
        Ok(SlashOutcome::Continue)
    }

    pub(super) async fn cmd_mcp(
        &mut self,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        let sub = arg.unwrap_or("").trim();
        if matches!(
            sub,
            "on" | "off" | "enable" | "disable" | "enabled" | "disabled" | "true" | "false"
        ) {
            handle_feature_toggle(self.ui, self.toggles, self.agent, Feature::Mcp, Some(sub)).await;
            if self.toggles.enabled(Feature::Mcp) {
                let registry = self.agent.lock().await.registry().clone();
                if let Err(err) = crate::commands::chat::helpers::ensure_mcp_servers_registered(
                    &registry,
                    self.config,
                )
                .await
                {
                    self.ui.emit_above(&format!(
                        "{} Failed to start MCP servers: {}",
                        "✖".red().bold(),
                        err
                    ));
                }
            }
            return Ok(SlashOutcome::Continue);
        }
        if !self.toggles.enabled(Feature::Mcp) {
            self.ui.emit_above(&format!(
                "{} MCP is OFF for this session. Use /mcp on to enable it.",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
        }
        if sub == "list" {
            let readout = crate::commands::mcp::format_mcp_servers(self.config);
            self.ui.emit_above(&format!("\n{}", readout.trim_end()));
        } else if sub == "tools" {
            self.ui
                .emit_above(&format!("{} Querying MCP tools...", "•".cyan()));
            let readout = crate::commands::mcp::format_mcp_tools(self.config).await;
            self.ui.emit_above(&format!("\n{}", readout.trim_end()));
        } else {
            let servers = crate::commands::mcp::format_mcp_servers(self.config);
            self.ui.emit_above(&format!("\n{}", servers.trim_end()));
            if self.config.mcp.enabled && self.config.mcp.servers.iter().any(|s| s.enabled) {
                self.ui
                    .emit_above(&format!("\n{} Querying MCP tools...", "•".cyan()));
                let tools = crate::commands::mcp::format_mcp_tools(self.config).await;
                self.ui.emit_above(&format!("\n{}", tools.trim_end()));
            }
        }
        Ok(SlashOutcome::Continue)
    }

    async fn pin_skill_interactive(&mut self) {
        let items: Vec<String> = self.skills.iter().map(|s| s.name.clone()).collect();
        if items.is_empty() {
            self.ui
                .emit_above(&format!("{} No skills installed to pin.", "✖".red().bold()));
            return;
        }

        if let Some(rx) = self.key_rx.as_deref_mut() {
            let picked_bools = crate::commands::chat::commands::profile_wizard::multi_step(
                self.ui,
                rx,
                "Pin Skill",
                "Select one or more skills to pin (active on every turn)",
                &items,
            )
            .await;
            let Some(picked) = picked_bools else {
                return;
            };
            let chosen: Vec<String> = items
                .into_iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    if picked.get(i).copied().unwrap_or(false) {
                        Some(s)
                    } else {
                        None
                    }
                })
                .collect();
            if chosen.is_empty() {
                self.ui.emit_above("No skills selected.");
                return;
            }

            let scope_options = ["This session only", "Globally (all sessions)"];
            let scope_choice = crate::commands::chat::commands::profile_wizard::choice_step(
                self.ui,
                rx,
                "Pin Scope",
                "Where should the selected skill(s) be pinned?",
                &scope_options,
                0,
            )
            .await;
            let Some(scope_idx) = scope_choice else {
                return;
            };
            let is_global = scope_idx == 1;

            for skill in &chosen {
                if is_global {
                    if !self
                        .config
                        .agent
                        .pinned_skills
                        .iter()
                        .any(|s| harness_plugins::names_match(s, skill))
                    {
                        self.config.agent.pinned_skills.push(skill.clone());
                    }
                } else {
                    let mut session_pinned = self
                        .store
                        .get_pinned_skills(self.session_id)
                        .await
                        .unwrap_or_default();
                    if !session_pinned
                        .iter()
                        .any(|s| harness_plugins::names_match(s, skill))
                    {
                        session_pinned.push(skill.clone());
                    }
                    let _ = self
                        .store
                        .set_pinned_skills(self.session_id, &session_pinned)
                        .await;
                }
            }

            if is_global {
                let path = harness_core::config::config_file_path();
                if let Ok(yaml) = serde_yaml::to_string(self.config) {
                    let _ = std::fs::write(path, yaml);
                }
            }

            self.refresh_agent_pinned_skills().await;
            self.ui.emit_above(&format!(
                "{} Pinned skill(s) [{}] {}.",
                "✔".green().bold(),
                chosen.join(", ").cyan().bold(),
                if is_global {
                    "globally"
                } else {
                    "to this session"
                }
            ));
        } else {
            self.ui.emit_above(&format!(
                "{} Usage: /skills pin <skill-name> [session|global]",
                "•".cyan()
            ));
        }
    }

    async fn pin_skill_direct(&mut self, skill_name: &str, scope: Option<&str>) {
        let is_global = scope.is_some_and(|s| s.eq_ignore_ascii_case("global"));
        if is_global {
            if !self
                .config
                .agent
                .pinned_skills
                .iter()
                .any(|s| harness_plugins::names_match(s, skill_name))
            {
                self.config.agent.pinned_skills.push(skill_name.to_string());
                let path = harness_core::config::config_file_path();
                if let Ok(yaml) = serde_yaml::to_string(self.config) {
                    let _ = std::fs::write(path, yaml);
                }
            }
            self.refresh_agent_pinned_skills().await;
            self.ui.emit_above(&format!(
                "{} Pinned '{}' globally.",
                "✔".green().bold(),
                skill_name.cyan().bold()
            ));
        } else {
            let mut session_pinned = self
                .store
                .get_pinned_skills(self.session_id)
                .await
                .unwrap_or_default();
            if !session_pinned
                .iter()
                .any(|s| harness_plugins::names_match(s, skill_name))
            {
                session_pinned.push(skill_name.to_string());
            }
            let _ = self
                .store
                .set_pinned_skills(self.session_id, &session_pinned)
                .await;
            self.refresh_agent_pinned_skills().await;
            self.ui.emit_above(&format!(
                "{} Pinned '{}' to this session.",
                "✔".green().bold(),
                skill_name.cyan().bold()
            ));
        }
    }

    async fn unpin_skill_direct(&mut self, skill_name: &str, scope: Option<&str>) {
        let unpin_global =
            scope.is_none() || scope.is_some_and(|s| s.eq_ignore_ascii_case("global"));
        let unpin_session =
            scope.is_none() || scope.is_some_and(|s| s.eq_ignore_ascii_case("session"));

        if unpin_global {
            self.config
                .agent
                .pinned_skills
                .retain(|s| !harness_plugins::names_match(s, skill_name));
            let path = harness_core::config::config_file_path();
            if let Ok(yaml) = serde_yaml::to_string(self.config) {
                let _ = std::fs::write(path, yaml);
            }
        }
        if unpin_session {
            let mut session_pinned = self
                .store
                .get_pinned_skills(self.session_id)
                .await
                .unwrap_or_default();
            session_pinned.retain(|s| !harness_plugins::names_match(s, skill_name));
            let _ = self
                .store
                .set_pinned_skills(self.session_id, &session_pinned)
                .await;
        }
        self.refresh_agent_pinned_skills().await;
        self.ui.emit_above(&format!(
            "{} Unpinned '{}'.",
            "✔".green().bold(),
            skill_name
        ));
    }

    async fn refresh_agent_pinned_skills(&mut self) {
        let session_pinned = self
            .store
            .get_pinned_skills(self.session_id)
            .await
            .unwrap_or_default();
        let mut combined = session_pinned;
        for g in &self.config.agent.pinned_skills {
            if !combined.iter().any(|s| harness_plugins::names_match(s, g)) {
                combined.push(g.clone());
            }
        }
        let resolved = harness_plugins::resolve_pinned_instructions(
            &combined,
            self.skills,
            Some(&self.config.paths.skills_dir),
        );
        let mut ag = self.agent.lock().await;
        ag.set_pinned_skills(combined);
        ag.set_pinned_skill_instructions(resolved);
    }
}
