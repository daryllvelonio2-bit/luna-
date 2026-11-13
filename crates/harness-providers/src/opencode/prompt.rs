use harness_core::types::{Message, Role};
use serde_json::json;

/// A prompt in the shape the OpenCode routes take it: the conversation as text, and the images
/// pasted into the newest user turn as attachments beside it.
pub struct OpencodePrompt {
    pub text: String,
    pub files: Vec<serde_json::Value>,
}

/// Formats the conversation history for the OpenCode routes.
///
/// A `data:image/...` URL inside the text is not an image to OpenCode — it is hundreds of
/// kilobytes of base64 the model reads as prose, and, depending on how the server splits the
/// message, it can push the instruction out of what the model actually sees. OpenCode has a
/// dedicated attachment field for pixels (`files`), so the URLs are lifted out of the text and
/// handed over there instead.
///
/// Only the newest user turn's images are attached. Images from earlier turns stay in the
/// transcript as the `[image: WxH]` marker the CLI wrote beside them: re-uploading every pasted
/// screenshot on every request would grow each prompt by hundreds of kilobytes and buy the model
/// a second look it cannot act on anymore.
pub fn format_opencode_prompt(messages: &[Message]) -> OpencodePrompt {
    // The turn's instruction is the newest user message, wherever the tool loop has left the
    // tail of the vector. Images in anything older are history; only this turn's pixels are sent.
    let attach_to = messages.iter().rposition(|msg| msg.role == Role::User);

    let mut parts = Vec::new();
    let mut files = Vec::new();
    for (index, msg) in messages.iter().enumerate() {
        let role_label = match msg.role {
            Role::System => "System Instruction",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => {
                if msg
                    .tool_result
                    .as_ref()
                    .is_some_and(|result| result.is_error)
                {
                    "Tool Error"
                } else {
                    "Tool Output"
                }
            }
        };
        let mut text = String::new();
        if let Some(ref content) = msg.content {
            let (without_images, images) = split_data_urls(content);
            let trimmed = without_images.trim();
            if !trimmed.is_empty() {
                text.push_str(trimmed);
            }
            if Some(index) == attach_to {
                for url in images {
                    files.push(image_attachment(&url, files.len() + 1));
                }
            }
        }
        if let Some(ref result) = msg.tool_result {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "[tool_call_id: {}] {}",
                result.call_id, result.content
            ));
        }
        if !msg.tool_calls.is_empty() {
            for call in &msg.tool_calls {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("[Tool Call: {}({})]", call.name, call.arguments));
            }
        }
        if !text.is_empty() {
            parts.push(format!("[{}]\n{}", role_label, text));
        }
    }
    let text = if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n")
    };
    OpencodePrompt { text, files }
}

/// The text half of [`format_opencode_prompt`], for callers that cannot forward attachments.
pub fn format_messages_for_opencode(messages: &[Message]) -> String {
    format_opencode_prompt(messages).text
}

/// One `data:image/...;base64,...` URL in the prompt, in the shape OpenCode's `files` field
/// takes: the URL itself is the URI, and the name only exists so a saved attachment has an
/// extension that matches its MIME type.
fn image_attachment(url: &str, ordinal: usize) -> serde_json::Value {
    let subtype = url
        .trim_start_matches("data:image/")
        .split([';', ','])
        .next()
        .unwrap_or("png");
    json!({
        "uri": url,
        "name": format!("pasted-image-{}.{}", ordinal, subtype),
        "description": "image pasted into the chat prompt",
    })
}

/// Split every real inline image URL out of `text`, returning the remaining prose and the URLs.
///
/// A URL runs to the next whitespace and only counts when it carries `;base64,` — the same rule
/// the OpenAI-compatible serializer uses, so prose that mentions `data:image/png` in passing is
/// left alone.
fn split_data_urls(text: &str) -> (String, Vec<String>) {
    if !text.contains("data:image/") {
        return (text.to_string(), Vec::new());
    }

    let mut cleaned = String::with_capacity(text.len());
    let mut urls = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("data:image/") {
        let after = &rest[start..];
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        if !after[..end].contains(";base64,") {
            cleaned.push_str(&rest[..start + "data:image/".len()]);
            rest = &after["data:image/".len()..];
            continue;
        }
        cleaned.push_str(&rest[..start]);
        urls.push(after[..end].to_string());
        rest = &after[end..];
    }
    cleaned.push_str(rest);
    (cleaned, urls)
}
