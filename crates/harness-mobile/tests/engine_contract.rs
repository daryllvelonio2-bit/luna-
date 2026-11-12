use harness_mobile::{EngineEvent, LunaEngine, MobileConfig, MobileError};
use harness_ui_spec::{Screen, SpecError};
use serde_json::json;

fn config(dir: &std::path::Path) -> MobileConfig {
    MobileConfig::new(dir, "openai", "gpt-4o-mini").with_api_key("test-key")
}

fn screen(id: &str, action: &str) -> Screen {
    let value = json!({
        "schema_version": 1,
        "id": id,
        "title": "Publishing Dashboard",
        "capabilities": [action],
        "root": {
            "id": "root",
            "type": "column",
            "children": [
                { "id": "title", "type": "input", "bind": "post.title", "label": "Title" },
                { "id": "publish", "type": "button", "label": "Publish", "action": action }
            ]
        }
    });
    Screen::from_json(&value.to_string()).expect("screen")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_boots_and_announces_generated_screens() {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = LunaEngine::new(config(dir.path())).await.expect("engine");

    assert!(engine.screens().is_empty());
    let mut events = engine.subscribe();

    let saved = engine
        .create_screen(screen("publisher", "publish"))
        .expect("save screen");

    assert_eq!(engine.screens().len(), 1);
    assert_eq!(engine.screen("publisher"), Some(saved));
    assert!(matches!(
        events.recv().await.expect("screen event"),
        EngineEvent::ScreenSaved(screen) if screen.id == "publisher"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screens_survive_an_engine_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    {
        let engine = LunaEngine::new(config(dir.path())).await.expect("engine");
        engine
            .create_screen(screen("publisher", "publish"))
            .expect("save screen");
    }

    let restarted = LunaEngine::new(config(dir.path())).await.expect("engine");
    assert_eq!(restarted.screens().len(), 1);
    assert_eq!(
        restarted.screen("publisher").map(|screen| screen.title),
        Some("Publishing Dashboard".into())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_rejects_invalid_screens() {
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = LunaEngine::new(config(dir.path())).await.expect("engine");

    let invalid: Screen = serde_json::from_value(json!({
        "schema_version": 1,
        "id": "publisher",
        "title": "Publishing Dashboard",
        "capabilities": ["publish"],
        "root": {
            "id": "root",
            "type": "column",
            "children": [
                { "id": "same", "type": "text", "text": "a" },
                { "id": "same", "type": "text", "text": "b" }
            ]
        }
    }))
    .expect("raw screen");

    let error = engine.create_screen(invalid).expect_err("invalid screen");
    assert!(matches!(
        error,
        MobileError::Spec(SpecError::DuplicateNodeId { id }) if id == "same"
    ));
    assert!(engine.screens().is_empty());
}
