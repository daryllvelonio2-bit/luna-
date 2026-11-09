//! Lenient decoding for tool-call arguments.
//!
//! A provider streams a function call's arguments as text, and occasionally appends a second
//! object, a stray brace, or trailing prose after an otherwise complete value. That reply is not
//! truncated: the arguments the model meant are there, followed by noise. `serde_json::from_str`
//! rejects it as "trailing characters", so the call was treated as cut off and retried, and a
//! model that repeats the same shape on every attempt lost its call entirely.
//!
//! Decoding the first complete value keeps the usable call. A genuinely truncated value (EOF
//! mid-object) still fails: silently closing it would let a half-written `write_file` apply the
//! wrong edit.

use serde_json::Value;

/// Decode the arguments of one tool call.
///
/// An empty fragment means "no arguments" and decodes to `{}`. A complete value followed by
/// trailing characters decodes to that first value. A value that ends early is still an error.
pub fn parse_tool_arguments(raw: &str) -> Result<Value, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }

    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        // A syntax error can be trailing junk after a whole value. An EOF is a value cut off
        // mid-flight, which must stay an error.
        Err(error) if error.classify() == serde_json::error::Category::Syntax => {
            let mut values = serde_json::Deserializer::from_str(raw).into_iter::<Value>();
            match values.next() {
                Some(Ok(value)) => Ok(value),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_tool_arguments;

    #[test]
    fn empty_arguments_are_an_empty_object() {
        assert_eq!(parse_tool_arguments("  ").unwrap(), serde_json::json!({}));
    }

    #[test]
    fn a_whole_value_decodes_unchanged() {
        let value = parse_tool_arguments(r#"{"path":"/tmp/a","limit":5}"#).unwrap();
        assert_eq!(value["path"], "/tmp/a");
        assert_eq!(value["limit"], 5);
    }

    /// The case the saver hit: a whole object followed by a second one. The first is the call.
    #[test]
    fn a_second_object_after_the_first_is_ignored() {
        let value = parse_tool_arguments(r#"{"path":"/tmp/a"}{"path":"/tmp/a"}"#).unwrap();
        assert_eq!(value["path"], "/tmp/a");
    }

    #[test]
    fn trailing_prose_after_a_value_is_ignored() {
        let value = parse_tool_arguments(r#"{"path":"/tmp/a"} extra"#).unwrap();
        assert_eq!(value["path"], "/tmp/a");
    }

    /// A value cut off mid-object must not be closed silently: it may be a half-written edit.
    #[test]
    fn a_truncated_value_is_still_an_error() {
        assert!(parse_tool_arguments(r#"{"path":"/tmp/a"#).is_err());
    }

    #[test]
    fn malformed_arguments_are_still_an_error() {
        assert!(parse_tool_arguments(r#"{"path":}"#).is_err());
    }
}
