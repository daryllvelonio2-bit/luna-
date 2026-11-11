use harness_core::{ClarifyAnswer, ClarifyItem, ClarifyRequest};
use serde_json::json;

/// How a form field's answer reaches OpenCode.
///
/// The API accepts a scalar for a string/number/boolean field and an array only for a
/// multiselect. Sending an array for every field — which this bridge used to do — is rejected
/// with `FormInvalidAnswerError`, and the form then stays pending forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormFieldKind {
    Scalar,
    Multi,
}

/// One field of an OpenCode form, kept so the user's answer can be encoded as the API expects.
#[derive(Debug, Clone)]
pub(super) struct FormFieldMapping {
    pub(super) key: String,
    pub(super) field_type: String,
    pub(super) kind: FormFieldKind,
    /// The option *values* (not the labels shown to the user), in choice order.
    pub(super) option_values: Vec<String>,
}

/// The marker `prompt_clarify_choice` uses when the user pressed Esc instead of answering.
///
/// A skipped prompt is a cancelled form, not an answer: posting the marker as a value would be
/// invalid and leave the service waiting.
pub(super) const SKIPPED_ANSWER: &str = "(skipped by user)";

pub(super) fn parse_opencode_form_to_clarify(
    form: &serde_json::Value,
) -> (ClarifyRequest, Vec<FormFieldMapping>) {
    let mut questions = Vec::new();
    let mut field_mappings = Vec::new();

    if let Some(fields) = form.get("fields").and_then(|f| f.as_array()) {
        for (i, field) in fields.iter().enumerate() {
            let key = field
                .get("key")
                .and_then(|k| k.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("q{}", i));
            let field_type = field
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string")
                .to_string();

            let title = field.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let desc = field
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            let question_text = if !title.is_empty() && !desc.is_empty() && title != desc {
                format!("{}: {}", title, desc)
            } else if !desc.is_empty() {
                desc.to_string()
            } else if !title.is_empty() {
                title.to_string()
            } else {
                "Please select an option:".to_string()
            };

            let mut choices = Vec::new();
            let mut option_values = Vec::new();

            if let Some(opts) = field.get("options").and_then(|o| o.as_array()) {
                for opt in opts {
                    let value = opt
                        .get("value")
                        .and_then(|v| v.as_str())
                        .or_else(|| opt.get("label").and_then(|l| l.as_str()))
                        .unwrap_or("");
                    if !value.is_empty() {
                        // The reply carries the value; the label is only what the box shows.
                        let label = opt.get("label").and_then(|l| l.as_str()).unwrap_or(value);
                        option_values.push(value.to_string());
                        if let Some(opt_desc) = opt
                            .get("description")
                            .and_then(|d| d.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            choices.push(format!("{} — {}", label, opt_desc));
                        } else {
                            choices.push(label.to_string());
                        }
                    }
                }
            }

            // A boolean field has no options in the wire format, but its answer must still be a
            // bool. Offer the two values so the box is usable instead of asking for free text.
            if choices.is_empty() && field_type == "boolean" {
                choices = vec!["Yes".to_string(), "No".to_string()];
                option_values = vec!["true".to_string(), "false".to_string()];
            }

            questions.push(ClarifyItem {
                question: question_text,
                choices,
            });
            field_mappings.push(FormFieldMapping {
                key,
                kind: if field_type == "multiselect" {
                    FormFieldKind::Multi
                } else {
                    FormFieldKind::Scalar
                },
                field_type,
                option_values,
            });
        }
    }

    (ClarifyRequest { questions }, field_mappings)
}

/// Encode one raw answer as the JSON value the form's field type expects.
pub(super) fn form_value_from_raw(mapping: &FormFieldMapping, raw: &str) -> serde_json::Value {
    let value = match mapping.field_type.as_str() {
        "boolean" => json!(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "yes" | "y" | "1"
        )),
        "number" => match raw.trim().parse::<f64>() {
            Ok(number) => json!(number),
            Err(_) => json!(raw),
        },
        "integer" => match raw.trim().parse::<i64>() {
            Ok(number) => json!(number),
            Err(_) => json!(raw),
        },
        _ => json!(raw),
    };
    // Only multiselect fields take the array shape; everything else is scalar.
    if mapping.kind == FormFieldKind::Multi {
        json!([value])
    } else {
        value
    }
}

/// Turn one answered clarify item into the value for its form field.
pub(super) fn form_value_for_answer(
    mapping: &FormFieldMapping,
    answer: &ClarifyAnswer,
) -> Option<serde_json::Value> {
    let raw = match answer.choice_index {
        Some(index) => mapping.option_values.get(index)?,
        None => &answer.answer,
    };
    Some(form_value_from_raw(mapping, raw))
}
