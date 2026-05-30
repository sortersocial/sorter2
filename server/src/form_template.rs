use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// Serialize a value to compact JSON for a hidden `__rpc__` (or similar) field.
pub fn template_json_compact<T: Serialize>(v: &T) -> serde_json::Result<String> {
    serde_json::to_string(v)
}

/// Recursively walk the JSON AST and replace form holes with submitted values.
///
/// - `{"$form": "key"}` → string (empty if missing)
/// - `{"$form:i32": "key"}` → JSON number (0 if missing or unparseable)
pub fn substitute_form_vars(val: &mut Value, form_data: &HashMap<String, String>) {
    match val {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some((hole_key, Value::String(field_name))) = map.iter().next() {
                    if let Some(form_type) = hole_key.strip_prefix("$form") {
                        let submitted = form_data
                            .get(field_name.as_str())
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        *val = match form_type {
                            "" => Value::String(submitted.to_string()),
                            ":i32" => {
                                let n: i32 = submitted.trim().parse().unwrap_or(0);
                                Value::Number(n.into())
                            }
                            _ => Value::String(submitted.to_string()),
                        };
                        return;
                    }
                }
            }
            for v in map.values_mut() {
                substitute_form_vars(v, form_data);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                substitute_form_vars(v, form_data);
            }
        }
        _ => {}
    }
}

/// Parse JSON, apply [`substitute_form_vars`], return the mutated value.
pub fn fill_template_from_form(
    template_json: &str,
    form_data: &HashMap<String, String>,
) -> Result<Value, serde_json::Error> {
    let mut v: Value = serde_json::from_str(template_json)?;
    substitute_form_vars(&mut v, form_data);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Demo {
        room: String,
        thread_tag: String,
        nested: Nested,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Nested {
        text: String,
    }

    #[test]
    fn i32_holes_become_numbers() {
        let json = r#"{
            "ratio_left": {"$form:i32": "ratio_left"},
            "ratio_right": {"$form:i32": "ratio_right"}
        }"#;
        let mut form = HashMap::new();
        form.insert("ratio_left".into(), "75".into());
        form.insert("ratio_right".into(), "25".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["ratio_left"], 75);
        assert_eq!(v["ratio_right"], 25);

        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Ratios {
            ratio_left: i32,
            ratio_right: i32,
        }
        let r: Ratios = serde_json::from_value(v).unwrap();
        assert_eq!(
            r,
            Ratios {
                ratio_left: 75,
                ratio_right: 25,
            }
        );
    }

    #[test]
    fn i32_hole_missing_or_bad_defaults_to_zero() {
        let json = r#"{"n": {"$form:i32": "missing"}}"#;
        let v = fill_template_from_form(json, &HashMap::new()).unwrap();
        assert_eq!(v["n"], 0);
        let mut form = HashMap::new();
        form.insert("missing".into(), "nope".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["n"], 0);
    }

    #[test]
    fn holes_become_strings() {
        let json = r#"{
            "room": "public",
            "thread_tag": {"$form": "tag"},
            "nested": {"text": {"$form": "body"}}
        }"#;
        let mut form = HashMap::new();
        form.insert("tag".into(), "foo".into());
        form.insert("body".into(), "hello\nworld".into());

        let v = fill_template_from_form(json, &form).unwrap();
        let d: Demo = serde_json::from_value(v).unwrap();
        assert_eq!(
            d,
            Demo {
                room: "public".into(),
                thread_tag: "foo".into(),
                nested: Nested {
                    text: "hello\nworld".into(),
                },
            }
        );
    }

    #[test]
    fn missing_form_key_is_empty_string() {
        let json = r#"{"x": {"$form": "nope"}}"#;
        let mut form = HashMap::new();
        form.insert("other".into(), "y".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["x"], "");
    }

    #[test]
    fn array_of_holes() {
        let json = r#"{"items": [{"$form": "a"}, {"$form": "b"}]}"#;
        let mut form = HashMap::new();
        form.insert("a".into(), "1".into());
        form.insert("b".into(), "2".into());
        let v = fill_template_from_form(json, &form).unwrap();
        assert_eq!(v["items"], serde_json::json!(["1", "2"]));
    }

    #[test]
    fn template_json_compact_escapes_and_single_line() {
        let s = template_json_compact(&serde_json::json!({
            "x": "quote\"and\nnewline"
        }))
        .unwrap();
        assert!(!s.contains('\n'));
        assert!(s.contains("\\\"") || s.contains("\\n"));
    }
}
