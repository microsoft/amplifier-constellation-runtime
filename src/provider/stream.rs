use super::history;
use crate::config::ProviderKind;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Bounded SSE/NDJSON framing. HTTP chunk boundaries and UTF-8 boundaries are
/// unrelated; parse JSON only after a complete framed event has arrived.
#[derive(Default)]
pub struct Frames {
    pending: Vec<u8>,
    data: Vec<String>,
    total: usize,
}
impl Frames {
    pub fn feed(&mut self, bytes: &[u8], ndjson: bool) -> Result<Vec<String>> {
        self.total += bytes.len();
        ensure!(
            self.total <= 64 * 1024 * 1024,
            "Model response exceeded byte budget"
        );
        self.pending.extend_from_slice(bytes);
        ensure!(
            self.pending.len() <= 24 * 1024 * 1024,
            "Model event too large"
        );
        let mut out = vec![];
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if ndjson {
                if !line.is_empty() {
                    out.push(line.into());
                }
            } else if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).into());
            }
            ensure!(
                self.data.iter().map(String::len).sum::<usize>() <= 24 * 1024 * 1024,
                "Model event too large"
            );
        }
        Ok(out)
    }
    pub fn finish(&mut self, ndjson: bool) -> Result<Vec<String>> {
        self.feed(if ndjson { b"\n" } else { b"\n\n" }, ndjson)
    }
}
pub struct Collector {
    kind: ProviderKind,
    text: String,
    thinking: String,
    reasoning_field: Option<&'static str>,
    calls: BTreeMap<u64, Value>,
    blocks: BTreeMap<u64, Value>,
    arguments: BTreeMap<u64, String>,
    parts: Vec<Value>,
    usage: Value,
    stop: Option<String>,
    terminal: bool,
    response: Option<Value>,
    container: Value,
    grounding: Value,
}
impl Collector {
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            text: String::new(),
            thinking: String::new(),
            reasoning_field: None,
            calls: BTreeMap::new(),
            blocks: BTreeMap::new(),
            arguments: BTreeMap::new(),
            parts: vec![],
            usage: json!({}),
            stop: None,
            terminal: false,
            response: None,
            container: Value::Null,
            grounding: json!({}),
        }
    }
    pub fn terminal(&self) -> bool {
        self.terminal
    }
    pub fn event(&mut self, event: Value) -> Result<Vec<String>> {
        ensure!(
            event.get("error").is_none(),
            "Model reported an error; no tool was retried"
        );
        let mut text = vec![];
        match self.kind {
            ProviderKind::Openai | ProviderKind::AzureOpenai | ProviderKind::Vllm => {
                match event["type"].as_str().unwrap_or("") {
                    "response.output_text.delta" | "response.refusal.delta" => {
                        if let Some(s) = event["delta"].as_str() {
                            text.push(s.into());
                        }
                    }
                    "response.completed" | "response.incomplete" => {
                        self.response = Some(event["response"].clone());
                        self.terminal = true;
                    }
                    "response.failed" | "error" => {
                        bail!("Model reported a failed response; no tool was retried")
                    }
                    _ => {}
                }
            }
            ProviderKind::ChatCompletions | ProviderKind::Litellm => {
                if event["usage"].is_object() {
                    self.usage = event["usage"].clone();
                }
                if let Some(choice) = event["choices"].as_array().and_then(|v| v.first()) {
                    let delta = &choice["delta"];
                    if let Some(s) = delta["content"].as_str() {
                        self.text.push_str(s);
                        text.push(s.into());
                    }
                    if let Some(s) = delta["refusal"].as_str() {
                        self.text.push_str(s);
                        text.push(s.into());
                    }
                    // vLLM renamed reasoning_content to reasoning. Preserve the
                    // observed wire field privately for the next tool continuation.
                    // A compatibility server may send both aliases; select once.
                    let reasoning = delta["reasoning"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(|s| ("reasoning", s))
                        .or_else(|| {
                            delta["reasoning_content"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(|s| ("reasoning_content", s))
                        });
                    if let Some((field, s)) = reasoning {
                        if self.reasoning_field.is_none() || field == "reasoning" {
                            self.reasoning_field = Some(field);
                        }
                        self.thinking.push_str(s);
                    }
                    if let Some(calls) = delta["tool_calls"].as_array() {
                        for call in calls {
                            let index = call["index"]
                                .as_u64()
                                .context("Streamed tool call missing index")?;
                            let entry=self.calls.entry(index).or_insert(json!({"id":"","type":"function","function":{"name":"","arguments":""}}));
                            if let Some(s) = call["id"].as_str() {
                                let mut value = entry["id"].as_str().unwrap_or("").to_string();
                                value.push_str(s);
                                entry["id"] = json!(value);
                            }
                            for key in ["name", "arguments"] {
                                if let Some(s) = call["function"][key].as_str() {
                                    let mut value =
                                        entry["function"][key].as_str().unwrap_or("").to_string();
                                    value.push_str(s);
                                    entry["function"][key] = json!(value);
                                }
                            }
                        }
                    }
                    if let Some(s) = choice["finish_reason"].as_str() {
                        self.stop = Some(s.into());
                    }
                }
            }
            ProviderKind::Anthropic => match event["type"].as_str().unwrap_or("") {
                "message_start" => {
                    self.usage = event["message"]["usage"].clone();
                    self.container = event["message"]["container"].clone();
                }
                "content_block_start" => {
                    let index = event["index"].as_u64().context("Content index missing")?;
                    let b = event["content_block"].clone();
                    if b["type"] == "text"
                        && let Some(s) = b["text"].as_str()
                        && !s.is_empty()
                    {
                        text.push(s.into());
                    }
                    self.blocks.insert(index, b);
                }
                "content_block_delta" => {
                    let index = event["index"].as_u64().context("Content index missing")?;
                    let b = self
                        .blocks
                        .get_mut(&index)
                        .context("Delta has no content block")?;
                    let d = &event["delta"];
                    match d["type"].as_str().unwrap_or("") {
                        "text_delta" | "thinking_delta" | "signature_delta" => {
                            let key = match d["type"].as_str().unwrap() {
                                "text_delta" => "text",
                                "thinking_delta" => "thinking",
                                _ => "signature",
                            };
                            let s = d[key].as_str().context("Content delta missing value")?;
                            let mut value = b[key].as_str().unwrap_or("").to_owned();
                            value.push_str(s);
                            b[key] = json!(value);
                            if key == "text" {
                                text.push(s.into());
                            }
                        }
                        "input_json_delta" => self.arguments.entry(index).or_default().push_str(
                            d["partial_json"]
                                .as_str()
                                .context("Tool delta missing JSON")?,
                        ),
                        "citations_delta" => b["citations"]
                            .as_array_mut()
                            .map(|v| v.push(d["citation"].clone()))
                            .unwrap_or_else(|| {
                                b["citations"] = json!([d["citation"]]);
                            }),
                        _ => {}
                    }
                }
                "message_delta" => {
                    if event["delta"]["container"].is_object() {
                        self.container = event["delta"]["container"].clone();
                    }
                    self.stop = event["delta"]["stop_reason"].as_str().map(str::to_owned);
                    if let (Some(to), Some(from)) =
                        (self.usage.as_object_mut(), event["usage"].as_object())
                    {
                        to.extend(from.clone());
                    }
                }
                "message_stop" => self.terminal = true,
                "error" => bail!("Anthropic reported an error; no tool was retried"),
                _ => {}
            },
            ProviderKind::Gemini => {
                if event["usageMetadata"].is_object() {
                    self.usage = event["usageMetadata"].clone();
                }
                if let Some(candidate) = event["candidates"].as_array().and_then(|v| v.first()) {
                    if let Some(m) = candidate["groundingMetadata"].as_object() {
                        for (k, v) in m {
                            if v.is_array() {
                                if !self.grounding[k].is_array() {
                                    self.grounding[k] = json!([]);
                                }
                                self.grounding[k]
                                    .as_array_mut()
                                    .unwrap()
                                    .extend(v.as_array().unwrap().clone());
                            } else {
                                self.grounding[k] = v.clone();
                            }
                        }
                    }
                    if let Some(parts) = candidate["content"]["parts"].as_array() {
                        for p in parts {
                            if p["thought"] != true
                                && let Some(s) = p["text"].as_str()
                            {
                                text.push(s.into());
                            }
                            self.parts.push(p.clone());
                        }
                    }
                    if let Some(s) = candidate["finishReason"].as_str() {
                        self.stop = Some(s.into());
                    }
                }
                if event["promptFeedback"]["blockReason"].is_string() {
                    self.stop = Some("SAFETY".into());
                }
            }
            ProviderKind::Ollama => {
                if let Some(s) = event["message"]["content"].as_str() {
                    self.text.push_str(s);
                    text.push(s.into());
                }
                if let Some(s) = event["message"]["thinking"].as_str() {
                    self.thinking.push_str(s);
                }
                if let Some(calls) = event["message"]["tool_calls"].as_array() {
                    for call in calls {
                        self.calls.insert(self.calls.len() as u64, call.clone());
                    }
                }
                if event["done"] == true {
                    self.terminal = true;
                    self.stop = Some(event["done_reason"].as_str().unwrap_or("stop").into());
                    self.usage = json!({"prompt_eval_count":event["prompt_eval_count"],"eval_count":event["eval_count"]});
                }
            }
            ProviderKind::Mock | ProviderKind::GithubCopilot => {
                bail!("This provider has no HTTP stream")
            }
        }
        Ok(text)
    }
    pub fn finish(mut self) -> Result<Value> {
        if let Some(r) = self.response {
            ensure!(
                r["output"].is_array(),
                "Responses endpoint returned no output"
            );
            return Ok(r);
        }
        let reason = self
            .stop
            .as_deref()
            .context("Model stream ended before a completion marker; no tool executed")?;
        if self.kind == ProviderKind::Anthropic || self.kind == ProviderKind::Ollama {
            ensure!(
                self.terminal,
                "Model stream ended before its final event; no tool executed"
            );
        }
        let (status, stop) = match reason {
            "stop" | "tool_calls" | "end_turn" | "tool_use" | "STOP" => ("completed", "end_turn"),
            "pause_turn" => ("completed", "pause_turn"),
            "length" | "max_tokens" | "MAX_TOKENS" => ("incomplete", "max_tokens"),
            "content_filter" | "refusal" | "SAFETY" | "RECITATION" | "BLOCKLIST"
            | "PROHIBITED_CONTENT" => ("incomplete", "refusal"),
            _ => bail!("Provider stopped with unsupported reason {reason}; no tool executed"),
        };
        let mut public = vec![];
        let (message, usage) = match self.kind {
            ProviderKind::Anthropic => {
                for (index, args) in self.arguments {
                    self.blocks
                        .get_mut(&index)
                        .context("Tool JSON has no block")?["input"] = serde_json::from_str(&args)
                        .context("Incomplete Anthropic tool arguments")?;
                }
                let blocks: Vec<_> = self.blocks.into_values().collect();
                for b in &blocks {
                    match b["type"].as_str() {
                        Some("text") => {
                            public.push(history::text_item(b["text"].as_str().unwrap_or("")))
                        }
                        Some("tool_use") => public.push(history::call_item(
                            b["id"].as_str().unwrap_or(""),
                            b["name"].as_str().unwrap_or(""),
                            &b["input"],
                            None,
                        )?),
                        Some("thinking" | "redacted_thinking" | "server_tool_use") => {}
                        Some(t) if t.ends_with("_tool_result") => {}
                        _ => bail!("Unsupported Anthropic content block"),
                    }
                }
                (
                    json!({"role":"assistant","content":blocks}),
                    history::usage(
                        &self.usage["input_tokens"],
                        &self.usage["output_tokens"],
                        &self.usage,
                    ),
                )
            }
            ProviderKind::Gemini => {
                for p in &self.parts {
                    if let Some(c) = p.get("functionCall") {
                        ensure!(
                            c.get("partialArgs").is_none(),
                            "Partial Gemini function arguments are unsupported"
                        );
                        let wire = c["id"].as_str();
                        let id = wire
                            .map(str::to_owned)
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        public.push(history::call_item(
                            &id,
                            c["name"].as_str().unwrap_or(""),
                            c.get("args").unwrap_or(&json!({})),
                            wire,
                        )?);
                    } else if p["thought"] != true {
                        if let Some(s) = p["text"].as_str() {
                            public.push(history::text_item(s));
                        } else if p.get("inlineData").is_some()
                            || p.get("executableCode").is_some()
                            || p.get("codeExecutionResult").is_some()
                            || p.get("toolCall").is_some()
                            || p.get("toolResponse").is_some()
                        {
                            // Hosted execution and inline media are rendered separately.
                        } else {
                            bail!("Gemini returned unsupported non-text content");
                        }
                    }
                }
                (
                    json!({"role":"model","parts":self.parts}),
                    history::usage(
                        &self.usage["promptTokenCount"],
                        &self.usage["candidatesTokenCount"],
                        &self.usage,
                    ),
                )
            }
            ProviderKind::ChatCompletions | ProviderKind::Litellm | ProviderKind::Ollama => {
                let mut m = json!({"role":"assistant","content":self.text});
                if !self.text.is_empty() {
                    public.push(history::text_item(&self.text));
                }
                if !self.thinking.is_empty() {
                    m[if self.kind == ProviderKind::Ollama {
                        "thinking"
                    } else {
                        self.reasoning_field.unwrap_or("reasoning_content")
                    }] = json!(self.thinking);
                }
                if !self.calls.is_empty() {
                    let calls: Vec<_> = self.calls.into_values().collect();
                    for c in &calls {
                        let id = if self.kind == ProviderKind::Ollama {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            c["id"].as_str().context("Chat tool ID missing")?.into()
                        };
                        public.push(history::call_item(
                            &id,
                            c["function"]["name"].as_str().unwrap_or(""),
                            &c["function"]["arguments"],
                            None,
                        )?);
                    }
                    m["tool_calls"] = json!(calls);
                }
                let usage = if self.kind == ProviderKind::Ollama {
                    history::usage(
                        &self.usage["prompt_eval_count"],
                        &self.usage["eval_count"],
                        &self.usage,
                    )
                } else {
                    history::usage(
                        &self.usage["prompt_tokens"],
                        &self.usage["completion_tokens"],
                        &self.usage,
                    )
                };
                (m, usage)
            }
            _ => bail!("Model stream ended before completion"),
        };
        let mut result = history::normalized(self.kind, message, public, status, usage, stop);
        if !self.container.is_null() {
            result["output"][0]["container"] = self.container;
        }
        if self.grounding.as_object().is_some_and(|o| !o.is_empty()) {
            result["output"][0]["groundingMetadata"] = self.grounding;
        }
        Ok(result)
    }
}
