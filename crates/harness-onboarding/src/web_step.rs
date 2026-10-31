//! Phase 2f: web access.
//!
//! The last question the first-run flow asks, and the one that decides whether the agent can look
//! anything up at all. Four answers, because "on or off" is the wrong shape for this decision:
//!
//! - **DuckDuckGo** is the default because it needs nothing — no account, no key, no signup — and a
//!   fresh install that cannot search is an install whose first question about a library's current
//!   API is answered from memory.
//! - **Brave** and **Tavily** need a key. The step asks which *environment variable* holds it rather
//!   than asking for the key itself, and says so on screen: a key typed into an onboarding prompt
//!   ends up in a config file, and a config file is copied, shared and committed far more often than
//!   a shell profile.
//! - **Off** registers neither tool, for a machine that should read only what it is given.
//!
//! The step draws with the same panel, entry lines and wrappers as the memory and gateway screens
//! ([`crate::menu`]), so the flow looks like one flow rather than five.

use std::io::{BufRead, Write};

use harness_core::config::SearchBackend;
use harness_core::error::HarnessError;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::store::WebSelection;
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, Typewriter};

/// This step's panel: same brand, different section and question.
pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "WEB ACCESS",
    caption: "May I search the web and read pages for you?",
};

/// The question, in the accent colour.
pub const PROMPT: &str = "Select 1-4 ▸ ";
/// The footer: what to type, what is recorded, and where the key lives.
pub const HINT: &str = "Enter a number, then press Enter — blank keeps DuckDuckGo (1), which needs \
                        no account. Keys are read from the environment and never written to the \
                        config file. Change it later with `luna config set \
                        tools.web.search_backend brave`.";
/// How many rejected answers are tolerated before the flow gives up.
pub const MAX_ATTEMPTS: usize = 10;

/// One option on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What the user types.
    pub id: u8,
    /// The label, in the name colour.
    pub name: &'static str,
    /// What it means in practice, muted, wrapped under the label.
    pub detail: &'static str,
    /// The backend it writes.
    pub backend: SearchBackend,
    /// Whether the pair is registered at all.
    pub enabled: bool,
}

/// The options, in the order they are shown. The first is the default, and the default is what a
/// fresh install gets if the user just presses Enter.
pub const CHOICES: [Choice; 4] = [
    Choice {
        id: 1,
        name: "DuckDuckGo (free, no key)",
        detail: "Searches DuckDuckGo's HTML endpoint. No account, no API key, nothing to sign up \
                 for — and no contract either, so the page could change shape without notice.",
        backend: SearchBackend::DuckDuckGo,
        enabled: true,
    },
    Choice {
        id: 2,
        name: "Brave Search API",
        detail: "A documented JSON API with its own index. Needs a key from brave.com/search/api; \
                 I read it from an environment variable, so it stays out of your config file.",
        backend: SearchBackend::Brave,
        enabled: true,
    },
    Choice {
        id: 3,
        name: "Tavily Search API",
        detail: "Built for agents: each result arrives with extracted page text, which is useful \
                 when the question is about content rather than about which page to open. Needs a \
                 key from tavily.com.",
        backend: SearchBackend::Tavily,
        enabled: true,
    },
    Choice {
        id: 4,
        name: "No web access",
        detail: "Register neither tool. I read only what you put in front of me — nothing leaves \
                 this machine on my behalf.",
        backend: SearchBackend::Disabled,
        enabled: false,
    },
];

/// The default: keep the keyless backend.
pub const DEFAULT_ID: u8 = 1;

/// The option a user typed, or the reason it is not an option.
pub fn parse_selection(answer: &str) -> Result<Choice, String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Ok(choice(DEFAULT_ID).expect("the default is one of the choices"));
    }
    // Names are accepted as well as numbers: this is a decision people have opinions about, and
    // `brave` is both shorter to type and harder to get wrong than counting menu rows.
    if let Ok(backend) = SearchBackend::parse(trimmed) {
        return choice_for(backend)
            .ok_or_else(|| format!("'{trimmed}' is not an option: pick 1, 2, 3, or 4"));
    }
    let id: u8 = trimmed
        .parse()
        .map_err(|_| format!("'{trimmed}' is not a number: pick 1, 2, 3, or 4"))?;
    choice(id).ok_or_else(|| format!("there is no option {id}: pick 1, 2, 3, or 4"))
}

/// The option with this id, if it exists.
pub fn choice(id: u8) -> Option<Choice> {
    CHOICES.iter().copied().find(|choice| choice.id == id)
}

/// The option that writes this backend, if it is offered.
pub fn choice_for(backend: SearchBackend) -> Option<Choice> {
    CHOICES.iter().copied().find(|choice| choice.backend == backend)
}

/// The one-line summary of a choice, for the confirmation screen.
pub fn summary(selection: &WebSelection) -> String {
    if !selection.enabled {
        return "Disabled (no web tools)".to_string();
    }
    match selection.backend {
        SearchBackend::DuckDuckGo => "DuckDuckGo — free, no key needed".to_string(),
        SearchBackend::Brave | SearchBackend::Tavily => format!(
            "{} — key from {}",
            selection.backend.label(),
            selection
                .api_key_env
                .as_deref()
                .unwrap_or_else(|| selection.backend.default_api_key_env())
        ),
        SearchBackend::Disabled => "Disabled (no web tools)".to_string(),
    }
}

/// The body, placed as a centred block: one label line per option, then its wrapped explanation.
pub fn body(size: Size) -> Vec<Line> {
    menu::place_block(body_lines(size), size)
}

/// The body before centring: the lines whose widest one the panel is measured against.
fn body_lines(size: Size) -> Vec<Line> {
    let width = screen::body_limit(size.columns);
    let limit = width.saturating_sub(menu::ENTRY_INDENT);
    let mut lines = Vec::new();
    for (index, option) in CHOICES.iter().enumerate() {
        if index > 0 {
            lines.push(Line::blank());
        }
        lines.push(menu::entry_line(
            option.id,
            option.name,
            (option.id == DEFAULT_ID).then_some(menu::DEFAULT_MARKER),
            width,
        ));
        for wrapped in screen::wrap(option.detail, limit) {
            lines.push(Line::of(Span::muted(wrapped)).indent(menu::ENTRY_INDENT));
        }
    }
    lines
}

/// The header panel, sized to the widest line the body will draw.
pub fn header(size: Size) -> Vec<Line> {
    menu::panel_header(size, &PANEL, menu::widest_of(&body_lines(size)))
}

/// The prompt as it should be written: one short line, centred, cursor left after it.
pub fn prompt_line(size: Size, text: &str) -> Line {
    let line = Line::of(Span::strong(text, ACCENT_COLOR));
    let pad = screen::center_pad(size.columns as usize, line.width());
    line.indent(pad)
}

/// The footer.
pub fn hint_lines(size: Size) -> Vec<Line> {
    menu::muted_wrapped(HINT, size)
}

/// The whole screen for a terminal of `size`.
pub fn render(size: Size) -> Vec<Line> {
    let mut frame = header(size);
    frame.push(Line::blank());
    frame.extend(body(size));
    frame.push(Line::blank());
    frame.push(prompt_line(size, PROMPT));
    frame.push(Line::blank());
    frame.extend(hint_lines(size));
    frame
}

/// The plain text of the screen, for tests.
pub fn render_text(size: Size) -> Vec<String> {
    crate::style::text_of(&render(size))
}

/// Draw the screen and return the chosen web access.
pub fn select<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<WebSelection, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    writer.type_block_quick(&header(size))?;
    writer.blank()?;
    writer.write_block(&body(size))?;
    writer.blank()?;
    writer.write_block(&hint_lines(size))?;
    writer.blank()?;

    let mut chosen = None;
    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            // EOF before an answer: the shell that piped this in has nothing more to say, and the
            // documented default is a working keyless search rather than no web access at all.
            return Ok(WebSelection::default());
        }
        match parse_selection(&answer) {
            Ok(choice) => {
                chosen = Some(choice);
                break;
            }
            Err(reason) => {
                writer.write_centered(&format!("✗ {reason}"), Some(ERROR_COLOR))?;
                if attempt == MAX_ATTEMPTS {
                    break;
                }
            }
        }
    }
    let Some(choice) = chosen else {
        return Err(HarnessError::ConfigurationError(format!(
            "no web access option was selected in {MAX_ATTEMPTS} attempts; run `luna` again when \
             you are ready"
        )));
    };

    if !choice.enabled {
        writer.blank()?;
        writer.write_centered(&format!("✓ {}", summary(&WebSelection::disabled())), Some(NAME_COLOR))?;
        return Ok(WebSelection::disabled());
    }
    if choice.backend.is_keyless() {
        writer.blank()?;
        writer.write_centered(
            &format!(
                "✓ {}",
                summary(&WebSelection {
                    enabled: true,
                    backend: choice.backend,
                    api_key_env: None,
                })
            ),
            Some(NAME_COLOR),
        )?;
        return Ok(WebSelection {
            enabled: true,
            backend: choice.backend,
            api_key_env: None,
        });
    }

    // A keyed backend: ask which variable holds the key, and name the default so a blank answer is
    // still a usable setup on a machine that already exports the conventional name.
    let fallback = choice.backend.default_api_key_env();
    writer.blank()?;
    writer.write_prompt_line(&prompt_line(
        size,
        &format!("{} (blank: {fallback}) ▸ ", choice.name),
    ))?;
    let mut answer = String::new();
    let _ = reader.read_line(&mut answer);
    let named = answer.trim();
    let api_key_env = if named.is_empty() {
        fallback.to_string()
    } else {
        named.to_string()
    };

    let present = std::env::var(&api_key_env)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    writer.blank()?;
    writer.write_centered(
        &format!(
            "✓ {}",
            summary(&WebSelection {
                enabled: true,
                backend: choice.backend,
                api_key_env: Some(api_key_env.clone()),
            })
        ),
        Some(NAME_COLOR),
    )?;
    if present {
        writer.write_centered(
            &format!("${api_key_env} is set: web_search will use it"),
            Some(NAME_COLOR),
        )?;
    } else {
        // Said here rather than discovered mid-session: the choice is still written, because a
        // config file that refuses to record what the user just chose is a worse failure than one
        // that records it and reports the missing variable.
        writer.write_centered(
            &format!("export {api_key_env}=your-key before using web_search"),
            Some(ERROR_COLOR),
        )?;
    }

    Ok(WebSelection {
        enabled: true,
        backend: choice.backend,
        api_key_env: Some(api_key_env),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(columns: u16, rows: u16) -> Size {
        Size::new(columns, rows)
    }

    #[test]
    fn the_keyless_backend_is_the_default_and_off_is_the_fourth_option() {
        assert_eq!(CHOICES.len(), 4);
        assert_eq!(parse_selection("").unwrap().backend, SearchBackend::DuckDuckGo);
        assert!(parse_selection("").unwrap().enabled);
        assert_eq!(choice(4).unwrap().backend, SearchBackend::Disabled);
        assert!(!choice(4).unwrap().enabled);
        assert!(choice(5).is_none());
    }

    #[test]
    fn a_backend_can_be_named_instead_of_counted() {
        assert_eq!(parse_selection("brave\n").unwrap().id, 2);
        assert_eq!(parse_selection(" Tavily ").unwrap().id, 3);
        assert_eq!(parse_selection("ddg").unwrap().id, 1);
        assert_eq!(parse_selection("off").unwrap().id, 4);
        let error = parse_selection("google").unwrap_err();
        assert!(error.contains("pick 1, 2, 3, or 4"), "{error}");
        let error = parse_selection("9").unwrap_err();
        assert!(error.contains("there is no option 9"), "{error}");
    }

    #[test]
    fn the_summary_names_the_backend_and_where_its_key_comes_from() {
        assert_eq!(
            summary(&WebSelection::default()),
            "DuckDuckGo — free, no key needed"
        );
        let brave = WebSelection {
            enabled: true,
            backend: SearchBackend::Brave,
            api_key_env: None,
        };
        assert_eq!(
            summary(&brave),
            "Brave Search API — key from BRAVE_SEARCH_API_KEY"
        );
        let custom = WebSelection {
            enabled: true,
            backend: SearchBackend::Tavily,
            api_key_env: Some("MY_KEY".to_string()),
        };
        assert_eq!(summary(&custom), "Tavily Search API — key from MY_KEY");
        assert_eq!(summary(&WebSelection::disabled()), "Disabled (no web tools)");
    }

    #[test]
    fn every_option_is_listed_with_its_number_and_the_default_marked_once() {
        let text = render_text(size(100, 40)).join("\n");
        for option in CHOICES {
            assert!(text.contains(option.name), "{} missing", option.name);
        }
        assert!(text.contains("WEB ACCESS"));
        assert_eq!(text.matches(menu::DEFAULT_MARKER).count(), 1);
        assert!(text.contains("never written to the config file"), "{text}");
    }

    #[test]
    fn the_screen_fits_a_normal_and_a_short_terminal() {
        for (columns, rows) in [(100u16, 40u16), (100, 20), (60, 24)] {
            let frame = render(size(columns, rows));
            for line in &frame {
                assert!(
                    line.width() <= columns as usize,
                    "{columns}x{rows}: line overflows: {:?}",
                    line.text()
                );
            }
        }
    }

    #[test]
    fn the_panel_widens_with_the_body_and_stops_at_the_header_cap() {
        for columns in [60u16, 100, 200] {
            let geometry = size(columns, 40);
            let frame_width = screen::display_width(header(geometry)[0].text().trim());
            let wide = menu::widest_of(&body_lines(geometry));
            // Exactly the rule `menu::panel_header` applies: clamp to the header range, then to
            // whatever the terminal can hold.
            let expected = wide
                .clamp(menu::HEADER_MIN, menu::HEADER_MAX)
                .min(screen::body_limit(columns))
                + 4;
            assert_eq!(frame_width, expected, "{columns} columns");
        }
        // At a wide terminal it is the cap that stops the box, not the body — which is why the
        // assertion above is not simply "the panel matches the body".
        let wide = size(200, 40);
        assert!(menu::widest_of(&body_lines(wide)) > menu::HEADER_MAX);
    }
}
