use harness_core::*;
use harness_notes::*;
use serde_json::json;
use std::sync::Arc;

/// An `update_note` approval must show what the overwrite removes. A preview that carries only
/// the new body reads as a wall of additions, so the deleted lines are invisible in the
/// approval view and the user cannot see what changed.
#[tokio::test]
async fn update_note_preview_diffs_the_stored_body_against_the_new_one() {
    let engine = Arc::new(FileNotesEngine::in_memory().unwrap());
    let note = engine
        .save_note("Retired runbook", "alpha\nbeta\ngamma\n", &[])
        .await
        .unwrap();
    let tool = UpdateNoteTool::new(engine.clone());

    let preview = tool
        .preview(&json!({ "id": note.id, "content": "alpha\n" }))
        .await
        .unwrap();

    assert_eq!(preview.target.as_deref(), Some(note.id.as_str()));
    let diff = preview
        .diff
        .expect("an overwrite preview must carry a diff");
    assert!(diff.contains("-beta"), "{diff}");
    assert!(diff.contains("-gamma"), "{diff}");

    // A title-only update changes no body, so there is nothing to diff.
    let retitle = tool
        .preview(&json!({ "id": note.id, "title": "New name" }))
        .await
        .unwrap();
    assert!(retitle.diff.is_none(), "{retitle:?}");
}
