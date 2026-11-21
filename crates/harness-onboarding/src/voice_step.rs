//! Voice mode setup step.
//!
//! Configures Luna's hands-free voice interface and audio settings:
//!
//! - **Antigravity / Google Live Voice** (default): real-time bidirectional audio streaming over
//!   WebSocket with sub-second latency and natural barge-in, using your authenticated Google
//!   Antigravity session.
//! - **OpenAI Voice**: Whisper speech-to-text paired with OpenAI high-fidelity text-to-speech.
//! - **Groq Whisper + Edge TTS**: Free and fast cloud Whisper STT on Groq paired with Microsoft Edge TTS.
//! - **Custom Voice Setup**: Interactive configuration for custom voice parameters (wake word,
//!   custom provider endpoint, speech models, voice name, and sensitivity threshold).
//! - **Disabled**: Disables voice features. Luna operates strictly via text.

use std::io::{BufRead, Write};

use harness_core::error::HarnessError;

use crate::menu::{self, Panel};
use crate::screen::{self, Size};
use crate::store::VoiceSelection;
use crate::style::{Line, Span, ACCENT_COLOR, ERROR_COLOR, NAME_COLOR};
use crate::typewriter::{Pacer, Typewriter};

/// This step's panel: consistent styling with other onboarding steps.
pub const PANEL: Panel = Panel {
    title: "🌙  LUNA",
    subtitle: "VOICE MODE",
    caption: "How would you like to interact with voice?",
};

/// The prompt in the accent colour.
pub const PROMPT: &str = "Select 1-5 ▸ ";

/// The footer: what to type and what is recorded.
pub const HINT: &str = "Enter a number, then press Enter — blank keeps Voice disabled (5). \
     Change it anytime later with `luna setup voice` or `/voice on`.";

/// How many rejected answers are tolerated before falling back to default.
pub const MAX_ATTEMPTS: usize = 10;

/// One option on this screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub id: u8,
    pub name: &'static str,
    pub detail: &'static str,
    pub provider: &'static str,
    pub enabled: bool,
}

pub const CHOICES: [Choice; 5] = [
    Choice {
        id: 1,
        name: "Antigravity / Google Live Voice",
        detail: "Real-time bidirectional conversational voice using your authenticated Google \
                 Antigravity session. Sub-second latency, natural speech, and native barge-in.",
        provider: "antigravity",
        enabled: true,
    },
    Choice {
        id: 2,
        name: "OpenAI Voice (Whisper STT + OpenAI TTS)",
        detail: "Speech recognition via Whisper-1 and natural text-to-speech with OpenAI \
                 voices (alloy, echo, nova, etc.). Uses OPENAI_API_KEY from environment or pool.",
        provider: "openai",
        enabled: true,
    },
    Choice {
        id: 3,
        name: "Groq Whisper + Edge TTS (Ultra-fast & Free)",
        detail: "Ultra-fast Whisper cloud transcription on Groq free tier paired with Microsoft \
                 Edge TTS. Uses GROQ_API_KEY from environment or pool.",
        provider: "groq",
        enabled: true,
    },
    Choice {
        id: 4,
        name: "Custom Voice Setup",
        detail: "Customize your own setup: pick custom STT/TTS providers, speech models, \
                 API keys, wake word, voice personality, and sensitivity threshold.",
        provider: "custom",
        enabled: true,
    },
    Choice {
        id: 5,
        name: "Disabled (no voice mode) [Default]",
        detail:
            "Keep voice and microphone disabled. Luna runs strictly via text and terminal without \
                 opening audio devices.",
        provider: "none",
        enabled: false,
    },
];

pub const DEFAULT_ID: u8 = 5;

/// The option a user typed, or the reason it is not an option.
pub fn parse_selection(answer: &str) -> Result<Choice, String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return choice(DEFAULT_ID)
            .ok_or_else(|| format!("internal error: default option {DEFAULT_ID} is missing"));
    }

    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "1" | "antigravity" | "agy" | "google" => return Ok(CHOICES[0]),
        "2" | "openai" | "whisper" => return Ok(CHOICES[1]),
        "3" | "groq" | "edge" => return Ok(CHOICES[2]),
        "4" | "custom" | "customized" => return Ok(CHOICES[3]),
        "5" | "off" | "disabled" | "disable" | "none" | "0" => return Ok(CHOICES[4]),
        _ => {}
    }

    if let Ok(id) = trimmed.parse::<u8>() {
        choice(id).ok_or_else(|| format!("there is no option {id}: pick 1, 2, 3, 4, or 5"))
    } else {
        Err(format!(
            "'{trimmed}' is not an option: pick 1, 2, 3, 4, or 5"
        ))
    }
}

pub fn choice(id: u8) -> Option<Choice> {
    CHOICES.iter().copied().find(|c| c.id == id)
}

/// One-line summary for the confirmation screen.
pub fn summary(selection: &VoiceSelection) -> String {
    if !selection.enabled {
        return "Disabled (no voice mode)".to_string();
    }
    match selection.provider.as_str() {
        "antigravity" => format!("Antigravity Google Live (voice: {})", selection.voice_name),
        "openai" => format!(
            "OpenAI Voice (voice: {}, key: {})",
            selection.voice_name,
            selection.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY")
        ),
        "groq" => format!(
            "Groq Whisper + Edge TTS (key: {})",
            selection.api_key_env.as_deref().unwrap_or("GROQ_API_KEY")
        ),
        "custom" => format!(
            "Custom {} (voice: {}, wake word: {})",
            selection.provider, selection.voice_name, selection.wake_word
        ),
        _ => format!("{} (voice: {})", selection.provider, selection.voice_name),
    }
}

pub fn body(size: Size) -> Vec<Line> {
    menu::place_block(body_lines(size), size)
}

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

pub fn header(size: Size) -> Vec<Line> {
    menu::panel_header(size, &PANEL, menu::widest_of(&body_lines(size)))
}

pub fn prompt_line(size: Size, text: &str) -> Line {
    let line = Line::of(Span::strong(text, ACCENT_COLOR));
    let pad = screen::center_pad(size.columns as usize, line.width());
    line.indent(pad)
}

pub fn hint_lines(size: Size) -> Vec<Line> {
    menu::muted_wrapped(HINT, size)
}

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

pub fn render_text(size: Size) -> Vec<String> {
    crate::style::text_of(&render(size))
}

/// Draw the screen and return the chosen voice configuration.
pub fn select<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
) -> Result<VoiceSelection, HarnessError> {
    writer.clear_screen()?;
    let size = writer.size();
    writer.type_block_quick(&header(size))?;
    writer.blank()?;
    writer.write_block(&body(size))?;
    writer.blank()?;
    writer.write_block(&hint_lines(size))?;
    writer.blank()?;

    let Some(choice) = prompt_choice(writer, reader, size)? else {
        return Ok(VoiceSelection::default());
    };

    match choice.id {
        1 => finish_antigravity(writer, reader, size),
        2 => finish_openai(writer, reader, size),
        3 => finish_groq(writer, reader, size),
        4 => finish_custom(writer, reader, size),
        _ => finish_disabled(writer),
    }
}

fn prompt_choice<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
    size: Size,
) -> Result<Option<Choice>, HarnessError> {
    let mut chosen = None;
    for attempt in 1..=MAX_ATTEMPTS {
        writer.write_prompt_line(&prompt_line(size, PROMPT))?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        match parse_selection(&answer) {
            Ok(choice) => {
                chosen = Some(choice);
                break;
            }
            Err(reason) => {
                if attempt < MAX_ATTEMPTS {
                    writer.write_line(&Line::of(Span::strong(reason, ERROR_COLOR)))?;
                }
            }
        }
    }
    Ok(chosen)
}

fn finish_disabled<W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
) -> Result<VoiceSelection, HarnessError> {
    writer.write_line(&Line::of(Span::strong(
        "Voice mode disabled. Operating in text-only mode.",
        NAME_COLOR,
    )))?;
    Ok(VoiceSelection::disabled())
}

fn finish_antigravity<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
    size: Size,
) -> Result<VoiceSelection, HarnessError> {
    let prompt = "Voice personality [Aoede, Puck, Charon, Fenrir, Kore] (default: Aoede) ▸ ";
    writer.write_prompt_line(&prompt_line(size, prompt))?;

    let mut answer = String::new();
    let voice_name = if reader.read_line(&mut answer)? > 0 && !answer.trim().is_empty() {
        let trimmed = answer.trim();
        // Capitalize nicely if recognized
        match trimmed.to_ascii_lowercase().as_str() {
            "puck" => "Puck".to_string(),
            "charon" => "Charon".to_string(),
            "fenrir" => "Fenrir".to_string(),
            "kore" => "Kore".to_string(),
            "aoede" => "Aoede".to_string(),
            other => other.to_string(),
        }
    } else {
        "Aoede".to_string()
    };

    writer.write_line(&Line::of(Span::strong(
        format!("Antigravity voice connected (voice: {voice_name})."),
        NAME_COLOR,
    )))?;

    Ok(VoiceSelection::antigravity(Some(voice_name)))
}

fn finish_openai<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
    size: Size,
) -> Result<VoiceSelection, HarnessError> {
    let prompt = "OpenAI voice [alloy, echo, fable, onyx, nova, shimmer] (default: alloy) ▸ ";
    writer.write_prompt_line(&prompt_line(size, prompt))?;

    let mut answer = String::new();
    let voice_name = if reader.read_line(&mut answer)? > 0 && !answer.trim().is_empty() {
        answer.trim().to_ascii_lowercase()
    } else {
        "alloy".to_string()
    };

    writer.write_line(&Line::of(Span::strong(
        format!("OpenAI voice selected (voice: {voice_name})."),
        NAME_COLOR,
    )))?;

    Ok(VoiceSelection::openai(Some(voice_name), None))
}

fn finish_groq<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    _reader: &mut R,
    _size: Size,
) -> Result<VoiceSelection, HarnessError> {
    writer.write_line(&Line::of(Span::strong(
        "Groq Whisper + Edge TTS selected.",
        NAME_COLOR,
    )))?;

    Ok(VoiceSelection::groq(None))
}

fn finish_custom<R: BufRead, W: Write, P: Pacer>(
    writer: &mut Typewriter<'_, W, P>,
    reader: &mut R,
    size: Size,
) -> Result<VoiceSelection, HarnessError> {
    // 1. Voice provider / backend
    writer.write_prompt_line(&prompt_line(
        size,
        "Custom voice provider/backend [default: antigravity] ▸ ",
    ))?;
    let mut ans_provider = String::new();
    let provider = if reader.read_line(&mut ans_provider)? > 0 && !ans_provider.trim().is_empty() {
        ans_provider.trim().to_string()
    } else {
        "antigravity".to_string()
    };

    // 2. Wake word
    writer.write_prompt_line(&prompt_line(size, "Wake word [default: Luna] ▸ "))?;
    let mut ans_wake = String::new();
    let wake_word = if reader.read_line(&mut ans_wake)? > 0 && !ans_wake.trim().is_empty() {
        ans_wake.trim().to_string()
    } else {
        "Luna".to_string()
    };

    // 3. Voice name
    writer.write_prompt_line(&prompt_line(size, "Voice name [default: Aoede] ▸ "))?;
    let mut ans_voice = String::new();
    let voice_name = if reader.read_line(&mut ans_voice)? > 0 && !ans_voice.trim().is_empty() {
        ans_voice.trim().to_string()
    } else {
        "Aoede".to_string()
    };

    // 4. Speech-to-text model override
    writer.write_prompt_line(&prompt_line(size, "STT model override [default: auto] ▸ "))?;
    let mut ans_stt = String::new();
    let stt_model = if reader.read_line(&mut ans_stt)? > 0 && !ans_stt.trim().is_empty() {
        ans_stt.trim().to_string()
    } else {
        String::new()
    };

    // 5. Text-to-speech model override
    writer.write_prompt_line(&prompt_line(size, "TTS model override [default: auto] ▸ "))?;
    let mut ans_tts = String::new();
    let tts_model = if reader.read_line(&mut ans_tts)? > 0 && !ans_tts.trim().is_empty() {
        ans_tts.trim().to_string()
    } else {
        String::new()
    };

    // 6. Silence threshold ms
    writer.write_prompt_line(&prompt_line(
        size,
        "Silence threshold in ms [default: 800] ▸ ",
    ))?;
    let mut ans_thresh = String::new();
    let silence_threshold_ms: u64 =
        if reader.read_line(&mut ans_thresh)? > 0 && !ans_thresh.trim().is_empty() {
            ans_thresh.trim().parse().unwrap_or(800)
        } else {
            800
        };

    // 7. Enable barge-in
    writer.write_prompt_line(&prompt_line(
        size,
        "Enable barge-in interrupt? [Y/n, default: Y] ▸ ",
    ))?;
    let mut ans_barge = String::new();
    let enable_barge_in = if reader.read_line(&mut ans_barge)? > 0 && !ans_barge.trim().is_empty() {
        !matches!(
            ans_barge.trim().to_ascii_lowercase().as_str(),
            "n" | "no" | "false"
        )
    } else {
        true
    };

    writer.write_line(&Line::of(Span::strong(
        format!(
            "Custom voice configured ({provider}, voice: {voice_name}, wake word: {wake_word})."
        ),
        NAME_COLOR,
    )))?;

    Ok(VoiceSelection {
        enabled: true,
        provider,
        wake_word,
        voice_name,
        stt_model,
        tts_model,
        base_url: None,
        api_key_env: None,
        silence_threshold_ms,
        enable_barge_in,
    })
}
