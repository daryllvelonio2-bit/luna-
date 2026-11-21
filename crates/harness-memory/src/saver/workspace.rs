//! Which directories a pass may derive a note's project from.
//!
//! The session was launched somewhere, and that directory is the strongest statement of "which
//! project is this". A conversation about a repository the user opened from elsewhere is the
//! common second case: the absolute paths in tool calls and tool output name the tree even when
//! the process never changed into it. Both are candidates; the notes engine decides which one
//! actually sits inside a source tree.

use std::path::PathBuf;

use harness_core::{Message, Role};

/// How many candidate roots a pass carries. Enough for the working directory plus the handful of
/// trees a real session touches, small enough that project detection stays a filesystem walk of
/// a few directories per note.
pub(super) const MAX_CANDIDATES: usize = 16;

/// The roots to try, most authoritative first: the configured ones, then paths the conversation
/// mentioned, in the order they appeared.
pub(super) fn candidates(configured: &[PathBuf], tail: &[Message]) -> Vec<PathBuf> {
    let mut roots = configured.to_vec();
    roots.truncate(MAX_CANDIDATES);
    for message in tail {
        if matches!(message.role, Role::System) {
            continue;
        }
        if let Some(content) = message.content.as_deref() {
            collect_paths(content, &mut roots);
        }
        for call in &message.tool_calls {
            collect_paths(&call.arguments.to_string(), &mut roots);
        }
        if let Some(result) = &message.tool_result {
            collect_paths(&result.content, &mut roots);
        }
    }
    roots
}

fn collect_paths(text: &str, roots: &mut Vec<PathBuf>) {
    for token in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '=' | '<' | '>'
            )
    }) {
        if roots.len() >= MAX_CANDIDATES {
            return;
        }
        let token = token.trim_end_matches(['.', ':', ',', ';']);
        if token.len() < 2 || !token.starts_with('/') || token.contains("://") {
            continue;
        }
        let path = PathBuf::from(token);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::Message;

    #[test]
    fn configured_roots_come_first_and_stay_deduped() {
        let configured = vec![PathBuf::from("/work/app")];
        let tail = vec![Message::user(
            "I am in /work/app and also /work/app/src/main.rs",
        )];
        let roots = candidates(&configured, &tail);
        assert_eq!(roots[0], PathBuf::from("/work/app"));
        assert_eq!(
            roots
                .iter()
                .filter(|root| *root == &PathBuf::from("/work/app"))
                .count(),
            1
        );
        assert!(roots.contains(&PathBuf::from("/work/app/src/main.rs")));
    }

    #[test]
    fn tool_arguments_contribute_paths() {
        let call = harness_core::ToolCall {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/repos/luna/src/main.rs"}),
            signature: None,
        };
        let tail = vec![Message::assistant_with_tools(vec![call])];
        let roots = candidates(&[], &tail);
        assert!(roots.contains(&PathBuf::from("/repos/luna/src/main.rs")));
    }
}
