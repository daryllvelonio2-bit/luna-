//! palette

use super::*;

pub fn all_palette_items() -> Vec<PaletteItem> {
    vec![
        PaletteItem {
            title: "New session".into(),
            shortcut: "ctrl+n".into(),
            category: "Session".into(),
            action: PaletteAction::NewSession,
        },
        PaletteItem {
            title: "Switch session".into(),
            shortcut: "/sessions".into(),
            category: "Session".into(),
            action: PaletteAction::SwitchSession,
        },
        PaletteItem {
            title: "Switch model".into(),
            shortcut: "/model".into(),
            category: "Model".into(),
            action: PaletteAction::SwitchModel,
        },
        PaletteItem {
            title: "Cycle agent mode".into(),
            shortcut: "shift+tab".into(),
            category: "Agent".into(),
            action: PaletteAction::CycleAgentMode,
        },
        PaletteItem {
            title: "Set thinking level".into(),
            shortcut: "/thinking".into(),
            category: "Model".into(),
            action: PaletteAction::SelectThinking,
        },
        PaletteItem {
            title: "Toggle thinking & tool details".into(),
            shortcut: "ctrl+o".into(),
            category: "View".into(),
            action: PaletteAction::ToggleProcessDetails,
        },
        PaletteItem {
            title: "Toggle todo panel".into(),
            shortcut: "ctrl+t".into(),
            category: "View".into(),
            action: PaletteAction::ToggleTodoPanel,
        },
        PaletteItem {
            title: "Compact conversation context".into(),
            shortcut: "/compact".into(),
            category: "Session".into(),
            action: PaletteAction::CompactContext,
        },
        PaletteItem {
            title: "Show session file diffs".into(),
            shortcut: "/diff".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowDiff,
        },
        PaletteItem {
            title: "Show token usage".into(),
            shortcut: "/usage".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowUsage,
        },
        PaletteItem {
            title: "Background jobs & subagents".into(),
            shortcut: "/jobs".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowBackgroundJobs,
        },
        PaletteItem {
            title: "Workspace directory".into(),
            shortcut: "/dir".into(),
            category: "Config".into(),
            action: PaletteAction::SlashCommand("/dir".into()),
        },
        PaletteItem {
            title: "Settings".into(),
            shortcut: "/settings".into(),
            category: "Config".into(),
            action: PaletteAction::SlashCommand("/settings".into()),
        },
        PaletteItem {
            title: "Keyboard shortcuts & help".into(),
            shortcut: "/help".into(),
            category: "Help".into(),
            action: PaletteAction::ShowHelp,
        },
        PaletteItem {
            title: "Quit Luna".into(),
            shortcut: "/exit".into(),
            category: "App".into(),
            action: PaletteAction::Quit,
        },
    ]
}

pub fn filtered_palette_items(query: &str) -> Vec<PaletteItem> {
    let q = query.trim().to_ascii_lowercase();
    all_palette_items()
        .into_iter()
        .filter(|item| {
            q.is_empty()
                || item.title.to_ascii_lowercase().contains(&q)
                || item.category.to_ascii_lowercase().contains(&q)
                || item.shortcut.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_sessions<'a>(
    sessions: &'a [harness_core::SessionRecord],
    query: &str,
) -> Vec<&'a harness_core::SessionRecord> {
    let q = query.trim().to_ascii_lowercase();
    sessions
        .iter()
        .filter(|s| {
            q.is_empty()
                || s.name.to_ascii_lowercase().contains(&q)
                || s.title.to_ascii_lowercase().contains(&q)
                || s.id.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_providers<'a>(
    items: &'a [ProviderPickerItem],
    query: &str,
) -> Vec<&'a ProviderPickerItem> {
    let q = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|p| {
            q.is_empty()
                || p.id.to_ascii_lowercase().contains(&q)
                || p.display_name.to_ascii_lowercase().contains(&q)
                || p.tag.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_models<'a>(items: &'a [ModelPickerItem], query: &str) -> Vec<&'a ModelPickerItem> {
    let q = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|m| {
            q.is_empty()
                || m.model.to_ascii_lowercase().contains(&q)
                || m.provider.to_ascii_lowercase().contains(&q)
                || m.display_name.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

/// Concatenate a rendered line's spans back into plain text (styles and ANSI already resolved).
pub(crate) fn line_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

/// Overlay the selection background on the cell range `[from_col, to_col)` of one line.
///
/// Spans are split at the selection boundaries so the original foreground colours survive and
/// wide glyphs are selected as a whole.
pub(crate) fn highlight_line_selection(
    line: Line<'static>,
    from_col: usize,
    to_col: usize,
) -> Line<'static> {
    if to_col <= from_col {
        return line;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut col = 0usize;

    for span in line.spans {
        let span_style = span.style;
        let mut run = String::new();
        let mut run_selected: Option<bool> = None;

        for ch in span.content.chars() {
            let width = ch.width().unwrap_or(0);
            let selected = if width == 0 {
                run_selected.unwrap_or(false)
            } else {
                col < to_col && col + width > from_col
            };
            if run_selected != Some(selected) {
                if let Some(previous) = run_selected {
                    let style = if previous {
                        span_style.bg(bg_selection())
                    } else {
                        span_style
                    };
                    out.push(Span::styled(std::mem::take(&mut run), style));
                }
                run_selected = Some(selected);
            }
            run.push(ch);
            col += width;
        }

        if let Some(previous) = run_selected {
            let style = if previous {
                span_style.bg(bg_selection())
            } else {
                span_style
            };
            out.push(Span::styled(run, style));
        }
    }

    Line {
        style: line.style,
        alignment: line.alignment,
        spans: out,
    }
}

pub(crate) fn truncate_str(s: &str, max_cells: usize) -> String {
    let clean = clean_terminal_text(s);
    if clean.width() <= max_cells {
        return clean;
    }
    if max_cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in clean.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > max_cells {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}
