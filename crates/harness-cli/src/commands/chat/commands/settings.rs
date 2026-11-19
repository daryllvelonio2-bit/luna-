//! The `/settings` slash command: configure in-progress execution display limits,
//! turn completion auto-collapse behavior, and related CLI preferences.

use std::sync::Arc;

use colored::Colorize;
use harness_core::{Config, HarnessError, ThemeMode};

use super::super::tui::theme::set_active_theme;
use super::super::ui::ChatUi;
use super::picker::select_from_rx;
use crate::commands::chat::AppKey;

/// Handle `/settings [args]`.
pub(crate) async fn handle_settings_command(
    key_rx: Option<&mut tokio::sync::mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
    process_expanded: &mut bool,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(arg_str) = arg {
        let parts: Vec<&str> = arg_str.split_whitespace().collect();
        match parts.as_slice() {
            ["limit" | "block-limit", val] => {
                let limit = parse_limit(val)?;
                apply_block_limit(limit, ui, config, display_settings)?;
                return Ok(());
            }
            ["collapse" | "auto-collapse", val] => {
                let enabled = parse_bool(val)?;
                apply_auto_collapse(enabled, ui, config, display_settings, process_expanded)?;
                return Ok(());
            }
            ["thinking" | "auto-hide-thinking", val] => {
                let enabled = parse_bool(val)?;
                apply_auto_hide_thinking(enabled, ui, config, display_settings)?;
                return Ok(());
            }
            ["theme", val] => {
                let theme = parse_theme(val)?;
                apply_theme(theme, ui, config, display_settings)?;
                return Ok(());
            }
            ["voice" | "audio" | "talk", val] => {
                apply_voice_setting(val, ui, config)?;
                return Ok(());
            }
            ["voice" | "audio" | "talk", sub, val] => {
                apply_voice_sub_setting(sub, val, ui, config)?;
                return Ok(());
            }
            ["voice" | "audio" | "talk"] => {
                if let Some(rx) = key_rx {
                    run_voice_wizard(rx, ui, config).await?;
                } else {
                    let status = if config.voice.enabled {
                        format!("enabled ({})", config.voice.provider)
                    } else {
                        "disabled".to_string()
                    };
                    ui.emit_above(&format!(
                        "{} Voice mode is currently: {}",
                        "•".cyan(),
                        status.bold()
                    ));
                }
                return Ok(());
            }
            ["show"] => {
                show_settings(ui, config);
                return Ok(());
            }
            _ => {
                ui.emit_above(&format!(
                    "{} Unknown settings option: {}\nUsage: /settings [theme <dark|light>] [limit <1-10|all>] [collapse <on|off>] [thinking <on|off>] [voice <on|off|antigravity|openai|groq>] [show]",
                    "•".cyan(),
                    arg_str
                ));
                return Ok(());
            }
        }
    }

    if let Some(rx) = key_rx {
        run_settings_wizard(rx, ui, config, display_settings, process_expanded).await?;
    } else {
        show_settings(ui, config);
    }

    Ok(())
}

fn parse_limit(val: &str) -> Result<Option<usize>, HarnessError> {
    match val.to_lowercase().as_str() {
        "all" | "none" | "uncapped" | "0" | "default" => Ok(None),
        s => match s.parse::<usize>() {
            Ok(n) if (1..=10).contains(&n) => Ok(Some(n)),
            _ => Err(HarnessError::ConfigurationError(
                "display block limit must be between 1 and 10, or 'all'/'uncapped'".to_string(),
            )),
        },
    }
}

fn parse_bool(val: &str) -> Result<bool, HarnessError> {
    match val.to_lowercase().as_str() {
        "on" | "true" | "yes" | "enabled" | "1" => Ok(true),
        "off" | "false" | "no" | "disabled" | "0" => Ok(false),
        _ => Err(HarnessError::ConfigurationError(
            "collapse setting must be 'on' or 'off'".to_string(),
        )),
    }
}

fn parse_theme(val: &str) -> Result<ThemeMode, HarnessError> {
    match val.trim().to_ascii_lowercase().as_str() {
        "dark" | "darkmode" | "dark-mode" => Ok(ThemeMode::Dark),
        "light" | "lightmode" | "light-mode" => Ok(ThemeMode::Light),
        _ => Err(HarnessError::ConfigurationError(
            "theme must be 'dark' or 'light'".to_string(),
        )),
    }
}

async fn run_settings_wizard(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
    process_expanded: &mut bool,
) -> Result<(), HarnessError> {
    let current_limit_desc = match config.display.block_limit {
        None => "Uncapped (display everything)".to_string(),
        Some(1) => "1 line".to_string(),
        Some(n) => format!("{n} lines"),
    };
    let current_collapse_desc = if config.display.auto_collapse {
        "Enabled (collapse on finish)"
    } else {
        "Disabled (keep expanded)"
    };
    let current_thinking_desc = if config.display.auto_hide_thinking {
        "Enabled (hide while a tool runs)"
    } else {
        "Disabled (keep thinking visible)"
    };
    let current_theme_desc = match config.display.theme {
        ThemeMode::Dark => "Dark mode (Default)",
        ThemeMode::Light => "Light mode",
    };
    let current_voice_desc = if config.voice.enabled {
        format!(
            "Enabled ({} · {})",
            config.voice.provider, config.voice.wake_word
        )
    } else {
        "Disabled".to_string()
    };

    let main_items = vec![
        format!("1. Display Block Limit · [{current_limit_desc}]"),
        format!("2. Auto-collapse on Turn Finish · [{current_collapse_desc}]"),
        format!("3. Auto-collapse Thinking During Tool Calls · [{current_thinking_desc}]"),
        format!("4. Theme · [{current_theme_desc}]"),
        format!("5. Voice Mode · [{current_voice_desc}]"),
        "6. View Current Settings Summary".to_string(),
    ];

    let choice = select_from_rx(rx, ui, "CLI Settings", &main_items, 0, false, false).await;
    match choice {
        Some(0) => {
            let limit_items = vec![
                "Uncapped / Display everything (Default)".to_string(),
                "1 line".to_string(),
                "2 lines".to_string(),
                "3 lines".to_string(),
                "4 lines".to_string(),
                "5 lines".to_string(),
                "6 lines".to_string(),
                "7 lines".to_string(),
                "8 lines".to_string(),
                "9 lines".to_string(),
                "10 lines".to_string(),
            ];
            let default_idx = config.display.block_limit.unwrap_or(0);
            let limit_choice = select_from_rx(
                rx,
                ui,
                "Select Display Block Limit",
                &limit_items,
                default_idx,
                true,
                false,
            )
            .await;
            if let Some(idx) = limit_choice {
                let limit = if idx == 0 { None } else { Some(idx) };
                apply_block_limit(limit, ui, config, display_settings)?;
            }
        }
        Some(1) => {
            let collapse_items = vec![
                "Enabled (collapse when finished - Default)".to_string(),
                "Disabled (keep expanded)".to_string(),
            ];
            let default_idx = if config.display.auto_collapse { 0 } else { 1 };
            let collapse_choice = select_from_rx(
                rx,
                ui,
                "Auto-collapse on Turn Completion",
                &collapse_items,
                default_idx,
                true,
                false,
            )
            .await;
            if let Some(idx) = collapse_choice {
                let enabled = idx == 0;
                apply_auto_collapse(enabled, ui, config, display_settings, process_expanded)?;
            }
        }
        Some(2) => {
            let thinking_items = vec![
                "Enabled (collapse thinking while a tool runs - Default)".to_string(),
                "Disabled (keep thinking visible)".to_string(),
            ];
            let default_idx = if config.display.auto_hide_thinking {
                0
            } else {
                1
            };
            let thinking_choice = select_from_rx(
                rx,
                ui,
                "Auto-collapse Thinking During Tool Calls",
                &thinking_items,
                default_idx,
                true,
                false,
            )
            .await;
            if let Some(idx) = thinking_choice {
                let enabled = idx == 0;
                apply_auto_hide_thinking(enabled, ui, config, display_settings)?;
            }
        }
        Some(3) => {
            let theme_items = vec!["Dark Mode (Default)".to_string(), "Light Mode".to_string()];
            let default_idx = match config.display.theme {
                ThemeMode::Dark => 0,
                ThemeMode::Light => 1,
            };
            let theme_choice = select_from_rx(
                rx,
                ui,
                "Select TUI Theme",
                &theme_items,
                default_idx,
                true,
                false,
            )
            .await;
            if let Some(idx) = theme_choice {
                let theme = if idx == 1 {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                apply_theme(theme, ui, config, display_settings)?;
            }
        }
        Some(4) => {
            run_voice_wizard(rx, ui, config).await?;
        }
        Some(5) => {
            show_settings(ui, config);
        }
        _ => {}
    }

    Ok(())
}

async fn run_voice_wizard(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &mut Config,
) -> Result<(), HarnessError> {
    let current_status = if config.voice.enabled {
        format!("Enabled ({})", config.voice.provider)
    } else {
        "Disabled".to_string()
    };
    let voice_items = vec![
        format!("Toggle Voice Mode · [{current_status}]"),
        "Provider: Antigravity (Google Live Voice)".to_string(),
        "Provider: OpenAI (Whisper STT + TTS)".to_string(),
        "Provider: Groq (Whisper STT + Edge TTS)".to_string(),
        format!(
            "Barge-in Interrupt · [{}]",
            if config.voice.enable_barge_in {
                "Enabled"
            } else {
                "Disabled"
            }
        ),
    ];
    let choice = select_from_rx(rx, ui, "Voice Settings", &voice_items, 0, true, false).await;
    match choice {
        Some(0) => {
            let next = if config.voice.enabled { "off" } else { "on" };
            apply_voice_setting(next, ui, config)?;
        }
        Some(1) => apply_voice_setting("antigravity", ui, config)?,
        Some(2) => apply_voice_setting("openai", ui, config)?,
        Some(3) => apply_voice_setting("groq", ui, config)?,
        Some(4) => {
            let next = if config.voice.enable_barge_in {
                "off"
            } else {
                "on"
            };
            apply_voice_sub_setting("barge", next, ui, config)?;
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn apply_voice_setting(
    val: &str,
    ui: &ChatUi,
    config: &mut Config,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let (enabled, provider) = match val.to_ascii_lowercase().as_str() {
        "off" | "false" | "disable" | "disabled" | "0" | "none" => (false, None),
        "on" | "true" | "enable" | "enabled" | "1" => (true, None),
        "antigravity" | "agy" | "google" => (true, Some("antigravity")),
        "openai" | "whisper" => (true, Some("openai")),
        "groq" | "edge" => (true, Some("groq")),
        "custom" => (true, Some("custom")),
        other => {
            return Err(HarnessError::ConfigurationError(format!(
                "unknown voice option: '{other}'. Expected 'on', 'off', 'antigravity', 'openai', or 'groq'."
            )));
        }
    };

    config.voice.enabled = enabled;
    let _ = harness_core::config::set_in_file(
        &layers,
        "voice.enabled",
        if enabled { "true" } else { "false" },
    );

    if let Some(p) = provider {
        config.voice.provider = p.to_string();
        let _ = harness_core::config::set_in_file(&layers, "voice.provider", p);
    }

    let desc = if enabled {
        format!("Enabled ({})", config.voice.provider)
    } else {
        "Disabled".to_string()
    };

    ui.emit_above(&format!(
        "{} Voice mode set to: {} (saved to config)",
        "✔".green().bold(),
        desc.bold()
    ));

    Ok(())
}

pub(crate) fn apply_voice_sub_setting(
    sub: &str,
    val: &str,
    ui: &ChatUi,
    config: &mut Config,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    match sub.to_ascii_lowercase().as_str() {
        "wake" | "wakeword" | "wake-word" => {
            config.voice.wake_word = val.to_string();
            let _ = harness_core::config::set_in_file(&layers, "voice.wake_word", val);
            ui.emit_above(&format!(
                "{} Voice wake word set to: '{}' (saved to config)",
                "✔".green().bold(),
                val.bold()
            ));
        }
        "barge" | "barge-in" | "bargein" => {
            let enabled = parse_bool(val)?;
            config.voice.enable_barge_in = enabled;
            let _ = harness_core::config::set_in_file(
                &layers,
                "voice.enable_barge_in",
                if enabled { "true" } else { "false" },
            );
            ui.emit_above(&format!(
                "{} Voice barge-in interrupt set to: {} (saved to config)",
                "✔".green().bold(),
                if enabled {
                    "Enabled".bold()
                } else {
                    "Disabled".bold()
                }
            ));
        }
        "provider" => {
            apply_voice_setting(val, ui, config)?;
        }
        "threshold" | "silence" => {
            let threshold: u64 = val.parse().map_err(|_| {
                HarnessError::ConfigurationError(format!("invalid threshold ms: '{val}'"))
            })?;
            config.voice.silence_threshold_ms = threshold;
            let _ = harness_core::config::set_in_file(&layers, "voice.silence_threshold_ms", val);
            ui.emit_above(&format!(
                "{} Voice silence threshold set to: {}ms (saved to config)",
                "✔".green().bold(),
                threshold
            ));
        }
        other => {
            return Err(HarnessError::ConfigurationError(format!(
                "unknown voice setting '{other}'. Supported: provider, wake, barge, threshold"
            )));
        }
    }
    Ok(())
}

fn apply_block_limit(
    limit: Option<usize>,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
) -> Result<(), HarnessError> {
    config.display.block_limit = limit;
    ui.set_display_block_limit(limit);
    if let Ok(mut lock) = display_settings.write() {
        lock.block_limit = limit;
    }

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let desc = match limit {
        None => {
            let _ = harness_core::config::unset_in_file(&layers, "display.block_limit");
            "Uncapped (display everything)".to_string()
        }
        Some(n) => {
            let _ =
                harness_core::config::set_in_file(&layers, "display.block_limit", &n.to_string());
            format!("{n} lines")
        }
    };

    ui.emit_above(&format!(
        "{} Display block limit set to: {} (saved to config)",
        "✔".green().bold(),
        desc.bold()
    ));

    Ok(())
}

fn apply_auto_collapse(
    enabled: bool,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
    process_expanded: &mut bool,
) -> Result<(), HarnessError> {
    config.display.auto_collapse = enabled;
    *process_expanded = !enabled;
    if let Ok(mut lock) = display_settings.write() {
        lock.auto_collapse = enabled;
    }

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let _ = harness_core::config::set_in_file(
        &layers,
        "display.auto_collapse",
        if enabled { "true" } else { "false" },
    );

    let desc = if enabled {
        "Enabled (collapse when finished)"
    } else {
        "Disabled (keep expanded)"
    };

    ui.emit_above(&format!(
        "{} Auto-collapse on turn finish set to: {} (saved to config)",
        "✔".green().bold(),
        desc.bold()
    ));

    Ok(())
}

fn apply_auto_hide_thinking(
    enabled: bool,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
) -> Result<(), HarnessError> {
    config.display.auto_hide_thinking = enabled;
    if let Ok(mut lock) = display_settings.write() {
        lock.auto_hide_thinking = enabled;
    }

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let _ = harness_core::config::set_in_file(
        &layers,
        "display.auto_hide_thinking",
        if enabled { "true" } else { "false" },
    );

    let desc = if enabled {
        "Enabled (hide thinking while a tool runs)"
    } else {
        "Disabled (keep thinking visible)"
    };

    ui.emit_above(&format!(
        "{} Auto-collapse thinking during tool calls set to: {} (saved to config)",
        "✔".green().bold(),
        desc.bold()
    ));

    Ok(())
}

fn apply_theme(
    theme: ThemeMode,
    ui: &ChatUi,
    config: &mut Config,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
) -> Result<(), HarnessError> {
    config.display.theme = theme;
    set_active_theme(theme);
    if let Ok(mut lock) = display_settings.write() {
        lock.theme = theme;
    }

    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let _ = harness_core::config::set_in_file(&layers, "display.theme", theme.label());

    let desc = match theme {
        ThemeMode::Dark => "Dark mode (default)",
        ThemeMode::Light => "Light mode",
    };

    ui.emit_above(&format!(
        "{} Theme set to: {} (saved to config)",
        "✔".green().bold(),
        desc.bold()
    ));

    Ok(())
}

fn show_settings(ui: &ChatUi, config: &Config) {
    let summary = format_settings_summary(config, None);
    ui.emit_above(&format!(
        "\n{}\n{}\n",
        "Current CLI Settings:".bold(),
        summary
    ));
}

/// Plain-text summary of CLI, feature toggle, and memory saver settings.
pub(crate) fn format_settings_summary(
    config: &Config,
    toggles: Option<&super::FeatureToggles>,
) -> String {
    let limit_str = match config.display.block_limit {
        None => "Uncapped (display everything)".to_string(),
        Some(1) => "1 line".to_string(),
        Some(n) => format!("{n} lines"),
    };
    let collapse_str = if config.display.auto_collapse {
        "Enabled (collapse when finished)"
    } else {
        "Disabled (keep expanded)"
    };
    let thinking_str = if config.display.auto_hide_thinking {
        "Enabled (hide while a tool runs)"
    } else {
        "Disabled (keep thinking visible)"
    };
    let theme_str = match config.display.theme {
        ThemeMode::Dark => "Dark mode (default)",
        ThemeMode::Light => "Light mode",
    };
    let compact_str = if config.auto_compact_enabled {
        "Enabled"
    } else {
        "Disabled"
    };
    let saver_str = match config.memory.mode {
        harness_core::MemoryMode::Session => "Session writer (saves mid-turn)",
        harness_core::MemoryMode::Background => "Background saver (distills after reply)",
    };
    let voice_str = if config.voice.enabled {
        format!(
            "Enabled (provider: {}, wake word: '{}', barge-in: {})",
            config.voice.provider,
            config.voice.wake_word,
            if config.voice.enable_barge_in {
                "on"
            } else {
                "off"
            }
        )
    } else {
        "Disabled".to_string()
    };
    let approval_model_str = match (
        config
            .tools
            .approval_provider
            .as_deref()
            .filter(|s| !s.is_empty()),
        config
            .tools
            .approval_model
            .as_deref()
            .filter(|s| !s.is_empty()),
    ) {
        (Some(p), Some(m)) => match config.tools.approval_reasoning_effort {
            Some(e) => format!("{m} ({p}, thinking: {e})"),
            None => format!("{m} ({p})"),
        },
        _ => "Default model".to_string(),
    };

    let feat_line = |f: super::Feature| -> String {
        let live = toggles.map(|t| t.enabled(f));
        let (global_on, until) = match f {
            super::Feature::Mcp => (
                config.mcp.enabled && config.agent.mcp_enabled,
                config.features.mcp_disabled_until,
            ),
            super::Feature::Skills => (
                config.agent.skills_enabled,
                config.features.skills_disabled_until,
            ),
            super::Feature::Memory => (
                config.agent.memory_enabled,
                config.features.memory_disabled_until,
            ),
            super::Feature::Notes => (
                config.agent.notes_enabled,
                config.features.notes_disabled_until,
            ),
        };
        let now = Config::current_unix_secs();
        let active = live.unwrap_or_else(|| global_on && !until.is_some_and(|u| now < u));
        if active {
            if !global_on || until.is_some_and(|u| now < u) {
                "ON (this session)".to_string()
            } else {
                "ON".to_string()
            }
        } else if let Some(u) = until.filter(|&u| u > now) {
            let days = (u - now).div_ceil(86_400);
            format!("OFF ({days}d remaining)")
        } else if !global_on {
            "OFF (global)".to_string()
        } else {
            "OFF (this session)".to_string()
        }
    };

    format!(
        "  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}",
        "Theme:",
        theme_str,
        "Display Block Limit:",
        limit_str,
        "Auto-collapse on Finish:",
        collapse_str,
        "Auto-collapse Thinking:",
        thinking_str,
        "Auto-compact Context:",
        compact_str,
        "Memory Saver Mode:",
        saver_str,
        "Voice Mode:",
        voice_str,
        "Approval Model:",
        approval_model_str,
        "MCP:",
        feat_line(super::Feature::Mcp),
        "Skills:",
        feat_line(super::Feature::Skills),
        "Memory:",
        feat_line(super::Feature::Memory),
        "Notes:",
        feat_line(super::Feature::Notes),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_theme_accepts_dark_and_light_variants() {
        assert_eq!(parse_theme("dark").unwrap(), ThemeMode::Dark);
        assert_eq!(parse_theme("DARK").unwrap(), ThemeMode::Dark);
        assert_eq!(parse_theme("light").unwrap(), ThemeMode::Light);
        assert_eq!(parse_theme("light-mode").unwrap(), ThemeMode::Light);
        assert!(parse_theme("sepia").is_err());
    }

    #[test]
    fn format_settings_summary_reports_theme() {
        let mut cfg = Config::default();
        assert!(format_settings_summary(&cfg, None).contains("Dark mode (default)"));
        cfg.display.theme = ThemeMode::Light;
        assert!(format_settings_summary(&cfg, None).contains("Light mode"));
    }

    #[test]
    fn format_settings_summary_reports_voice() {
        let mut cfg = Config::default();
        assert!(format_settings_summary(&cfg, None).contains("Disabled"));
        cfg.voice.enabled = true;
        cfg.voice.provider = "antigravity".to_string();
        cfg.voice.wake_word = "Luna".to_string();
        let summary = format_settings_summary(&cfg, None);
        assert!(summary.contains("Voice Mode:"));
        assert!(summary.contains("antigravity"));
        assert!(summary.contains("Luna"));
    }
}
