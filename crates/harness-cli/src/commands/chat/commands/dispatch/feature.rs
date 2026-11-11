//! The runtime-toggle arms of the dispatcher: skills, memory, notes, mcp, remember.

use super::*;

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
        if arg.is_some() {
            handle_feature_toggle(self.ui, self.toggles, self.agent, Feature::Skills, arg).await;
            return Ok(SlashOutcome::Continue);
        }
        if !self.toggles.enabled(Feature::Skills) {
            self.ui.emit_above(&format!(
                "{} Skills are OFF for this session. Use /skills on to enable them.",
                "•".cyan()
            ));
            return Ok(SlashOutcome::Continue);
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
        handle_feature_toggle(self.ui, self.toggles, self.agent, Feature::Memory, arg).await;
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
}
