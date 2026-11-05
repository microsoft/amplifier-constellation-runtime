mod history;
mod stream;
mod websocket;

use crate::{
    config::{Config, ProviderAuth, ProviderKind},
    rpc::Peer,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use stream::{Collector, Frames};
use tokio_util::sync::CancellationToken;

pub struct Provider {
    client: reqwest::Client,
    config: Config,
    key: Option<String>,
    websocket: Option<websocket::Client>,
    pub(crate) boundary_inputs: Vec<crate::steering::Input>,
}
impl Provider {
    pub fn new(config: &Config) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let p = &config.provider;
        let key = if p.kind == ProviderKind::Mock
            || matches!(p.auth, ProviderAuth::None | ProviderAuth::Chatgpt)
        {
            None
        } else {
            std::env::var(&p.api_key_env)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        let required = !matches!(p.auth, ProviderAuth::None | ProviderAuth::Chatgpt)
            && p.kind != ProviderKind::Mock
            && (p.auth != ProviderAuth::Auto
                || matches!(
                    p.kind,
                    ProviderKind::Openai
                        | ProviderKind::AzureOpenai
                        | ProviderKind::Anthropic
                        | ProviderKind::Gemini
                        | ProviderKind::GithubCopilot
                ));
        ensure!(
            !required || key.is_some(),
            "Configure {} in this runtime's private environment",
            p.api_key_env
        );
        ensure!(
            key.as_ref()
                .is_none_or(|v| !v.chars().any(char::is_control)),
            "Model credential contains an unexpected control character"
        );
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(config.limits.turn_seconds));
        if let Some(path) = &p.tls_ca_file {
            for cert in reqwest::Certificate::from_pem_bundle(&std::fs::read(path)?)? {
                builder = builder.add_root_certificate(cert);
            }
        }
        Ok(Self {
            client: builder.build()?,
            config: config.clone(),
            key,
            websocket: None,
            boundary_inputs: vec![],
        })
    }
    async fn headers(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        let p = &self.config.provider;
        if p.auth == ProviderAuth::Chatgpt {
            let token = crate::chatgpt::access_token(
                p.chatgpt_credentials_file
                    .as_ref()
                    .context("ChatGPT sign-in is not configured")?,
            )
            .await?;
            return Ok(request.bearer_auth(token));
        }
        if p.kind == ProviderKind::Anthropic {
            request = request.header("anthropic-version", "2023-06-01");
            if !p.native.anthropic_betas.is_empty() {
                request = request.header("anthropic-beta", p.native.anthropic_betas.join(","));
            }
        }
        if let Some(key) = &self.key {
            let api_key = p.auth == ProviderAuth::ApiKey
                || (p.auth == ProviderAuth::Auto
                    && matches!(
                        p.kind,
                        ProviderKind::Anthropic | ProviderKind::Gemini | ProviderKind::AzureOpenai
                    ));
            if api_key {
                request = request.header(
                    match p.kind {
                        ProviderKind::Anthropic => "x-api-key",
                        ProviderKind::Gemini => "x-goog-api-key",
                        _ => "api-key",
                    },
                    key,
                );
            } else {
                request = request.bearer_auth(key);
            }
        }
        Ok(request)
    }
    fn request(
        &self,
        instructions: &str,
        items: &[Value],
        tools: &[Value],
    ) -> Result<(reqwest::Url, Value)> {
        let p = &self.config.provider;
        let mut url = reqwest::Url::parse(&p.endpoint)?;
        let max = self.config.limits.max_output_tokens;
        let tools = if p.native.disable_function_tools {
            &[][..]
        } else {
            tools
        };
        let mut body = match p.kind {
            ProviderKind::Openai | ProviderKind::AzureOpenai | ProviderKind::Vllm => {
                ensure!(
                    items.iter().all(|v| v["type"] != "provider_state"),
                    "Cannot send another provider's state to Responses"
                );
                let mut b = json!({"model":p.model,"instructions":instructions,"input":history::responses(items),"tools":tools,"max_output_tokens":max,"store":false,"stream":true});
                if p.auth == ProviderAuth::Chatgpt {
                    b.as_object_mut().unwrap().remove("max_output_tokens");
                }
                if p.kind != ProviderKind::Vllm {
                    b["include"] = json!(["reasoning.encrypted_content"]);
                    b["parallel_tool_calls"] = json!(false);
                }
                if let Some(e) = &p.reasoning_effort {
                    b["reasoning"] = json!({"effort":e});
                }
                if let Some(version) = &p.api_version {
                    url.query_pairs_mut().append_pair("api-version", version);
                }
                b
            }
            ProviderKind::Anthropic => {
                let ts:Vec<_>=tools.iter().map(|t|json!({"name":t["name"],"description":t["description"],"input_schema":t["parameters"]})).collect();
                let mut b = json!({"model":p.model,"system":instructions,"messages":history::messages(p.kind,items)?,"tools":ts,"max_tokens":max,"stream":true});
                if let Some(e) = &p.reasoning_effort {
                    b["thinking"] = json!({"type":"adaptive"});
                    b["output_config"] = json!({"effort":e});
                }
                b
            }
            ProviderKind::Gemini => {
                url.path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("Invalid Gemini base URL"))?
                    .pop_if_empty()
                    .push("models")
                    .push(&format!(
                        "{}:streamGenerateContent",
                        p.model.strip_prefix("models/").unwrap_or(&p.model)
                    ));
                url.query_pairs_mut().append_pair("alt", "sse");
                let ts:Vec<_>=tools.iter().map(|t|json!({"name":t["name"],"description":t["description"],"parametersJsonSchema":t["parameters"]})).collect();
                let mut b = json!({"systemInstruction":{"parts":[{"text":instructions}]},"contents":history::messages(p.kind,items)?,"tools":[{"functionDeclarations":ts}],"generationConfig":{"maxOutputTokens":max}});
                if let Some(e) = &p.reasoning_effort {
                    b["generationConfig"]["thinkingConfig"] =
                        json!({"thinkingLevel":e.to_ascii_uppercase()});
                }
                if let Some(budget) = p.thinking_budget_tokens {
                    b["generationConfig"]["thinkingConfig"] = json!({"thinkingBudget":budget});
                }
                b
            }
            ProviderKind::Ollama | ProviderKind::ChatCompletions | ProviderKind::Litellm => {
                let mut messages = vec![json!({"role":"system","content":instructions})];
                messages.extend(history::messages(p.kind, items)?);
                let ts:Vec<_>=tools.iter().map(|t|json!({"type":"function","function":{"name":t["name"],"description":t["description"],"parameters":t["parameters"]}})).collect();
                let mut b = json!({"model":p.model,"messages":messages,"tools":ts,"stream":true});
                if p.kind == ProviderKind::Ollama {
                    b["options"] = json!({"num_predict":max});
                    if let Some(e) = &p.reasoning_effort {
                        b["think"] = json!(e);
                    }
                } else {
                    b["max_tokens"] = json!(max);
                    b["stream_options"] = json!({"include_usage":true});
                    if let Some(e) = &p.reasoning_effort {
                        b["reasoning_effort"] = json!(e);
                    }
                }
                b
            }
            ProviderKind::Mock | ProviderKind::GithubCopilot => {
                bail!("This provider does not use the HTTP transport")
            }
        };
        if tools.is_empty() {
            body.as_object_mut().unwrap().remove("tools");
        }
        if !p.native.tools.is_empty() {
            if matches!(p.kind, ProviderKind::Openai | ProviderKind::AzureOpenai) {
                body["max_tool_calls"] = json!(self.config.limits.max_tool_calls);
            }
            if !body["tools"].is_array() {
                body["tools"] = json!([]);
            }
            body["tools"]
                .as_array_mut()
                .unwrap()
                .extend(p.native.tools.clone());
        }
        if p.native.computer.is_some() {
            if !body["tools"].is_array() {
                body["tools"] = json!([]);
            }
            body["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"computer"}));
        }
        if p.kind == ProviderKind::Anthropic
            && let Some(container) = items
                .iter()
                .rev()
                .find_map(|v| v.get("container").filter(|v| v["id"].is_string()))
        {
            body["container"] = container["id"].clone();
        }
        if p.kind == ProviderKind::Gemini {
            if !p.native.response_modalities.is_empty() {
                body["generationConfig"]["responseModalities"] =
                    json!(p.native.response_modalities);
            }
            if let Some(v) = &p.native.image_config {
                body["generationConfig"]["imageConfig"] = v.clone();
            }
            if let Some(v) = &p.native.speech_config {
                body["generationConfig"]["speechConfig"] = v.clone();
            }
        }
        if let Some(schema) = &p.native.json_schema {
            match p.kind {
                ProviderKind::Openai | ProviderKind::AzureOpenai | ProviderKind::Vllm => {
                    body["text"] = json!({"format":{"type":"json_schema","name":"agent_output","schema":schema,"strict":true}})
                }
                ProviderKind::Anthropic => {
                    body["output_config"]["format"] = json!({"type":"json_schema","schema":schema})
                }
                ProviderKind::Gemini => {
                    body["generationConfig"]["responseMimeType"] = json!("application/json");
                    body["generationConfig"]["responseJsonSchema"] = schema.clone();
                }
                ProviderKind::Ollama => body["format"] = schema.clone(),
                _ => {
                    body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"agent_output","schema":schema,"strict":true}})
                }
            }
        }
        Ok((url, body))
    }
    pub async fn complete(
        &mut self,
        instructions: &str,
        items: &[Value],
        tools: &[Value],
        peer: &Peer,
        cancel: &CancellationToken,
        inbox: std::sync::Arc<crate::steering::Inbox>,
    ) -> Result<Value> {
        let mut result = self
            .complete_inner(instructions, items, tools, peer, cancel, inbox.clone())
            .await;
        if let Ok(response) = &mut result
            && !self.boundary_inputs.is_empty()
        {
            response["_steering_inputs"] = serde_json::to_value(&self.boundary_inputs)?;
            inbox.state(&self.boundary_inputs, "incorporated", None)?;
            inbox
                .receipts(&self.boundary_inputs, "incorporated", peer)
                .await?;
        }
        self.boundary_inputs.clear();
        result
    }
    async fn complete_inner(
        &mut self,
        instructions: &str,
        items: &[Value],
        tools: &[Value],
        peer: &Peer,
        cancel: &CancellationToken,
        inbox: std::sync::Arc<crate::steering::Inbox>,
    ) -> Result<Value> {
        let sid = inbox.sid.clone();
        let sid = sid.as_str();
        let p = &self.config.provider;
        if p.kind == ProviderKind::GithubCopilot {
            inbox.state(&self.boundary_inputs, "submitted", None)?;
            let response = crate::copilot::complete(
                &self.config,
                self.key
                    .as_deref()
                    .context("Configure the explicit Copilot credential")?,
                instructions,
                items,
                tools,
                cancel,
            )
            .await?;
            for item in response["output"].as_array().unwrap() {
                if let Some(content) = item["content"].as_array() {
                    for part in content {
                        peer.update(sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":part["text"]}})).await?;
                    }
                }
            }
            return Ok(response);
        }
        if p.kind == ProviderKind::Mock {
            let text=p.mock_response.as_deref().unwrap_or("Mock response: this is a deterministic test provider; no model or tools were called.");
            ensure!(!cancel.is_cancelled(), "Cancelled");
            peer.update(sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}})).await?;
            return Ok(
                json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],"usage":{"mock":true}}),
            );
        }
        let (url, body) = self.request(instructions, items, tools)?;
        if p.kind == ProviderKind::Openai {
            if self.websocket.is_none() {
                self.websocket = Some(
                    websocket::Client::start(self, peer.clone(), inbox, cancel.clone()).await?,
                );
            }
            return self.websocket.as_mut().unwrap().complete(body).await;
        }
        let request = self.headers(self.client.post(url)).await?.json(&body);
        inbox.state(&self.boundary_inputs, "submitted", None)?;
        let response = tokio::select! {_=cancel.cancelled()=>bail!("Cancelled"),r=request.send()=>r.context("Model endpoint could not be reached")?};
        ensure!(
            response.status().is_success(),
            "{} request failed (HTTP {}); check this provider's credential, endpoint, model and quota",
            p.kind.id(),
            response.status().as_u16()
        );
        let mut bytes = response.bytes_stream();
        let mut frames = Frames::default();
        let mut collector = Collector::new(p.kind);
        let ndjson = p.kind == ProviderKind::Ollama;
        loop {
            let next = tokio::select! {_=cancel.cancelled()=>bail!("Cancelled"),v=bytes.next()=>v};
            let Some(chunk) = next else { break };
            for data in frames.feed(&chunk.context("Model stream interrupted")?, ndjson)? {
                if data == "[DONE]" {
                    return collector.finish();
                }
                let event: Value =
                    serde_json::from_str(&data).context("Invalid model stream event")?;
                for text in collector.event(event)? {
                    if !text.is_empty() {
                        peer.update(sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}})).await?;
                    }
                }
                if collector.terminal() {
                    return collector.finish();
                }
            }
        }
        for data in frames.finish(ndjson)? {
            if data != "[DONE]" {
                for text in collector.event(serde_json::from_str(&data)?)? {
                    peer.update(sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}})).await?;
                }
            }
        }
        collector.finish()
    }
}

impl Provider {
    /// Read a provider file already cited in this session. Destinations are
    /// derived from the configured API origin, never from a model-supplied URL.
    pub async fn file(
        &self,
        items: &[Value],
        id: &str,
        container: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        fn cited(v: &Value, id: &str, container: Option<&str>) -> bool {
            if v["file_id"] == id
                && (container.is_none() || v["container_id"].as_str() == container)
            {
                return true;
            }
            match v {
                Value::Array(a) => a.iter().any(|v| cited(v, id, container)),
                Value::Object(o) => o.values().any(|v| cited(v, id, container)),
                _ => false,
            }
        }
        ensure!(
            items
                .iter()
                .filter(|v| v["role"] != "user")
                .any(|v| cited(v, id, container)),
            "File was not returned by the provider in this session"
        );
        ensure!(
            id.len() <= 200
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "Invalid provider file ID"
        );
        let mut url = reqwest::Url::parse(&self.config.provider.endpoint)?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid provider URL"))?;
            segments.pop_if_empty().pop();
            if let Some(c) = container {
                ensure!(
                    matches!(
                        self.config.provider.kind,
                        ProviderKind::Openai | ProviderKind::AzureOpenai
                    ),
                    "Container files require Responses"
                );
                ensure!(
                    !c.is_empty()
                        && c.len() <= 200
                        && c.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                    "Invalid container ID"
                );
                segments.push("containers").push(c);
            }
            segments.push("files").push(id).push("content");
        }
        let mut request = self.headers(self.client.get(url)).await?;
        if self.config.provider.kind == ProviderKind::Anthropic {
            request = request.header("anthropic-beta", "files-api-2025-04-14");
        }
        let work = async {
            let r = request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Provider file could not be reached"))?;
            ensure!(
                r.status().is_success(),
                "Provider file failed (HTTP {})",
                r.status().as_u16()
            );
            let mime = r
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            let mut stream = r.bytes_stream();
            let mut data = vec![];
            while let Some(c) = stream.next().await {
                let c = c?;
                ensure!(
                    data.len() + c.len() <= 8 * 1024 * 1024,
                    "Provider file exceeds 8 MiB limit"
                );
                data.extend(c);
            }
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let content = if matches!(
                mime.as_str(),
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            ) {
                json!({"type":"image","mimeType":mime,"data":b64})
            } else {
                json!({"type":"resource","resource":{"uri":format!("provider-file:///{id}"),"mimeType":mime,"blob":b64}})
            };
            Ok(json!({"file_id":id,"mime_type":mime,"bytes":data.len(),"_acp_content":[content]}))
        };
        tokio::select! {biased;_=cancel.cancelled()=>bail!("Provider file download cancelled"),r=work=>r}
    }
}
