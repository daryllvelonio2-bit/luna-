//! YAML front matter: split, parse, and field access.
//!
//! Records are Markdown files whose first line is `---`, followed by a YAML
//! mapping, a closing `---`, then the untouched body. Nothing here rewrites
//! the body; the file is the canonical byte store.

use serde_yaml::Value;

/// Split a Markdown document into `(yaml, body)`.
///
/// Returns `None` when the document has no front matter block. A malformed
/// block (opening `---` with no closing `---`) is also `None`: callers decide
/// whether that is a warning or an error.
pub fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\r\n").or_else(|| text.strip_prefix("---\n"))?;
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((yaml, body.trim_start_matches(['\r', '\n'])));
        }
        offset += line.len();
    }
    None
}

/// Render a document with a YAML mapping and a body.
pub fn render(yaml: &str, body: &str) -> String {
    let yaml = yaml.trim_end_matches('\n');
    let body = body.trim_start_matches(['\r', '\n']);
    if body.is_empty() {
        format!("---\n{yaml}\n---\n")
    } else if body.ends_with('\n') {
        format!("---\n{yaml}\n---\n{body}")
    } else {
        format!("---\n{yaml}\n---\n{body}\n")
    }
}

pub fn parse(yaml: &str) -> Result<Value, serde_yaml::Error> {
    serde_yaml::from_str::<Value>(yaml)
}

pub fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

pub fn opt_string(value: &Value, key: &str) -> Option<String> {
    string(value, key).filter(|s| !s.trim().is_empty())
}

pub fn u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

pub fn f32(value: &Value, key: &str) -> Option<f32> {
    value.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

pub fn bool(value: &Value, key: &str, default: bool) -> bool {
    value
        .get(key)
        .and_then(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::String(s) => match s.as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            },
            Value::Number(n) => n.as_i64().map(|n| n != 0),
            _ => None,
        })
        .unwrap_or(default)
}

pub fn list(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::String(s)) => s
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_renders_round_trip() {
        let doc = render("id: rec_1\nactive: true", "hello\nworld");
        let (yaml, body) = split(&doc).expect("front matter");
        assert_eq!(body, "hello\nworld\n");
        let parsed = parse(yaml).expect("valid yaml");
        assert_eq!(string(&parsed, "id").as_deref(), Some("rec_1"));
        assert!(bool(&parsed, "active", false));
    }

    #[test]
    fn missing_front_matter_is_none() {
        assert!(split("# just markdown\n").is_none());
    }

    #[test]
    fn list_accepts_sequence_and_csv() {
        let parsed = parse("a: [x, y]\nb: \"x, y\"\n").unwrap();
        assert_eq!(list(&parsed, "a"), vec!["x", "y"]);
        assert_eq!(list(&parsed, "b"), vec!["x", "y"]);
    }
}
