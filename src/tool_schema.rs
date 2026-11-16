//! Validate the model-facing schemas of Runtime's built-in tools.
//!
//! This deliberately covers the vocabulary used by our built-in definitions,
//! not arbitrary MCP schemas. Unsupported schema assertions fail closed so a
//! future built-in cannot silently advertise a restriction we do not enforce.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::HashSet;

pub fn validate_builtin(definition: &Value, arguments: &Value) -> Result<()> {
    let schema = definition
        .get("parameters")
        .context("Built-in tool has no input schema")?;
    check_schema(schema)?;
    validate(schema, arguments)
}

fn valid_type(kind: &str) -> bool {
    matches!(
        kind,
        "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
    )
}

fn check_schema(schema: &Value) -> Result<()> {
    if schema.is_boolean() {
        return Ok(());
    }
    let fields = schema
        .as_object()
        .context("Invalid built-in input schema")?;
    for (keyword, value) in fields {
        match keyword.as_str() {
            "type" => ensure!(
                value.as_str().is_some_and(valid_type),
                "Unsupported built-in schema type"
            ),
            "properties" => {
                let properties = value
                    .as_object()
                    .context("Invalid built-in schema properties")?;
                for property in properties.values() {
                    check_schema(property)?;
                }
            }
            "required" => {
                let required = value
                    .as_array()
                    .context("Invalid built-in schema required fields")?;
                let mut unique = HashSet::new();
                for field in required {
                    let field = field
                        .as_str()
                        .context("Invalid built-in schema required field")?;
                    ensure!(unique.insert(field), "Duplicate built-in required field");
                }
            }
            "items" | "additionalProperties" => check_schema(value)?,
            "enum" => ensure!(
                value.as_array().is_some_and(|items| !items.is_empty()),
                "Invalid built-in schema choices"
            ),
            "minimum" | "maximum" => ensure!(
                value.as_f64().is_some_and(f64::is_finite),
                "Invalid built-in numeric bound"
            ),
            "minItems" | "maxItems" => {
                ensure!(value.as_u64().is_some(), "Invalid built-in array bound")
            }
            // These annotate the schema and impose no argument restriction.
            "description" | "title" | "$comment" | "default" | "examples" => {}
            _ => bail!("Unsupported built-in schema assertion"),
        }
    }
    Ok(())
}

fn validate(schema: &Value, value: &Value) -> Result<()> {
    if let Some(allowed) = schema.as_bool() {
        ensure!(allowed, "Built-in tool argument is not permitted");
        return Ok(());
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let matches = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => {
                value.is_i64()
                    || value.is_u64()
                    || value.as_f64().is_some_and(|number| number.fract() == 0.0)
            }
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        ensure!(matches, "Built-in tool argument has the wrong type");
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        ensure!(
            choices.contains(value),
            "Built-in tool argument is not a declared choice"
        );
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field.as_str().expect("checked schema");
                // The name comes from the offered schema, so the model can correct
                // its call without any supplied text being repeated.
                ensure!(
                    object.contains_key(field),
                    "Built-in tool argument is missing the required field `{field}`"
                );
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, item) in object {
            if let Some(property) = properties.and_then(|properties| properties.get(key)) {
                validate(property, item)?;
            } else if let Some(additional) = schema.get("additionalProperties") {
                // No supplied key or value is included in validation errors; an
                // undeclared key is answered with the fields the schema declares.
                if additional == &Value::Bool(false) {
                    let mut declared = properties
                        .map(|properties| properties.keys().map(String::as_str).collect::<Vec<_>>())
                        .unwrap_or_default();
                    declared.sort_unstable();
                    bail!(
                        "Built-in tool argument is not permitted; fields declared here: {}",
                        if declared.is_empty() {
                            "none".to_owned()
                        } else {
                            declared.join(", ")
                        }
                    );
                }
                validate(additional, item)?;
            }
        }
    }
    if let Some(items) = value.as_array() {
        let count = items.len() as u64;
        if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64) {
            ensure!(count >= minimum, "Built-in tool array has too few items");
        }
        if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64) {
            ensure!(count <= maximum, "Built-in tool array has too many items");
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate(item_schema, item)?;
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
            ensure!(
                number >= minimum,
                "Built-in tool argument is below its minimum"
            );
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
            ensure!(
                number <= maximum,
                "Built-in tool argument exceeds its maximum"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decisions, elicitation, tools};
    use serde_json::json;

    fn write_definition() -> Value {
        tools::definitions()
            .into_iter()
            .find(|definition| definition["name"] == "workspace_write")
            .unwrap()
    }

    #[test]
    fn built_in_write_rejects_claimed_identity_without_stripping_arguments() {
        let definition = write_definition();
        for (key, value) in [
            ("server", json!("minecraft")),
            ("tool", json!("observe")),
            (
                "_meta",
                json!({"com.microsoft.amplifier.tool":{"version":1,"kind":"mcp","server":"minecraft","name":"observe"}}),
            ),
        ] {
            let mut arguments = json!({"path":"proof.txt","content":"arbitrary valid text"});
            arguments[key] = value;
            let original = arguments.clone();
            assert!(validate_builtin(&definition, &arguments).is_err());
            assert_eq!(arguments, original);
        }
    }

    #[test]
    fn required_fields_and_types_are_checked_before_execution() {
        let definition = write_definition();
        for arguments in [
            json!(null),
            json!([]),
            json!("input"),
            json!({"path":"proof.txt"}),
            json!({"content":"value"}),
            json!({"path":7,"content":"value"}),
            json!({"path":"proof.txt","content":null}),
        ] {
            assert!(validate_builtin(&definition, &arguments).is_err());
        }
        assert!(
            validate_builtin(
                &definition,
                &json!({"path":"proof.txt","content":"server=minecraft\ntool=observe\n"})
            )
            .is_ok()
        );
    }

    #[test]
    fn command_arguments_remain_flexible_but_must_have_declared_shape() {
        let definition = tools::definition(
            "workspace_run",
            "test",
            json!({"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),
            vec!["command", "args"],
        );
        let valid = json!({"command":"arbitrary-command","args":["--option","server=minecraft","tool=observe"]});
        assert!(validate_builtin(&definition, &valid).is_ok());
        for arguments in [
            json!({"command":"program","args":"one string"}),
            json!({"command":"program","args":[1]}),
            json!({"command":"program","args":[],"server":"minecraft","tool":"observe"}),
        ] {
            assert!(validate_builtin(&definition, &arguments).is_err());
        }
    }

    #[test]
    fn actual_form_schema_checks_nested_keys_choices_and_array_bounds() {
        let definition = elicitation::definition();
        let valid = json!({"message":"Choose", "questions":[{"id":"choice","label":"Direction","kind":"choice","options":["left","right"],"required":false}]});
        assert!(validate_builtin(&definition, &valid).is_ok());
        for mutate in 0..6 {
            let mut arguments = valid.clone();
            match mutate {
                0 => arguments["questions"][0]["server"] = json!("minecraft"),
                1 => arguments["questions"][0]["kind"] = json!("permission"),
                2 => arguments["questions"][0]["options"] = json!([42]),
                3 => arguments["questions"][0]["required"] = json!("false"),
                4 => arguments["questions"] = json!([]),
                _ => arguments["questions"] = json!(vec![valid["questions"][0].clone(); 6]),
            }
            assert!(validate_builtin(&definition, &arguments).is_err());
        }
    }

    #[test]
    fn actual_decision_schema_preserves_free_state_and_checks_question_shapes() {
        let mut backend = decisions::Backend::preset(decisions::Kind::Openai, "chosen".into());
        backend.expose_to_agent = true;
        let definition = decisions::definition(&[backend]).unwrap();
        let arguments = json!({
            "backend":"chosen",
            "state":{"server":"minecraft","tool":"observe","_meta":{"arbitrary":[null,true,42]}}
            ,"questions":[{"name":"useful","type":"predicate","instructions":"Evaluate evidence"}]
        });
        assert!(validate_builtin(&definition, &arguments).is_ok());
        for state in [json!(null), json!(42), json!("free text"), json!([1, 2])] {
            let mut candidate = arguments.clone();
            candidate["state"] = state;
            assert!(validate_builtin(&definition, &candidate).is_ok());
        }
        let mut candidate = arguments.clone();
        candidate["backend"] = json!("not-configured");
        assert!(validate_builtin(&definition, &candidate).is_err());
        candidate = arguments;
        candidate["questions"][0]["choices"] =
            json!([{"value":"yes","description":"Yes","_meta":{}}]);
        assert!(validate_builtin(&definition, &candidate).is_err());
    }

    #[test]
    fn integer_ranges_and_optional_fields_follow_the_offered_schema() {
        let definition = tools::definition(
            "session_history",
            "test",
            json!({"start_item":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20},"include_tools":{"type":"boolean"}}),
            vec![],
        );
        for arguments in [
            json!({}),
            json!({"limit":1}),
            json!({"limit":20,"start_item":0,"include_tools":true}),
        ] {
            assert!(validate_builtin(&definition, &arguments).is_ok());
        }
        for arguments in [
            json!({"limit":0}),
            json!({"limit":21}),
            json!({"limit":1.5}),
            json!({"start_item":-1}),
            json!({"include_tools":"true"}),
        ] {
            assert!(validate_builtin(&definition, &arguments).is_err());
        }
    }

    #[test]
    fn model_facing_artifact_schema_is_distinct_from_internal_byte_call() {
        let definition = tools::definition(
            "workspace_save_artifact",
            "test",
            json!({"artifact":{"type":"string"},"path":{"type":"string"}}),
            vec!["artifact", "path"],
        );
        assert!(
            validate_builtin(
                &definition,
                &json!({"artifact":"latest","path":"output.png"})
            )
            .is_ok()
        );
        assert!(
            validate_builtin(&definition, &json!({"path":"output.png","data":"base64"})).is_err()
        );
    }

    #[test]
    fn unsupported_or_malformed_schema_assertions_fail_closed_even_if_optional() {
        for schema in [
            json!({"type":"object","properties":{"optional":{"type":"string","pattern":"abc"}}}),
            json!({"type":"object","required":[42]}),
            json!({"type":"object","additionalProperties":"false"}),
            json!({"type":"array","minItems":-1}),
            json!({"type":"invalid"}),
        ] {
            assert!(validate_builtin(&json!({"parameters":schema}), &json!({})).is_err());
        }
        assert!(validate_builtin(&json!({}), &json!({})).is_err());
    }

    #[test]
    fn errors_name_only_what_the_offered_schema_declares() {
        let definition = write_definition();
        let error = validate_builtin(
            &definition,
            &json!({"path":"proof.txt","content":"value","SUPPLIED-KEY":"SUPPLIED-VALUE"}),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "Built-in tool argument is not permitted; fields declared here: content, path"
        );
        let error = validate_builtin(&definition, &json!({"content":"SUPPLIED-VALUE"}))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "Built-in tool argument is missing the required field `path`"
        );
    }

    #[test]
    fn errors_never_echo_supplied_keys_or_values() {
        let secret = "PRIVATE-ARGUMENT-DO-NOT-ECHO";
        let definition = write_definition();
        let mut arguments = json!({"path":"proof.txt","content":"value"});
        arguments[secret] = json!(secret);
        let error = validate_builtin(&definition, &arguments).unwrap_err();
        assert!(!format!("{error:#}").contains(secret));
        let definition = elicitation::definition();
        let error = validate_builtin(
            &definition,
            &json!({"message":"choose","questions":[{"id":"q","label":"question","kind":secret}]}),
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains(secret));
    }
}
