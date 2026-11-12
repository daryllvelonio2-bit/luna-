use harness_ui_spec::{Screen, SpecError, SpecLoadError, SCHEMA_VERSION};
use serde_json::{json, Value};

fn valid_spec() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "id": "publisher",
        "title": "Publishing Dashboard",
        "capabilities": ["save_draft", "publish"],
        "root": {
            "id": "root",
            "type": "column",
            "children": [
                { "id": "heading", "type": "text", "text": "Compose", "role": "heading" },
                { "id": "title", "type": "input", "bind": "post.title", "label": "Title", "placeholder": "Post title" },
                {
                    "id": "channel",
                    "type": "select",
                    "bind": "post.channel",
                    "label": "Channel",
                    "options": [
                        { "value": "x", "label": "X" },
                        { "value": "instagram", "label": "Instagram" }
                    ],
                    "value": "x"
                },
                { "id": "auto", "type": "toggle", "bind": "post.auto", "label": "Auto-publish", "value": false },
                {
                    "id": "posts",
                    "type": "table",
                    "bind": "post.list",
                    "columns": [
                        { "key": "title", "label": "Title" },
                        { "key": "status", "label": "Status" }
                    ]
                },
                {
                    "id": "empty",
                    "type": "conditional",
                    "bind": "post.count",
                    "equals": 0,
                    "children": [
                        { "id": "empty_text", "type": "text", "text": "No posts yet" }
                    ],
                    "otherwise": [
                        {
                            "id": "rows",
                            "type": "repeat",
                            "bind": "post.rows",
                            "children": [
                                { "id": "row_title", "type": "text", "text": "row" }
                            ]
                        }
                    ]
                },
                { "id": "cover", "type": "image", "src": "asset://cover.png", "alt": "Cover" },
                { "id": "gap", "type": "spacer", "size": 12 },
                { "id": "save", "type": "button", "label": "Save", "action": "save_draft" },
                { "id": "publish", "type": "button", "label": "Publish", "action": "publish" }
            ]
        }
    })
}

fn load(value: &Value) -> Result<Screen, SpecLoadError> {
    Screen::from_json(&value.to_string())
}

fn assert_invalid(value: &Value, expected: SpecError) {
    match load(value) {
        Err(SpecLoadError::Invalid(error)) => assert_eq!(error, expected),
        other => panic!("expected invalid spec, got {other:?}"),
    }
}

#[test]
fn valid_spec_round_trips() {
    let original = load(&valid_spec()).expect("valid spec");
    assert_eq!(original.schema_version, SCHEMA_VERSION);

    let encoded = original.to_json().expect("serialize");
    let decoded = Screen::from_json(&encoded).expect("reload");
    assert_eq!(original, decoded);
}

#[test]
fn walks_the_conditional_otherwise_branch() {
    let screen = load(&valid_spec()).expect("valid spec");
    let bind = "post.rows";
    assert!(screen.declared_binds().contains(&bind));
}

#[test]
fn rejects_undeclared_action() {
    let mut value = valid_spec();
    value["root"]["children"][9]["action"] = json!("delete_everything");
    assert_invalid(
        &value,
        SpecError::UndeclaredAction {
            screen: "publisher".into(),
            node: "publish".into(),
            action: "delete_everything".into(),
        },
    );
}

#[test]
fn rejects_duplicate_node_ids() {
    let mut value = valid_spec();
    value["root"]["children"][1]["id"] = json!("heading");
    assert_invalid(
        &value,
        SpecError::DuplicateNodeId {
            id: "heading".into(),
        },
    );
}

#[test]
fn rejects_duplicate_state_bindings() {
    let mut value = valid_spec();
    value["root"]["children"][3]["bind"] = json!("post.title");
    assert_invalid(
        &value,
        SpecError::DuplicateBind {
            bind: "post.title".into(),
        },
    );
}

#[test]
fn rejects_duplicate_select_values() {
    let mut value = valid_spec();
    value["root"]["children"][2]["options"][1]["value"] = json!("x");
    assert_invalid(
        &value,
        SpecError::DuplicateSelectValue {
            id: "channel".into(),
            value: "x".into(),
        },
    );
}

#[test]
fn rejects_empty_select_options() {
    let mut value = valid_spec();
    value["root"]["children"][2]["options"] = json!([]);
    assert_invalid(
        &value,
        SpecError::EmptySelectOptions {
            id: "channel".into(),
        },
    );
}

#[test]
fn rejects_duplicate_capabilities() {
    let mut value = valid_spec();
    value["capabilities"] = json!(["publish", "publish"]);
    assert_invalid(
        &value,
        SpecError::DuplicateCapability {
            capability: "publish".into(),
        },
    );
}

#[test]
fn rejects_unsupported_schema_version() {
    let mut value = valid_spec();
    value["schema_version"] = json!(999);
    assert_invalid(
        &value,
        SpecError::UnsupportedSchemaVersion {
            found: 999,
            supported: SCHEMA_VERSION,
        },
    );
}

#[test]
fn rejects_unknown_node_type() {
    let mut value = valid_spec();
    value["root"]["children"][1]["type"] = json!("hologram");
    assert!(matches!(load(&value), Err(SpecLoadError::Json(_))));
}

#[test]
fn rejects_empty_text_input_binding() {
    let mut value = valid_spec();
    value["root"]["children"][1]["bind"] = json!("   ");
    assert_invalid(&value, SpecError::EmptyBind { id: "title".into() });
}
