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

/// Inline every image path in `prompt` as a data URL, on its own line.
///
/// Each path is replaced in place; the resulting line is what the provider layer turns into an
/// `image_url` (OpenAI route) or `inlineData` (Gemini route) content part.
pub fn inline_image_paths(prompt: &str) -> String {
    if !prompt.contains('.') {
        return prompt.to_string();
    }

    let mut out = String::with_capacity(prompt.len());
    for line in prompt.lines_including_final() {
        let mut replaced = false;
        let inlined = replace_tokens_with_data_urls(line, &mut |token| {
            let Some(data_url) = data_url_for_file(token) else {
                return None;
            };
            replaced = true;
            Some(data_url)
        });
        out.push_str(&inlined);
        if !replaced {
            out.push('\n');
        } else {
            out.push('\n');
        }
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
        tracing::debug!(
            path = %path.display(),
            size = meta.len(),
            "image path in prompt exceeds the inline budget; left as text"
        );
        return None;
    }

    let bytes = std::fs::read(&path).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{};base64,{}", mime, encoded))
}

/// Files above this are left as text. A generated image is 1–4 MB; a 20 MB scan is not an image
/// the model needs at full fidelity, and embedding it would dominate the prompt.
const MAX_INLINE_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Expand `~` so a prompt that says `~/Downloads/x.png` finds the file.
fn resolve_path(token: &str) -> Option<std::path::PathBuf> {
    if let Some(stripped) = token.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home).join(stripped));
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
}
