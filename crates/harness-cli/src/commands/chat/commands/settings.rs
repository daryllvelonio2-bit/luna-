//! The `/settings` slash command: configure in-progress execution display limits,
//! turn completion auto-collapse behavior, and related CLI preferences.

use std::sync::Arc;

use colored::Colorize;
use harness_core::{Config, HarnessError};

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
            ["show"] => {
                show_settings(ui, config);
                return Ok(());
            }
            _ => {
                ui.emit_above(&format!(
                    "{} Unknown settings option: {}\nUsage: /settings [limit <1-10|all>] [collapse <on|off>] [thinking <on|off>] [show]",
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

    let main_items = vec![
        format!("1. Display Block Limit · [{current_limit_desc}]"),
        format!("2. Auto-collapse on Turn Finish · [{current_collapse_desc}]"),
        format!("3. Auto-collapse Thinking During Tool Calls · [{current_thinking_desc}]"),
        "4. View Current Settings Summary".to_string(),
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
            show_settings(ui, config);
        }
        _ => {}
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

fn show_settings(ui: &ChatUi, config: &Config) {
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

    let report = format!(
        "\n{}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n  {:<26} {}\n",
        "Current CLI Settings:".bold(),
        "Display Block Limit:".dimmed(),
        limit_str.cyan(),
        "Auto-collapse on Finish:".dimmed(),
        collapse_str.cyan(),
        "Auto-collapse Thinking:".dimmed(),
        thinking_str.cyan(),
        "Auto-compact Context:".dimmed(),
        if config.auto_compact_enabled {
            "Enabled".green()
        } else {
            "Disabled".yellow()
        },
        "Memory Mode:".dimmed(),
        format!("{:?}", config.memory.mode).cyan(),
    );
    ui.emit_above(&report);
}
