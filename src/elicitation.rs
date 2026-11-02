//! Optional ACP form input. The pending request lives on one Peer connection;
//! no answer or authority is accepted from another connection or saved history.
use crate::{rpc::Peer, tools};
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

pub fn definition() -> Value {
    tools::definition(
        "user_input",
        "Ask the current user for a small amount of structured, non-sensitive information. Use for a choice or missing detail that materially affects the task. Never request passwords, API keys, tokens, private keys, recovery codes, or payment credentials. This is not an approval or permission tool. Respect decline/cancel; do not invent answers. Only available when this client supports ACP forms.",
        json!({
            "message":{"type":"string"},
            "questions":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"object","properties":{
                "id":{"type":"string"},"label":{"type":"string"},"kind":{"type":"string","enum":["text","choice","boolean","number"]},
                "options":{"type":"array","items":{"type":"string"}},"required":{"type":"boolean"}
            },"required":["id","label","kind"],"additionalProperties":false}}
        }),
        vec!["message", "questions"],
    )
}

fn schema(args: &Value) -> Result<Value> {
    let message = args["message"].as_str().context("A message is required")?;
    ensure!(
        !message.trim().is_empty() && message.len() <= 4096,
        "Input request message is empty or too long"
    );
    let questions = args["questions"]
        .as_array()
        .context("Questions must be an array")?;
    ensure!(
        (1..=5).contains(&questions.len()),
        "Ask between one and five questions"
    );
    let mut properties = Map::new();
    let mut required = Vec::new();
    for question in questions {
        let id = question["id"].as_str().context("Question ID required")?;
        ensure!(
            !id.is_empty()
                && id.len() <= 80
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "Question IDs must use letters, digits or underscores"
        );
        ensure!(!properties.contains_key(id), "Question IDs must be unique");
        let title = question["label"]
            .as_str()
            .context("Question label required")?;
        ensure!(
            !title.trim().is_empty() && title.len() <= 1024,
            "Question label is empty or too long"
        );
        let mut property = match question["kind"].as_str() {
            Some("text") => json!({"type":"string","maxLength":4096}),
            Some("boolean") => json!({"type":"boolean"}),
            Some("number") => json!({"type":"number"}),
            Some("choice") => {
                let options = question["options"]
                    .as_array()
                    .context("Choice options required")?;
                ensure!(
                    (1..=20).contains(&options.len())
                        && options
                            .iter()
                            .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 200)),
                    "Choices require 1–20 short string options"
                );
                json!({"type":"string","enum":options})
            }
            _ => anyhow::bail!("Unsupported question kind"),
        };
        property["title"] = json!(title);
        properties.insert(id.to_owned(), property);
        if question.get("required").is_none_or(|v| v == true) {
            required.push(id);
        }
        ensure!(
            question.get("required").is_none_or(Value::is_boolean),
            "required must be boolean"
        );
    }
    Ok(json!({"type":"object","properties":properties,"required":required}))
}

fn validate_answer(schema: &Value, result: Value) -> Result<Value> {
    let action = result["action"]
        .as_str()
        .context("Elicitation action missing")?;
    if matches!(action, "decline" | "cancel") {
        return Ok(json!({"action":action}));
    }
    ensure!(action == "accept", "Unknown elicitation action");
    let empty = Map::new();
    let content = match result.get("content").filter(|v| !v.is_null()) {
        None => &empty,
        Some(value) => value.as_object().context("Form answer must be an object")?,
    };
    let properties = schema["properties"].as_object().unwrap();
    ensure!(
        serde_json::to_vec(content)?.len() <= 32 * 1024,
        "Form answer is too large"
    );
    for field in schema["required"].as_array().unwrap() {
        ensure!(
            content.contains_key(field.as_str().unwrap()),
            "A required form answer is missing"
        );
    }
    for (key, value) in content {
        let property = properties
            .get(key)
            .context("Unexpected form answer field")?;
        ensure!(
            match property["type"].as_str() {
                Some("string") => value.as_str().is_some_and(|s| s.chars().count() <= 4096),
                Some("number") => value.is_number(),
                Some("boolean") => value.is_boolean(),
                _ => false,
            },
            "Form answer has an invalid type or length"
        );
        if let Some(options) = property["enum"].as_array() {
            ensure!(
                options.contains(value),
                "Form answer is not an offered choice"
            );
        }
    }
    Ok(json!({"action":"accept","content":content}))
}

pub async fn ask(
    peer: &Peer,
    sid: &str,
    call: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let schema = schema(args)?;
    let result = peer.call("elicitation/create", json!({"sessionId":sid,"toolCallId":call,"mode":"form","message":args["message"],"requestedSchema":schema}), cancel).await?;
    validate_answer(&schema, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_answers_are_validated_and_declines_cannot_supply_values() {
        let schema = schema(&json!({"message":"Choose a direction", "questions":[{"id":"direction","label":"Direction","kind":"choice","options":["small","large"]}]})).unwrap();
        assert!(
            validate_answer(
                &schema,
                json!({"action":"accept","content":{"direction":"small"}})
            )
            .is_ok()
        );
        for content in [
            json!({}),
            json!({"direction":"invented"}),
            json!({"direction":true}),
            json!({"direction":"small","authority":"admin"}),
        ] {
            assert!(
                validate_answer(&schema, json!({"action":"accept","content":content})).is_err()
            );
        }
        assert_eq!(
            validate_answer(
                &schema,
                json!({"action":"decline","content":{"direction":"large"}})
            )
            .unwrap(),
            json!({"action":"decline"})
        );
    }
}
