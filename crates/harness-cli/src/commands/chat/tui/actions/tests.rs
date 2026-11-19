//! tests

use super::*;

#[cfg(test)]
mod tests {
    use super::{compact_scope_arg, model_scope_arg};

    #[test]
    fn default_model_scope_flags_are_recognised() {
        assert_eq!(model_scope_arg(Some("global")), Some(true));
        assert_eq!(model_scope_arg(Some("--global")), Some(true));
        assert_eq!(model_scope_arg(Some("session")), Some(false));
        assert_eq!(model_scope_arg(Some("--session")), Some(false));
    }

    #[test]
    fn a_bare_default_model_asks_for_scope() {
        assert_eq!(model_scope_arg(None), None);
        assert_eq!(model_scope_arg(Some("anything else")), None);
    }

    #[test]
    fn compact_model_scope_defaults_to_session_for_any_other_argument() {
        assert_eq!(compact_scope_arg(None), None);
        assert_eq!(compact_scope_arg(Some("global")), Some(true));
        assert_eq!(compact_scope_arg(Some("whatever")), Some(false));
    }

    #[test]
    fn transient_queue_notices_are_properly_detected() {
        use super::is_transient_queue_notice;
        assert!(is_transient_queue_notice(
            "⏸ Queued (1 waiting) — sent when the turn in flight finishes"
        ));
        assert!(is_transient_queue_notice(
            "↳ Held until the running tool finishes, then sent"
        ));
        assert!(is_transient_queue_notice(
            "↳ Redirecting — stopping this reply to take your message"
        ));
        assert!(!is_transient_queue_notice(
            "✓ Deleted session sa889751a and started fresh session s5227ec12"
        ));
        assert!(!is_transient_queue_notice(
            "Build · Nemotron 3.5 Super VL 120B A12b · 0.0s"
        ));
    }

    #[test]
    fn compaction_notice_expires_after_ttl() {
        use super::SystemNotice;
        let notice = SystemNotice {
            text: "✓ Compacted 1 turns (10 messages) into 1 summary".to_string(),
            created_at: std::time::Instant::now() - std::time::Duration::from_secs(6),
        };
        assert!(notice.is_stale(std::time::Instant::now(), true, false));

        let fresh_notice = SystemNotice::new("✓ Compacted 1 turns (10 messages) into 1 summary");
        assert!(!fresh_notice.is_stale(std::time::Instant::now(), true, false));
    }

    #[test]
    fn subagent_ui_entry_state_and_cancellation() {
        use super::SubagentUiEntry;
        let mut entry = SubagentUiEntry {
            id: "sub_100".to_string(),
            agent: "Explore".to_string(),
            title: "Inspect directory".to_string(),
            status: "running".to_string(),
            activity: "starting".to_string(),
            elapsed_secs: 5,
            turns: Vec::new(),
            log_tail: Vec::new(),
        };
        assert_eq!(entry.status, "running");
        entry.status = "cancelled".to_string();
        entry.activity = "cancelled by user".to_string();
        assert_eq!(entry.status, "cancelled");
        assert_eq!(entry.activity, "cancelled by user");
    }

    #[test]
    fn worker_turns_from_history_excludes_parent_fork_history() {
        use super::worker_turns_from_history;
        use crate::commands::chat::render::TurnRecord;

        let turn = |q: &str, r: &str| TurnRecord {
            user_query: q.to_string(),
            reasoning: None,
            tools: Vec::new(),
            tool_items: Vec::new(),
            duration: std::time::Duration::ZERO,
            reply: r.to_string(),
        };

        let parent_only = vec![turn("hello", "hi"), turn("check status", "ok")];
        // While running before the worker persists its own turn, parent turns must not appear.
        assert!(worker_turns_from_history(parent_only.clone(), "fix bug", "running").is_empty());

        let with_worker = vec![
            turn("hello", "hi"),
            turn("check status", "ok"),
            turn("GOAL: fix bug", "fixed"),
        ];
        let sliced = worker_turns_from_history(with_worker, "fix bug", "running");
        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].user_query, "GOAL: fix bug");
        assert_eq!(sliced[0].reply, "fixed");
    }

    #[test]
    fn build_approval_question_prompt_includes_diff_and_question() {
        let req = harness_core::ApprovalRequest {
            tool: "patch".to_string(),
            call_id: "c1".to_string(),
            target: Some("src/main.rs".to_string()),
            diff: Some(
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            ),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
        };
        let prompt = super::build_approval_question_prompt(&req, "Is +new safe?");
        assert!(prompt.contains("Pending tool call: patch"));
        assert!(prompt.contains("Target: src/main.rs"));
        assert!(prompt.contains("+new"));
        assert!(prompt.contains("Is +new safe?"));
    }

    #[tokio::test]
    async fn priority_scope_modal_actions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cred_path = temp_dir.path().join("credentials.json");
        let cred_mgr = harness_providers::CredentialManager::new(Some(cred_path)).unwrap();
        cred_mgr
            .add_credential("antigravity", "account-1", "token-1", true)
            .await
            .unwrap();
        cred_mgr
            .add_credential("antigravity", "account-2", "token-2", false)
            .await
            .unwrap();

        // 1. Session scope overrides for session s1
        cred_mgr.set_session_priority_credential("s1", "antigravity", "account-2");
        assert_eq!(
            cred_mgr.get_session_priority_credential("s1", "antigravity"),
            Some("account-2".to_string())
        );
        assert_eq!(
            cred_mgr.get_session_priority_credential("s2", "antigravity"),
            None
        );

        // 2. Global scope clears session override for s1 and sets global priority
        cred_mgr
            .set_priority_credential("antigravity", "account-2")
            .await
            .unwrap();
        cred_mgr.clear_session_priority_credential("s1", "antigravity");
        assert_eq!(
            cred_mgr.get_session_priority_credential("s1", "antigravity"),
            None
        );
    }
}
