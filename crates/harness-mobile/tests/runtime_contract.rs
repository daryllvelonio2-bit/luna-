use std::sync::Arc;

use async_trait::async_trait;
use harness_mobile::{
    ActionCall, ActionContext, ActionError, ActionHandler, ActionId, ActionRegistry, RuntimeError,
    ScreenRuntime, StatePatch,
};
use harness_ui_spec::{Screen, SpecError};
use serde_json::{json, Value};

fn publisher(capabilities: &[&str]) -> Screen {
    let capabilities: Vec<&str> = capabilities.to_vec();
    let value = json!({
        "schema_version": 1,
        "id": "publisher",
        "title": "Publishing Dashboard",
        "capabilities": capabilities,
        "root": {
            "id": "root",
            "type": "column",
            "children": [
                { "id": "title", "type": "input", "bind": "post.title", "label": "Title", "value": "draft" },
                { "id": "publish", "type": "button", "label": "Publish", "action": "publish" }
            ]
        }
    });
    Screen::from_json(&value.to_string()).expect("screen")
}

struct RecordPublish;

#[async_trait]
impl ActionHandler for RecordPublish {
    async fn invoke(
        &self,
        call: &ActionCall,
        context: &ActionContext,
    ) -> Result<StatePatch, ActionError> {
        if call
            .args
            .get("fail")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(ActionError::failed(call.id.as_str(), "provider rejected"));
        }
        let mut patch = StatePatch::new();
        patch.set("last_action", json!(call.id.as_str()));
        patch.set(
            "last_title",
            context
                .state
                .get("post.title")
                .cloned()
                .unwrap_or(Value::Null),
        );
        Ok(patch)
    }
}

fn registry_with_publish() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    registry
        .register(
            ActionId::new("publish").expect("id"),
            Arc::new(RecordPublish),
        )
        .expect("register");
    registry
}

#[tokio::test]
async fn dispatch_updates_state_from_handler_patch() {
    let mut runtime = ScreenRuntime::new(publisher(&["publish"])).expect("runtime");
    assert_eq!(
        runtime.state_value("post.title"),
        Some(&Value::String("draft".into()))
    );

    runtime
        .set_input("post.title", json!("Launch day"))
        .expect("input");
    runtime
        .dispatch(&registry_with_publish(), "publish", json!({}))
        .await
        .expect("dispatch");

    assert_eq!(runtime.state_value("last_action"), Some(&json!("publish")));
    assert_eq!(
        runtime.state_value("last_title"),
        Some(&json!("Launch day"))
    );
}

#[tokio::test]
async fn rejects_state_writes_to_undeclared_binds() {
    let mut runtime = ScreenRuntime::new(publisher(&["publish"])).expect("runtime");
    let error = runtime
        .set_input("post.secret", json!("x"))
        .expect_err("unknown bind");
    assert!(matches!(error, RuntimeError::UnknownBind(bind) if bind == "post.secret"));
}

#[tokio::test]
async fn rejects_action_without_a_registered_handler() {
    let mut runtime = ScreenRuntime::new(publisher(&["publish"])).expect("runtime");
    let error = runtime
        .dispatch(&ActionRegistry::new(), "publish", json!({}))
        .await
        .expect_err("unknown action");
    assert!(
        matches!(error, RuntimeError::Action(ActionError::UnknownAction(action)) if action == "publish")
    );
}

#[tokio::test]
async fn registry_rejects_action_not_granted_by_screen() {
    let ungranted: Screen = serde_json::from_value(json!({
        "schema_version": 1,
        "id": "publisher",
        "title": "Publishing Dashboard",
        "root": { "id": "publish", "type": "button", "label": "Publish", "action": "publish" }
    }))
    .expect("screen");

    let call = ActionCall::new(ActionId::new("publish").expect("id"), json!({}));
    let error = registry_with_publish()
        .dispatch(&ungranted, &call, &Default::default())
        .await
        .expect_err("unauthorized");
    assert!(matches!(error, ActionError::UnauthorizedAction { action, .. } if action == "publish"));
}

#[test]
fn rejects_duplicate_action_registration() {
    let mut registry = registry_with_publish();
    let error = registry
        .register(
            ActionId::new("publish").expect("id"),
            Arc::new(RecordPublish),
        )
        .expect_err("duplicate");
    assert!(matches!(error, ActionError::DuplicateAction(action) if action == "publish"));
}

#[tokio::test]
async fn handler_failure_leaves_state_untouched() {
    let mut runtime = ScreenRuntime::new(publisher(&["publish"])).expect("runtime");
    let before = runtime.state().clone();

    let error = runtime
        .dispatch(&registry_with_publish(), "publish", json!({ "fail": true }))
        .await
        .expect_err("handler failed");

    assert!(
        matches!(error, RuntimeError::Action(ActionError::Failed { action, .. }) if action == "publish")
    );
    assert_eq!(runtime.state(), &before);
}

#[test]
fn runtime_rejects_unvalidated_spec() {
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

    let error = ScreenRuntime::new(invalid).expect_err("invalid");
    assert_eq!(error, SpecError::DuplicateNodeId { id: "same".into() });
}
