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

fn restored(original: &[Value], checkpoints: Vec<Value>, settings: &str) -> Result<Vec<Value>> {
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
            return Ok(server_window(&original[n..], result));
        }
    }
    Ok(server_window(original, original.to_vec()))
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
        scope: (&Store, &str, &str),
        peer: &Peer,
        cancel: &CancellationToken,
    ) -> Result<View> {
        let (store, sid, turn) = scope;
        let fixed = instructions.len() + serde_json::to_vec(tools)?.len();
        let budget = self
            .config
            .limits
            .max_context_bytes
            .checked_sub(fixed)
            .context("Instructions and tools exceed context budget")?;
        if !self.native_context() {
            return crate::context::prepare(original, &self.config.context, budget);
        }
        let settings = digest(&json!({"instructions":instructions,"tools":tools}))?;
        let provider = self.config.provider.fingerprint();
        let mut items = restored(
            original,
            store.provider_checkpoints(sid, &provider)?,
            &settings,
        )?;
        let bytes = crate::media::context_bytes(&items)?;
        let due = bytes > budget * 3 / 4
            || self.context_tokens >= self.config.context.compact_threshold_tokens as u64;
        // OpenAI compacts active chains on the server. Never replace an active
        // chain while a steering successor might already be executing.
        let can_compact =
            self.config.provider.kind != ProviderKind::Openai || self.websocket.is_none();
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
                    store.event(sid,Some(turn),"context.compaction.failed",json!({"provider":self.config.provider.kind,"elapsed_ms":started.elapsed().as_millis(),"full_history_preserved":true}))?;
                    return Err(e);
                }
            };
            let checkpoint = json!({"kind":"provider_native","prefix_items":original.len(),"prefix_sha256":digest(&original)?,"settings_sha256":settings,"model_items":window});
            store.checkpoint(sid, &provider, &checkpoint)?;
            items = window;
            self.context_tokens = 0;
            store.event(sid,Some(turn),"context.compacted",json!({"kind":"provider_native","input_bytes":bytes,"model_context_bytes":serde_json::to_vec(&items)?.len(),"elapsed_ms":started.elapsed().as_millis(),"usage":usage,"full_history_preserved":true}))?;
        }
        ensure!(
            crate::media::context_bytes(&items)? <= budget && crate::media::request_fits(&items)?,
            "Provider model window exceeds byte limit; full history retained"
        );
        Ok(View {
            items,
            checkpoint: None,
        })
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
            ensure!(
                response.status().is_success(),
                "Context service rejected request (HTTP {}); history unchanged",
                response.status().as_u16()
            );
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
        if let Some(o) = body["output_config"].as_object_mut() {
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
            view,
            vec![
                canonical[0].clone(),
                canonical[1].clone(),
                original[1].clone()
            ]
        );
        assert_eq!(
            restored(&original, vec![cp.clone()], "changed-tools").unwrap(),
            original
        );
        let changed = vec![json!({"role":"user","content":"different"})];
        assert_eq!(restored(&changed, vec![cp], "settings").unwrap(), changed);
    }
}
