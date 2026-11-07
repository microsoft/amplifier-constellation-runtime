//! Owner-listed ACP choices. Saved choices never grant a new provider route.
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelOption {
    pub model: String,
    pub name: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub route: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
}

pub fn options(config: &Config) -> Vec<ModelOption> {
    if config.model_options.is_empty() {
        vec![ModelOption {
            model: config.provider.model.clone(),
            name: config.provider.model.clone(),
            reasoning_effort: config.provider.reasoning_effort.clone(),
            reasoning_efforts: config
                .provider
                .reasoning_effort
                .clone()
                .into_iter()
                .collect(),
        }]
    } else {
        config.model_options.clone()
    }
}

pub fn validate_options(config: &Config, home: &Path) -> Result<()> {
    if config.model_options.is_empty() {
        return Ok(());
    }
    ensure!(
        config.model_options.len() <= 32,
        "Too many configured model options"
    );
    let mut ids = HashSet::new();
    for choice in &config.model_options {
        ensure!(ids.insert(&choice.model), "Duplicate model option");
        ensure!(
            !choice.name.trim().is_empty() && choice.name.len() <= 200,
            "Invalid model option name"
        );
        ensure!(
            choice.reasoning_efforts.len() <= 8,
            "Too many reasoning choices"
        );
        let efforts: HashSet<_> = choice.reasoning_efforts.iter().collect();
        ensure!(
            efforts.len() == choice.reasoning_efforts.len(),
            "Duplicate reasoning option"
        );
        ensure!(
            choice.reasoning_effort.as_ref().map_or_else(
                || efforts.is_empty() || choice.reasoning_efforts.iter().any(|e| e == "default"),
                |v| efforts.contains(v)
            ),
            "Default reasoning effort must be offered"
        );
        for effort in std::iter::once(choice.reasoning_effort.clone()).chain(
            choice
                .reasoning_efforts
                .iter()
                .map(|e| (e != "default").then(|| e.clone())),
        ) {
            let mut candidate = config.clone();
            candidate.model_options.clear();
            candidate.provider.model = choice.model.clone();
            candidate.provider.reasoning_effort = effort;
            candidate.validate(home)?;
        }
    }
    let current = config
        .model_options
        .iter()
        .find(|c| c.model == config.provider.model)
        .context("Configured default model must be in model_options")?;
    ensure!(
        current.reasoning_effort == config.provider.reasoning_effort,
        "Default model reasoning must match provider configuration"
    );
    Ok(())
}

/// Includes account selector, permissions on the route and native configuration,
/// but not secrets, the selected model or its reasoning control.
pub fn route(config: &Config) -> String {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let fields: std::collections::BTreeMap<_, _> =
                    map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
                serde_json::to_value(fields).unwrap()
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            v => v,
        }
    }
    let mut value = serde_json::to_value(&config.provider).unwrap();
    value.as_object_mut().unwrap().remove("model");
    value.as_object_mut().unwrap().remove("reasoning_effort");
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&sorted(value)).unwrap())
    )
}

pub fn apply(config: &Config, selection: &Selection) -> Result<Config> {
    ensure!(
        selection.route == route(config),
        "Saved model choice belongs to a different provider route or account configuration; restore that configuration or start a new session"
    );
    let choices = options(config);
    let choice = choices.iter().find(|c| c.model == selection.model)
        .context("Saved model is no longer offered; restore the configured option or start a new session")?;
    ensure!(
        selection.reasoning_effort.as_ref().map_or_else(
            || choice.reasoning_efforts.is_empty()
                || choice.reasoning_efforts.iter().any(|e| e == "default"),
            |e| choice.reasoning_efforts.contains(e)
        ),
        "Saved reasoning effort is no longer offered; restore the configured option"
    );
    let mut selected = config.clone();
    selected.provider.model = selection.model.clone();
    selected.provider.reasoning_effort = selection.reasoning_effort.clone();
    Ok(selected)
}

pub fn config_options(config: &Config) -> Vec<Value> {
    let choices = options(config);
    let mut result = vec![
        json!({"id":"model","name":"Model","category":"model","type":"select","currentValue":config.provider.model,
        "options":choices.iter().map(|c|json!({"value":c.model,"name":c.name})).collect::<Vec<_>>()}),
    ];
    let choice = choices
        .iter()
        .find(|c| c.model == config.provider.model)
        .unwrap();
    if !choice.reasoning_efforts.is_empty() {
        let efforts: Vec<_> = choice
            .reasoning_efforts
            .iter()
            .map(|e| json!({"value":e,"name":if e == "default" { "Provider default" } else { e }}))
            .collect();
        result.push(json!({"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select",
            "currentValue":config.provider.reasoning_effort.as_deref().unwrap_or("default"),"options":efforts}));
    }
    result
}

pub fn allows(config: &Config, key: &str, value: &str) -> bool {
    let choices = options(config);
    match key {
        "model" => choices.iter().any(|c| c.model == value),
        "reasoning_effort" => choices
            .iter()
            .find(|c| c.model == config.provider.model)
            .is_some_and(|c| c.reasoning_efforts.iter().any(|e| e == value)),
        _ => false,
    }
}

/// No projection or deletion of private reasoning to make a switch appear safe.
/// Ordinary text history can cross model choices; opaque or effect-bearing history
/// needs a separately designed migration and remains attached to its original model.
pub fn portable_history(items: &[Value]) -> bool {
    items.iter().all(|item| {
        let Some(object) = item.as_object() else {
            return false;
        };
        matches!(item["role"].as_str(), Some("user" | "assistant"))
            && item.get("type").is_none_or(|v| v == "message")
            && object.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "type"
                        | "role"
                        | "content"
                        | "_acp_prompt"
                        | "_acp_message_id"
                        | "id"
                        | "status"
                )
            })
            && (item["content"].is_string()
                || item["content"].as_array().is_some_and(|parts| {
                    parts.iter().all(|part| {
                        matches!(
                            part["type"].as_str(),
                            Some("text" | "input_text" | "output_text")
                        ) && part["text"].is_string()
                            && part.as_object().is_some_and(|p| {
                                p.keys()
                                    .all(|k| matches!(k.as_str(), "type" | "text" | "annotations"))
                                    && p.get("annotations")
                                        .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
                            })
                    })
                }))
    })
}
