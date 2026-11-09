//! Provider-owned model windows over an unchanged durable transcript.
//! A checkpoint is usable only for the exact original prefix and settings.
use super::{Provider, history};
use crate::{
    config::{ProviderAuth, ProviderKind},
    context::View,
    rpc::Peer,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct ContextRejection {
    status: u16,
    reason: &'static str,
}
impl std::fmt::Display for ContextRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Context service rejected request (HTTP {}, {}); history unchanged",
            self.status, self.reason
        )
    }
}
impl std::error::Error for ContextRejection {}

fn rejection_reason(body: &Value) -> &'static str {
    // Never copy arbitrary provider messages, parameters or codes into logs.
    match body["error"]["code"]
        .as_str()
        .or(body["error"]["type"].as_str())
    {
        Some("context_length_exceeded") => "context_length_exceeded",
        Some("rate_limit_exceeded" | "rate_limit_error") => "rate_limited",
        Some("invalid_api_key" | "authentication_error") => "authentication_failed",
        Some("invalid_request_error") => "invalid_request",
        _ => "provider_rejected",
    }
}

fn digest(value: &(impl serde::Serialize + ?Sized)) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

/// Token observations remain provider reported, never inferred from bytes.
pub(super) fn context_tokens(usage: &Value) -> u64 {
    usage["input_tokens"].as_u64().unwrap_or(0)
        + usage["output_tokens"].as_u64().unwrap_or(0)
        + usage["provider_usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0)
        + usage["provider_usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0)
}

fn restored(
    original: &[Value],
    checkpoints: Vec<Value>,
    settings: &str,
) -> Result<(Vec<Value>, Option<usize>, bool)> {
    for c in checkpoints {
        if c["kind"] != "provider_native" || c["settings_sha256"] != settings {
            continue;
        }
        let n = c["prefix_items"].as_u64().unwrap_or(u64::MAX) as usize;
        if n > original.len() || c["prefix_sha256"] != digest(&original[..n])? {
            continue;
        }
        if let Some(base) = c["model_items"].as_array() {
            let mut result = base.clone();
            result.extend_from_slice(&original[n..]);
            let baseline = crate::media::context_bytes(base)?;
            let newer_server_window = original[n..].iter().any(|v| v["type"] == "compaction");
            return Ok((
                server_window(&original[n..], result),
                (!newer_server_window).then_some(baseline),
                true,
            ));
        }
    }
    Ok((
        server_window(original, original.to_vec()),
        None,
        original.iter().any(|v| v["type"] == "compaction"),
    ))
}

/// Only server compaction items in the durable transcript can prune a prefix.
/// Standalone compact output is a canonical window, kept intact in a checkpoint.
fn server_window(original: &[Value], base: Vec<Value>) -> Vec<Value> {
    if let Some(index) = original.iter().rposition(|v| v["type"] == "compaction") {
        return original[index..].to_vec();
    }
    base
}

impl Provider {
    pub(crate) fn native_context(&self) -> bool {
        self.config.context.auto_compact
            && self.config.context.provider_compaction
            && matches!(
                self.config.provider.kind,
                ProviderKind::Openai | ProviderKind::Anthropic
            )
            && self.config.provider.auth != ProviderAuth::Chatgpt
    }

    pub(crate) async fn prepare_context(
        &mut self,
        original: &[Value],
        instructions: &str,
        tools: &[Value],
        scope: (&Store, &str, &str, usize),
        peer: &Peer,
        cancel: &CancellationToken,
    ) -> Result<View> {
        let (store, sid, turn, active_turn_start) = scope;
        let fixed = instructions.len() + serde_json::to_vec(tools)?.len();
        let budget = self
            .config
            .limits
            .max_context_bytes
            .checked_sub(fixed)
            .context("Instructions and tools exceed context budget")?;
        if !self.native_context() {
            return crate::context::prepare(
                original,
                &self.config.context,
                budget,
                active_turn_start,
            );
        }
        let settings = digest(&json!({"instructions":instructions,"tools":tools}))?;
        let provider = self.config.provider.fingerprint();
        let (mut items, compacted_bytes, native_window) = restored(
            original,
            store.provider_checkpoints(sid, &provider)?,
            &settings,
        )?;
        // An existing transcript may be much larger than a provider's input
        // window. Native compaction cannot ingest an unlimited archive. Keep
        // the established bounded projection until a native checkpoint exists,
        // including producer-declared refreshed snapshots. Never reinterpret
        // or prune a canonical native window through the extractive fallback.
        let mut checkpoint = None;
        if !native_window {
            let view =
                crate::context::prepare(original, &self.config.context, budget, active_turn_start)?;
            items = view.items;
            checkpoint = view.checkpoint;
        }
        let bytes = crate::media::context_bytes(&items)?;
        // A canonical window can retain large user messages. Recompacting the
        // same window on every new prompt adds latency without reclaiming it.
        // Wait for bounded material growth, unless the hard byte limit requires
        // another attempt. This is byte-pressure hysteresis, not token estimation.
        let grew =
            compacted_bytes.is_none_or(|baseline| bytes > baseline.saturating_add(budget / 8));
        let due = bytes > budget
            || (grew
                && (bytes > budget * 3 / 4
                    || self.context_tokens >= self.config.context.compact_threshold_tokens as u64));
        // Native automatic compaction and steering cannot currently coexist.
        // Claim a settled socket boundary before /compact. New inputs remain
        // durable/local during maintenance, and accepted steering is never
        // cancelled or replayed to obtain a smaller model window.
        let can_compact = if due && self.config.provider.kind == ProviderKind::Openai {
            match self.websocket.as_mut() {
                Some(socket) => socket.begin_compaction().await?,
                None => true,
            }
        } else {
            true
        };
        if due && can_compact {
            ensure!(
                self.compactions < self.config.context.max_compactions_per_turn,
                "Native compaction request budget exhausted; full history retained"
            );
            self.compactions += 1;
            let id = format!("context-{}", crate::store::id());
            peer.compaction(sid, &id, "in_progress").await?;
            let started = std::time::Instant::now();
            store.event(sid,Some(turn),"context.compaction.started",json!({"provider":self.config.provider.kind,"input_bytes":bytes,"observed_context_tokens":self.context_tokens,"attempt":self.compactions}))?;
            let result = self.compact(instructions, &items, tools, cancel).await;
            peer.compaction(
                sid,
                &id,
                if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
            )
            .await?;
            let (window, usage) = match result {
                Ok(v) => v,
                Err(e) => {
                    let rejection = e.downcast_ref::<ContextRejection>();
                    store.event(sid,Some(turn),"context.compaction.failed",json!({"provider":self.config.provider.kind,"elapsed_ms":started.elapsed().as_millis(),"full_history_preserved":true,"http_status":rejection.map(|v|v.status),"reason":rejection.map(|v|v.reason).unwrap_or("context_maintenance_failed")}))?;
                    return Err(e);
                }
            };
            let native_checkpoint = json!({"kind":"provider_native","prefix_items":original.len(),"prefix_sha256":digest(&original)?,"settings_sha256":settings,"model_items":window});
            store.checkpoint(sid, &provider, &native_checkpoint)?;
            checkpoint = None;
            items = window;
            self.context_tokens = 0;
            store.event(sid,Some(turn),"context.compacted",json!({"kind":"provider_native","input_bytes":bytes,"model_context_bytes":serde_json::to_vec(&items)?.len(),"elapsed_ms":started.elapsed().as_millis(),"usage":usage,"full_history_preserved":true}))?;
        }
        ensure!(
            crate::media::context_bytes(&items)? <= budget && crate::media::request_fits(&items)?,
            "Provider model window exceeds byte limit; full history retained"
        );
        Ok(View { items, checkpoint })
    }

    async fn bounded_json(
        &self,
        request: reqwest::RequestBuilder,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let work = async {
            let response = request
                .send()
                .await
                .context("Context service unavailable")?;
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(Ok(part)) = stream.next().await {
                    if bytes.len() + part.len() > 8192 {
                        break;
                    }
                    bytes.extend_from_slice(&part);
                }
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                return Err(ContextRejection {
                    status,
                    reason: rejection_reason(&body),
                }
                .into());
            }
            let streaming = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("text/event-stream"));
            let mut stream = response.bytes_stream();
            if streaming {
                let mut frames = super::stream::Frames::default();
                let mut message = json!({"content":[]});
                let mut ended = false;
                let mut total = 0usize;
                while let Some(part) = stream.next().await {
                    let part = part.context("Compaction stream interrupted")?;
                    total += part.len();
                    ensure!(
                        total <= 16 * 1024 * 1024,
                        "Compaction stream exceeded bounds; history unchanged"
                    );
                    for data in frames.feed(&part, false)? {
                        let event: Value = serde_json::from_str(&data)?;
                        match event["type"].as_str().unwrap_or("") {
                            "message_start" => {
                                message = event["message"].clone();
                                message["content"] = json!([]);
                            }
                            "content_block_start" => {
                                ensure!(
                                    event["content_block"]["type"] == "compaction",
                                    "Unexpected content during compaction"
                                );
                                message["content"]
                                    .as_array_mut()
                                    .context("Compaction content missing")?
                                    .push(event["content_block"].clone());
                            }
                            "content_block_delta" => {
                                anyhow::bail!("Unexpected partial compaction block")
                            }
                            "message_delta" => {
                                message["stop_reason"] = event["delta"]["stop_reason"].clone();
                                if let Some(usage) = event["usage"].as_object() {
                                    for (k, v) in usage {
                                        message["usage"][k] = v.clone();
                                    }
                                }
                            }
                            "message_stop" => ended = true,
                            "error" => {
                                anyhow::bail!("Provider compaction failed; history unchanged")
                            }
                            _ => (),
                        }
                    }
                    if ended {
                        return Ok(message);
                    }
                }
                anyhow::bail!(
                    "Compaction stream closed without terminal settlement; history unchanged"
                );
            }
            let mut bytes = vec![];
            while let Some(part) = stream.next().await {
                let part = part.context("Context service stream interrupted")?;
                ensure!(
                    bytes.len() + part.len() <= 16 * 1024 * 1024,
                    "Context service response too large"
                );
                bytes.extend(part);
            }
            Ok(serde_json::from_slice(&bytes)?)
        };
        tokio::select! {_=cancel.cancelled()=>anyhow::bail!("Context maintenance cancelled; history unchanged"),r=work=>r}
    }

    async fn compact(
        &self,
        instructions: &str,
        items: &[Value],
        tools: &[Value],
        cancel: &CancellationToken,
    ) -> Result<(Vec<Value>, Value)> {
        let p = &self.config.provider;
        if p.kind == ProviderKind::Openai {
            let url = format!("{}/compact", p.endpoint.trim_end_matches('/'));
            let request=self.headers(self.client.post(url)).await?.json(&json!({"model":p.model,"instructions":instructions,"input":history::responses(items)}));
            let response = self.bounded_json(request, cancel).await?;
            let output = response["output"]
                .as_array()
                .context("Compaction response has no canonical window")?;
            ensure!(
                output
                    .iter()
                    .any(|i| i["type"] == "compaction" && i["encrypted_content"].is_string()),
                "Compaction did not return its opaque continuation; history unchanged"
            );
            return Ok((output.clone(), response["usage"].clone()));
        }
        // Feature negotiation is account/route-specific. A model name is not
        // evidence that this endpoint supports the beta signed-block protocol.
        let mut url = reqwest::Url::parse(&p.endpoint)?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid model endpoint"))?
            .pop_if_empty()
            .pop()
            .push("models")
            .push(&p.model);
        let model = self
            .bounded_json(self.headers(self.client.get(url)).await?, cancel)
            .await?;
        ensure!(
            model["capabilities"]["compaction"]["summarize"]["supported"] == true,
            "This model route does not advertise native compaction; full history retained"
        );
        let (_, mut body) = self.request(instructions, items, tools)?;
        body["stream"] = json!(true);
        body["compaction"] = json!({"type":"summarize"});
        body.as_object_mut().unwrap().remove("tool_choice");
        if let Some(o) = body.get_mut("output_config").and_then(Value::as_object_mut) {
            o.remove("format");
        }
        let response = self
            .bounded_json(
                self.headers(self.client.post(&p.endpoint))
                    .await?
                    .json(&body),
                cancel,
            )
            .await?;
        let content = response["content"]
            .as_array()
            .context("Compaction content missing")?;
        ensure!(
            response["stop_reason"] == "compaction"
                && content.len() == 1
                && content[0]["type"] == "compaction"
                && content[0]["signature"].is_string(),
            "Compaction did not settle with one signed block; history unchanged"
        );
        Ok((
            vec![
                json!({"type":"provider_state","provider":"anthropic","message":{"role":"assistant","content":content}}),
            ],
            response["usage"].clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_never_duplicates_or_discards_unsummarized_tail() {
        let original = vec![
            json!({"role":"user","content":"old"}),
            json!({"role":"user","content":"fresh"}),
        ];
        let canonical = vec![
            json!({"role":"user","content":"retained"}),
            json!({"type":"compaction","encrypted_content":"opaque"}),
        ];
        let cp = json!({"kind":"provider_native","prefix_items":1,"prefix_sha256":digest(&original[..1]).unwrap(),"settings_sha256":"settings","model_items":canonical});
        let view = restored(&original, vec![cp.clone()], "settings").unwrap();
        assert_eq!(
            view.0,
            vec![
                canonical[0].clone(),
                canonical[1].clone(),
                original[1].clone()
            ]
        );
        assert_eq!(
            restored(&original, vec![cp.clone()], "changed-tools")
                .unwrap()
                .0,
            original
        );
        let changed = vec![json!({"role":"user","content":"different"})];
        assert_eq!(restored(&changed, vec![cp], "settings").unwrap().0, changed);
    }
}
