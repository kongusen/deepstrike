//! Tool-call argument text is model-authored.
//!
//! A call whose arguments are not a JSON object (a truncated stream, prose, an array) must reach
//! the executor as-is so it fails as an invalid call — rewriting it to `{}` would run the tool with
//! empty arguments the model never wrote. Mirrors Node `runtime/tool-arguments.ts`.

use serde_json::Value;

/// The arguments a provider hands the runner: the parsed object, `{}` for empty text, and the raw
/// text as a JSON string for anything that is not a JSON object.
pub fn from_model_text(text: &str) -> Value {
    if text.trim().is_empty() {
        return Value::Object(Default::default());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::String(text.to_string()),
    }
}

/// Execution requires a JSON object; anything else is an invalid call.
pub fn require_object(arguments: &Value) -> Result<(), String> {
    match arguments {
        Value::Object(_) => Ok(()),
        Value::Null => Ok(()),
        Value::String(text) => Err(match serde_json::from_str::<Value>(text) {
            Ok(_) => "arguments must be a JSON object".to_string(),
            Err(error) => format!("arguments are not valid JSON ({error})"),
        }),
        _ => Err("arguments must be a JSON object".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_object_text_is_kept_and_refused() {
        assert_eq!(from_model_text(""), serde_json::json!({}));
        assert_eq!(from_model_text("{\"a\":1}"), serde_json::json!({"a": 1}));
        let truncated = from_model_text("{\"path\": \"/tm");
        assert_eq!(truncated, Value::String("{\"path\": \"/tm".into()));
        assert!(
            require_object(&truncated)
                .unwrap_err()
                .contains("not valid JSON")
        );
        assert!(
            require_object(&from_model_text("[1]"))
                .unwrap_err()
                .contains("JSON object")
        );
        assert!(require_object(&serde_json::json!({})).is_ok());
    }
}
