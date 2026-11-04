//! Prompt-side image inlining.
//!
//! The model cannot open files on the user's machine — it asks a tool to do that. When the prompt
//! carries the path of an image, the pixels must be embedded in the prompt itself, as a data URL
//! the provider layer lifts into a vision content part. Without this, a model that would happily
//! describe an image it was given is reduced to running OCR on a path it cannot resolve to pixels.
//!
//! Path inlining is deliberately conservative: only files that exist, end in a known image
//! extension, and fit a size budget are inlined. Anything else is left as text, so a prompt that
//! mentions `diagram.png` in prose is not silently replaced by megabytes of base64.

use std::path::Path;

use base64::Engine;

/// Strip ` saved to <path>` inside `[image...]` markers: the image pixels are already attached to
/// the turn as a data URL, so sending the local temp file path is redundant and invites the model
/// to read or mention a path it does not need.
pub fn strip_image_saved_paths(text: &str) -> String {
    if !text.contains("[image") || !text.contains(" saved to ") {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("[image") {
        out.push_str(&rest[..start]);
        let marker_rest = &rest[start..];
        if let Some(end) = marker_rest.find(']') {
            let marker = &marker_rest[..=end];
            if let Some(saved_idx) = marker.find(" saved to ") {
                out.push_str(&marker[..saved_idx]);
                out.push(']');
            } else {
                out.push_str(marker);
            }
            rest = &marker_rest[end + 1..];
        } else {
            out.push_str(&marker_rest[.."[image".len()]);
            rest = &marker_rest["[image".len()..];
        }
    }
    out.push_str(rest);
    out
}

/// Drop leading `[image...]` attachment markers so a pasted screenshot ahead of the command does
/// not stop `/worker ...` from being recognized as a command. The markers stay in the text that is
/// sent and displayed; this is only for command detection.
pub fn strip_leading_image_markers(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(after) = rest.strip_prefix("[image") {
        let Some(end) = after.find(']') else {
            break;
        };
        rest = after[end + 1..].trim_start();
    }
    rest
}

/// Remove `[image...]` attachment markers from `text`, leaving the rest of the prompt intact.
/// Used when the user cancels a draft: the pixels are dropped from `pending_images`, so the marker
/// must not linger as a promise of an attachment that is no longer there.
pub fn remove_image_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[image") {
        out.push_str(&rest[..start]);
        let marker_rest = &rest[start..];
        match marker_rest.find(']') {
            Some(end) => {
                rest = &marker_rest[end + 1..];
                // The attach path puts one space on each side; drop the second one too.
                if out.ends_with(' ') && rest.starts_with(' ') {
                    rest = &rest[1..];
                } else if out.is_empty() {
                    rest = rest.trim_start();
                }
            }
            None => {
                out.push_str(marker_rest);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.trim_end().to_string()
}

/// Inline every image path in `prompt` as a data URL, on its own line.
///
/// Each path is replaced in place; the resulting line is what the provider layer turns into an
/// `image_url` (OpenAI route) or `inlineData` (Gemini route) content part.
pub fn inline_image_paths(prompt: &str) -> String {
    let cleaned = strip_image_saved_paths(prompt);
    let prompt = cleaned.as_str();
    if !prompt.contains('.') {
        return prompt.to_string();
    }

    let mut out = String::with_capacity(prompt.len());
    for line in prompt.lines_including_final() {
        let mut replaced = false;
        let inlined = replace_tokens_with_data_urls(line, &mut |token| {
            let data_url = data_url_for_file(token)?;
            replaced = true;
            Some(data_url)
        });
        out.push_str(&inlined);
        out.push('\n');
    }

    // `lines_including_final` already preserves the trailing-newline shape; strip the extra
    // newline that the loop above adds after a final line without one.
    if !prompt.ends_with('\n') {
        out.strip_suffix('\n').map(str::to_string).unwrap_or(out)
    } else {
        out
    }
}

/// Replace every whitespace-delimited token for which `visit` returns a replacement, keeping the
/// separators that surrounded it.
fn replace_tokens_with_data_urls(
    line: &str,
    visit: &mut dyn FnMut(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        // Split off the next token and the separator that follows it.
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (token, after) = rest.split_at(token_end);
        let sep_len = after.len() - after.trim_start().len();
        let (sep, tail) = after.split_at(sep_len);

        if let Some(replacement) = visit(token) {
            out.push_str(&replacement);
        } else {
            out.push_str(token);
        }
        out.push_str(sep);
        rest = tail;
    }
    out
}

/// The data URL for `token`, when it names an image file we should inline.
fn data_url_for_file(token: &str) -> Option<String> {
    // Strip shell residue: opening brackets/quotes from the front, closing punctuation from the
    // back, so `"~/x.png"` and `./x.png,` both work. The sets differ — a leading `.` belongs to
    // `./relative.png` and must survive, while a trailing `.` after a quote (`"x.png".`) must go.
    let cleaned = token
        .trim_start_matches(|c: char| "\"'`([{".contains(c))
        .trim_end_matches(|c: char| "\"'`)]}.,;:!?".contains(c))
        .trim();

    // Only explicit extensions qualify: a path like `notes.txt` or `main.rs` never becomes an
    // image, and probing every dotted token would read arbitrary files on every turn.
    let ext = Path::new(cleaned)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    };

    let path = resolve_path(cleaned)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_INLINE_IMAGE_BYTES {
        // A warning, not a debug line: the user asked for this image and the model did not get it,
        // so "why did it not see my screenshot" has an answer in the log by default.
        tracing::warn!(
            path = %path.display(),
            size = meta.len(),
            budget = MAX_INLINE_IMAGE_BYTES,
            "image is larger than the inline budget; sending the prompt without it"
        );
        return None;
    }

    let key = InlineKey {
        path: path.clone(),
        modified: meta.modified().ok(),
        len: meta.len(),
    };
    if let Some(data_url) = cached_inline(&key) {
        return Some(data_url);
    }

    let bytes = std::fs::read(&path).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{};base64,{}", mime, encoded);
    remember_inline(key, &data_url);
    Some(data_url)
}

/// What an inlined image is keyed by: the file it came from, at the size and modification time it
/// had when it was read. A file that changed is a different key, so a stale copy is never sent.
#[derive(PartialEq, Eq, Hash)]
struct InlineKey {
    path: std::path::PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
}

/// The images inlined so far this process.
///
/// A prompt that names the same screenshot on every turn re-read and re-encoded it every time, and
/// the encoded form is megabytes held in a `String` on each of those turns. Entries are dropped once
/// [`MAX_CACHED_INLINES`] is reached, so a long-lived process cannot accumulate every image it has
/// ever been shown.
static INLINED: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<InlineKey, String>>,
> = std::sync::OnceLock::new();

/// How many inlined images are kept before the cache is dropped wholesale.
///
/// Sixteen screenshots at the few hundred kilobytes deflate produces is a few megabytes held; each
/// one is bounded by [`MAX_INLINE_IMAGE_BYTES`], so the cap is what keeps the ceiling real.
const MAX_CACHED_INLINES: usize = 16;

fn cached_inline(key: &InlineKey) -> Option<String> {
    let cache = INLINED.get_or_init(Default::default);
    let cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.get(key).cloned()
}

fn remember_inline(key: InlineKey, data_url: &str) {
    let cache = INLINED.get_or_init(Default::default);
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= MAX_CACHED_INLINES {
        cache.clear();
    }
    cache.insert(key, data_url.to_string());
}

/// Files above this are left as text. A generated image is 1–4 MB; a 20 MB scan is not an image
/// the model needs at full fidelity, and embedding it would dominate the prompt.
const MAX_INLINE_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Expand `~` so a prompt that says `~/Downloads/x.png` finds the file.
fn resolve_path(token: &str) -> Option<std::path::PathBuf> {
    if let Some(stripped) = token
        .strip_prefix("~/")
        .or_else(|| token.strip_prefix("~\\"))
    {
        return Some(harness_core::config::home_dir().join(stripped));
    }
    if token == "~" {
        return Some(harness_core::config::home_dir());
    }
    Some(std::path::PathBuf::from(token))
}

/// [`str::lines`], keeping a final line that has no trailing newline (and a final newline itself).
trait LinesIncludingFinal {
    fn lines_including_final(&self) -> Vec<&str>;
}

impl LinesIncludingFinal for str {
    fn lines_including_final(&self) -> Vec<&str> {
        let mut lines: Vec<&str> = self.split('\n').collect();
        // `split('\n')` adds a phantom empty last element when the text ends with a newline.
        if lines.len() > 1 && lines.last() == Some(&"") {
            lines.pop();
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_without_dots_is_untouched() {
        let prompt = "describe this screenshot for me";
        assert_eq!(inline_image_paths(prompt), prompt);
    }

    #[test]
    fn prose_mentioning_a_dotted_word_is_untouched() {
        let prompt = "what does main.rs do";
        assert_eq!(inline_image_paths(prompt), prompt);
    }

    #[test]
    fn an_existing_png_path_becomes_a_data_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n fake").unwrap();

        let out = inline_image_paths(&format!("what is in {} exactly", path.display()));
        assert!(
            out.contains("data:image/png;base64,"),
            "the path should have been inlined: {out}"
        );
        assert!(
            !out.contains("shot.png"),
            "the path text should be gone: {out}"
        );
    }

    #[test]
    fn a_missing_file_stays_text() {
        let prompt = "look at /nope/does/not/exist.png please";
        assert_eq!(inline_image_paths(prompt), prompt);
    }

    #[test]
    fn quotes_and_punctuation_around_the_path_are_survived() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("img.jpg");
        std::fs::write(&path, b"jpeg").unwrap();

        let out = inline_image_paths(&format!("read \"{}\".", path.display()));
        assert!(out.contains("data:image/jpeg;base64,"));
    }

    #[test]
    fn an_oversize_image_is_left_as_text() {
        // A file whose metadata reports more than the budget without writing 8 MB to disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_INLINE_IMAGE_BYTES + 1).unwrap();

        let prompt = format!("look at {}", path.display());
        assert_eq!(inline_image_paths(&prompt), prompt);
    }

    #[test]
    fn a_tilde_path_resolves_against_the_home_directory() {
        // Does not assert on the result (home may not hold the file) — only that it does not
        // panic and leaves non-existent tilde paths alone.
        let prompt = "check ~/definitely/not/here.png now";
        assert_eq!(inline_image_paths(prompt), prompt);
    }

    #[test]
    fn removing_image_markers_leaves_the_prompt_text() {
        assert_eq!(remove_image_markers("[image: 10x10] hello"), "hello");
        assert_eq!(
            remove_image_markers("hello [image: 10x10] world"),
            "hello world"
        );
        assert_eq!(remove_image_markers("hello [image: 10x10]"), "hello");
        assert_eq!(remove_image_markers("[image: 1x1] [image: 2x2] x"), "x");
        assert_eq!(remove_image_markers("no markers"), "no markers");
    }

    #[test]
    fn leading_image_markers_are_skipped_for_command_detection() {
        assert_eq!(
            strip_leading_image_markers("[image: 1920x1080] /carbon fix this"),
            "/carbon fix this"
        );
        assert_eq!(
            strip_leading_image_markers("  [image] [image: 10x10 saved to /a.png]  /worker"),
            "/worker"
        );
        // A marker after the text is not leading and must stay part of the text.
        assert_eq!(
            strip_leading_image_markers("/carbon [image: 10x10] fix"),
            "/carbon [image: 10x10] fix"
        );
        assert_eq!(strip_leading_image_markers("plain prompt"), "plain prompt");
    }

    #[test]
    fn strip_image_saved_paths_removes_file_paths_from_image_markers() {
        let input = "[image: 1920x1080 saved to /tmp/screenshot_123.png] what is in this?";
        assert_eq!(
            strip_image_saved_paths(input),
            "[image: 1920x1080] what is in this?"
        );

        let input_no_dims = "[image saved to /tmp/shot.png]";
        assert_eq!(strip_image_saved_paths(input_no_dims), "[image]");

        let multiple = "[image: 800x600 saved to /a.png] and [image: 1024x768 saved to /b.png]";
        assert_eq!(
            strip_image_saved_paths(multiple),
            "[image: 800x600] and [image: 1024x768]"
        );

        let clean = "[image: 1920x1080] looks fine";
        assert_eq!(strip_image_saved_paths(clean), clean);
    }

    #[test]
    fn inline_image_paths_strips_saved_paths_instead_of_inlining_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n fake").unwrap();

        let prompt = format!(
            "[image: 1920x1080 saved to {}] describe this",
            path.display()
        );
        let out = inline_image_paths(&prompt);
        assert_eq!(out, "[image: 1920x1080] describe this");
    }
}
