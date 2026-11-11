use serde_json::json;

/// Inject bash and read tools into payload for OpenCode free-tier requests if missing.
///
/// OpenCode Zen's free tier verifies that requests originate from within OpenCode by
/// inspecting the payload tools. Both 'bash' and 'read' tool definitions must be present.
pub fn ensure_compat_tools(body: &mut serde_json::Value) {
    if !body.is_object() {
        return;
    }

    let tools_val = body.get_mut("tools");
    let mut tools_list = match tools_val {
        Some(serde_json::Value::Array(ref mut arr)) => std::mem::take(arr),
        _ => Vec::new(),
    };

    let mut has_bash = false;
    let mut has_read = false;

    for t in &tools_list {
        if let Some(name) = t.get("name").and_then(|n| n.as_str()) {
            if name == "bash" {
                has_bash = true;
            }
            if name == "read" {
                has_read = true;
            }
        }
        if let Some(func) = t.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                if name == "bash" {
                    has_bash = true;
                }
                if name == "read" {
                    has_read = true;
                }
            }
        }
    }

    if !has_bash {
        tools_list.push(json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Executes a given bash command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        }));
    }

    if !has_read {
        tools_list.push(json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file from local filesystem",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filePath": { "type": "string" }
                    },
                    "required": ["filePath"]
                }
            }
        }));
    }

    body["tools"] = serde_json::Value::Array(tools_list);
}

pub fn normalize_tool_arguments(input: &mut serde_json::Value) {
    if let serde_json::Value::Object(ref mut map) = input {
        // Path normalization
        if let Some(v) = map.get("filePath").cloned() {
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("filepath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("path").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("filepath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("filepath").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("file".to_string()).or_insert(v);
        } else if let Some(v) = map.get("file").cloned() {
            map.entry("filePath".to_string())
                .or_insert_with(|| v.clone());
            map.entry("path".to_string()).or_insert_with(|| v.clone());
            map.entry("filepath".to_string()).or_insert(v);
        }

        // Old string normalization
        if let Some(v) = map.get("oldString").cloned() {
            map.entry("old_string".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old".to_string()).or_insert(v);
        } else if let Some(v) = map.get("old_string").cloned() {
            map.entry("oldString".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("old".to_string()).or_insert(v);
        }

        // New string normalization
        if let Some(v) = map.get("newString").cloned() {
            map.entry("new_string".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new".to_string()).or_insert(v);
        } else if let Some(v) = map.get("new_string").cloned() {
            map.entry("newString".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new_str".to_string())
                .or_insert_with(|| v.clone());
            map.entry("new".to_string()).or_insert(v);
        }

        // Replace all normalization
        if let Some(v) = map.get("replaceAll").cloned() {
            map.entry("replace_all".to_string()).or_insert(v);
        } else if let Some(v) = map.get("replace_all").cloned() {
            map.entry("replaceAll".to_string()).or_insert(v);
        }

        // Patch / Diff normalization
        if let Some(v) = map.get("patchText").cloned() {
            map.entry("diff".to_string()).or_insert_with(|| v.clone());
            map.entry("patch".to_string()).or_insert(v);
        } else if let Some(v) = map.get("diff").cloned() {
            map.entry("patchText".to_string())
                .or_insert_with(|| v.clone());
            map.entry("patch".to_string()).or_insert(v);
        } else if let Some(v) = map.get("patch").cloned() {
            map.entry("patchText".to_string())
                .or_insert_with(|| v.clone());
            map.entry("diff".to_string()).or_insert(v);
        }
    }
}
