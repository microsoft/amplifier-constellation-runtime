//! Provider-owned model windows over an unchanged durable transcript.
//! A checkpoint is usable only for the exact original prefix and settings.
use super::{Provider, history, rejection};
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

/// What the saved checkpoints leave for one request. `window` is a provider's
/// canonical output and is sent as it was returned. `tail` is the saved items
/// after it, which this runtime still owns.
struct Restored {
    window: Vec<Value>,
    tail: Vec<Value>,
    compacted_bytes: Option<usize>,
    native: bool,
}
impl Restored {
    /// The window, then the tail with producer-declared snapshots refreshed.
    /// A canonical window is never reinterpreted; without this projection every
    /// prompt saved after it would stay whole until the next compaction.
    /// `pending` is input sent with this request and saved after it. Projecting
    /// it here gives the next request the same prefix this one sends.
    fn view(&self, pending: &[Value]) -> Vec<Value> {
        let mut tail = self.tail.clone();
        tail.extend_from_slice(pending);
        crate::context::project_snapshots(&mut tail);
        let mut view = self.window.clone();
        view.extend(tail);
        view
    }
}

/// A projected transcript with the input waiting to be sent after it. That
/// input is saved once the request completes, and the next request projects
/// it with the transcript. Projecting it here gives both the same prefix: a
/// snapshot it refreshes is replaced from the request that carries it.
fn with_pending(mut items: Vec<Value>, pending: &[Value]) -> Vec<Value> {
    if !pending.is_empty() {
        items.extend_from_slice(pending);
        crate::context::project_snapshots(&mut items);
    }
    items
}

fn restored(original: &[Value], checkpoints: Vec<Value>, settings: &str) -> Result<Restored> {
    // Only server compaction items in the durable transcript can prune a
    // prefix. Standalone compact output is a canonical window, kept intact in
    // a checkpoint.
    let server_window = |items: &[Value]| {
        let index = items.iter().rposition(|v| v["type"] == "compaction")?;
        Some(Restored {
            window: vec![],
            tail: items[index..].to_vec(),
            compacted_bytes: None,
            native: true,
        })
    };
    for c in checkpoints {
        if c["kind"] != "provider_native" || c["settings_sha256"] != settings {
            continue;
        }
        let n = c["prefix_items"].as_u64().unwrap_or(u64::MAX) as usize;
        if n > original.len() || c["prefix_sha256"] != digest(&original[..n])? {
            continue;
        }
        if let Some(base) = c["model_items"].as_array() {
            if let Some(newer) = server_window(&original[n..]) {
                return Ok(newer);
            }
            return Ok(Restored {
                window: base.clone(),
                tail: original[n..].to_vec(),
                compacted_bytes: Some(crate::media::context_bytes(&crate::mcp::model_view(base))?),
                native: true,
            });
        }
    }
    Ok(server_window(original).unwrap_or(Restored {
        window: vec![],
        tail: original.to_vec(),
        compacted_bytes: None,
        native: false,
    }))
}

/// The provider refuses a request whose final assistant message ends on a
/// reasoning block. A reply with reasoning and no text ends that way, and the
/// window ends on it whenever maintenance runs before the next input is added.
/// The summary request leaves that trailing reasoning out: it has no public
/// content. Saved history and later model requests keep it unchanged.
fn drop_trailing_reasoning(messages: &mut Vec<Value>) {
    let Some(last) = messages.last_mut().filter(|m| m["role"] == "assistant") else {
        return;
    };
    let Some(content) = last["content"].as_array_mut() else {
        return;
    };
    while content
        .last()
        .is_some_and(|b| b["type"] == "thinking" || b["type"] == "redacted_thinking")
    {
        content.pop();
    }
    if content.is_empty() {
        messages.pop();
    }
}

impl Provider {
    /// Serialize context maintenance or a local capacity stop with the socket's
    /// steering owner. Accepted successors must settle before the engine ends
    /// the turn; a successful claim holds new inputs locally.
    pub(crate) async fn claim_context_boundary(&mut self) -> Result<bool> {
        match self.websocket.as_mut() {
            Some(socket) => socket.begin_compaction().await,
            None => Ok(true),
        }
    }

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
        // Input waiting at this boundary is sent with the request and saved
        // only once the provider completes it. It is never summary source.
        let pending: Vec<Value> = self
            .boundary_inputs
            .iter()
            .map(|input| input.item.clone())
            .collect();
        // Anthropic ties a reasoning block to the instructions, the tools and
        // every earlier message. Each saved reply records what it followed;
        // reasoning this view would send after anything else is left out.
        // A request that cannot be built fails when it is sent, not here.
        let follows = (self.config.provider.kind == ProviderKind::Anthropic)
            .then(|| self.request_settings(instructions, tools).ok())
            .flatten();
        let bound = |items: &mut Vec<Value>| {
            if let Some(settings) = &follows {
                history::keep_bound_reasoning(settings, items);
            }
        };
        if !self.native_context() {
            let mut view =
                crate::context::prepare(original, &self.config.context, budget, active_turn_start)?;
            view.items = with_pending(view.items, &pending);
            bound(&mut view.items);
            return Ok(view);
        }
        let settings = digest(&json!({"instructions":instructions,"tools":tools}))?;
        let provider = self.config.provider.fingerprint();
        let mut source = restored(
            original,
            store.provider_checkpoints(sid, &provider)?,
            &settings,
        )?;
        let compacted_bytes = source.compacted_bytes;
        // An existing transcript may be much larger than a provider's input
        // window. Native compaction cannot ingest an unlimited archive. Keep
        // the established bounded projection until a native checkpoint exists,
        // including producer-declared refreshed snapshots. Never reinterpret
        // or prune a canonical native window through the extractive fallback.
        let mut checkpoint = None;
        let mut items = if source.native {
            source.view(&[])
        } else {
            let view =
                crate::context::prepare(original, &self.config.context, budget, active_turn_start)?;
            checkpoint = view.checkpoint;
            view.items
        };
        // Count the same lossless client-tool view that provider serialization
        // uses. The stored native checkpoint and opaque provider records remain
        // unchanged; only correlated redundant MCP fallbacks can be omitted.
        items = crate::mcp::model_view(&items);
        // Before maintenance too: a summary request carries the same blocks.
        bound(&mut items);
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
            self.claim_context_boundary().await?
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
            items = crate::mcp::model_view(&window);
            source = Restored {
                window,
                tail: vec![],
                compacted_bytes: None,
                native: true,
            };
            self.context_tokens = 0;
            store.event(sid,Some(turn),"context.compacted",json!({"kind":"provider_native","input_bytes":bytes,"model_context_bytes":serde_json::to_vec(&items)?.len(),"elapsed_ms":started.elapsed().as_millis(),"usage":usage,"full_history_preserved":true}))?;
        }
        ensure!(
            crate::media::context_bytes(&items)? <= budget && crate::media::request_fits(&items)?,
            "Provider model window exceeds byte limit; full history retained"
        );
        if !pending.is_empty() {
            items = if source.native {
                crate::mcp::model_view(&source.view(&pending))
            } else {
                with_pending(items, &pending)
            };
            bound(&mut items);
        }
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
                let refusal = rejection::Refusal::read(response).await;
                return Err(ContextRejection {
                    status,
                    reason: refusal.reason().unwrap_or("provider_rejected"),
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
        if let Some(messages) = body["messages"].as_array_mut() {
            drop_trailing_reasoning(messages);
        }
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
        assert!(view.native);
        assert_eq!(
            view.view(&[]),
            vec![
                canonical[0].clone(),
                canonical[1].clone(),
                original[1].clone()
            ]
        );
        let unbound = restored(&original, vec![cp.clone()], "changed-tools").unwrap();
        assert!(!unbound.native);
        assert_eq!(unbound.view(&[]), original);
        let changed = vec![json!({"role":"user","content":"different"})];
        let unbound = restored(&changed, vec![cp], "settings").unwrap();
        assert!(!unbound.native);
        assert_eq!(unbound.view(&[]), changed);
    }
    #[test]
    fn saved_items_after_a_window_refresh_snapshots_and_the_window_does_not() {
        use crate::context::SNAPSHOT_META;
        let prompt = |text: &str, retained: &str| json!({"role":"user","content":text,"_acp_prompt":[{"type":"text","text":text,"_meta":{SNAPSHOT_META:{"version":1,"key":"room","retainedText":retained}}}]});
        let replaced = |retained: &str| {
            format!(
                "Historical request/context (the refreshed projection is supplied later; original text remains in session_history):\n{retained}"
            )
        };
        let reply = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Finished"}]});
        let original = vec![
            prompt("SUMMARIZED ROOM", "summarized request"),
            reply.clone(),
            prompt("SECOND ROOM", "second request"),
            reply.clone(),
            prompt("THIRD ROOM", "third request"),
        ];
        // A window is the provider's own output. Even an item in it that looks
        // like a declared snapshot is sent exactly as it was returned.
        let canonical = vec![
            prompt("RETAINED BY PROVIDER", "never applied"),
            json!({"type":"compaction","encrypted_content":"opaque"}),
        ];
        let cp = json!({"kind":"provider_native","prefix_items":1,"prefix_sha256":digest(&original[..1]).unwrap(),"settings_sha256":"settings","model_items":canonical});
        let restored_window = restored(&original, vec![cp.clone()], "settings").unwrap();
        let view = restored_window.view(&[]);
        assert_eq!(view[..2], canonical[..]);
        assert_eq!(view[2], reply);
        assert_eq!(view[3]["content"], replaced("second request"));
        assert_eq!(view[3]["_acp_prompt"], original[2]["_acp_prompt"]);
        assert_eq!(view[4], reply);
        assert_eq!(view[5], original[4]);
        assert_eq!(
            restored_window.compacted_bytes,
            Some(crate::media::context_bytes(&canonical).unwrap())
        );
        // Input waiting at the boundary is the newest version before it is saved.
        let waiting = prompt("WAITING ROOM", "waiting request");
        let view = restored_window.view(std::slice::from_ref(&waiting));
        assert_eq!(view[..2], canonical[..]);
        assert_eq!(view[3]["content"], replaced("second request"));
        assert_eq!(view[5]["content"], replaced("third request"));
        assert_eq!(view[6], waiting);
        // Once that input is saved, the same prefix is rebuilt from history.
        let mut saved = original.clone();
        saved.extend([waiting.clone(), reply.clone()]);
        let next = restored(&saved, vec![cp], "settings").unwrap().view(&[]);
        assert_eq!(next[..7], view[..]);
        // A server compaction item saved after the checkpoint starts the window.
        let mut pruned = original[..2].to_vec();
        pruned.push(json!({"type":"compaction","encrypted_content":"newer"}));
        pruned.extend(original[2..].iter().cloned());
        let cp = json!({"kind":"provider_native","prefix_items":1,"prefix_sha256":digest(&pruned[..1]).unwrap(),"settings_sha256":"settings","model_items":canonical});
        let newer = restored(&pruned, vec![cp], "settings").unwrap();
        assert_eq!(newer.compacted_bytes, None);
        let view = newer.view(&[]);
        assert_eq!(view[0], pruned[2]);
        assert_eq!(view[1]["content"], replaced("second request"));
        assert_eq!(view[3], original[4]);
        assert_eq!(view.len(), 4);
    }
    #[test]
    fn waiting_input_is_projected_with_the_transcript_it_is_saved_after() {
        use crate::context::{SNAPSHOT_META, refreshed_view};
        let prompt = |text: &str, retained: &str| json!({"role":"user","content":text,"_acp_prompt":[{"type":"text","text":text,"_meta":{SNAPSHOT_META:{"version":1,"key":"room","retainedText":retained}}}]});
        let call = json!({"type":"tool_use","id":"a","name":"clock","input":{}});
        let transcript = vec![
            prompt("FIRST ROOM", "first request"),
            json!({"type":"provider_state","provider":"anthropic","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"s"}, call]}}),
            json!({"type":"function_call","call_id":"a","name":"clock","arguments":"{}","_provider_shadow":true}),
            json!({"type":"function_call_output","call_id":"a","output":"12:00"}),
        ];
        // Alone, the transcript's only snapshot is its newest.
        let projected = refreshed_view(&transcript);
        assert_eq!(projected, transcript);
        assert_eq!(with_pending(projected.clone(), &[]), transcript);
        let waiting = prompt("WAITING ROOM", "waiting request");
        let view = with_pending(projected, std::slice::from_ref(&waiting));
        assert_eq!(
            view[0]["content"],
            "Historical request/context (the refreshed projection is supplied later; original text remains in session_history):\nfirst request"
        );
        // The reply followed the whole prompt.
        assert_eq!(view[1]["message"]["content"], json!([call]));
        assert_eq!(view[2..4], transcript[2..]);
        assert_eq!(view[4], waiting);
        // Once the input is saved, the transcript projects to the same items.
        let mut saved = transcript.clone();
        saved.push(waiting);
        assert_eq!(refreshed_view(&saved), view);
    }
    #[test]
    fn summary_request_never_ends_on_reasoning() {
        let user = json!({"role":"user","content":[{"type":"text","text":"overheard"}]});
        let thinking = json!({"type":"thinking","thinking":"","signature":"s"});
        let text = json!({"type":"text","text":"said"});
        let call = json!({"type":"tool_use","id":"t","name":"n","input":{}});
        // A reply with reasoning only: the whole message goes.
        let mut m = vec![
            user.clone(),
            json!({"role":"assistant","content":[thinking.clone(), json!({"type":"redacted_thinking","data":"d"})]}),
        ];
        drop_trailing_reasoning(&mut m);
        assert_eq!(m, vec![user.clone()]);
        // Reasoning before text or a tool call is not at the end and stays.
        for kept in [
            vec![thinking.clone(), text.clone()],
            vec![thinking.clone(), call],
        ] {
            let original = vec![user.clone(), json!({"role":"assistant","content":kept})];
            let mut m = original.clone();
            drop_trailing_reasoning(&mut m);
            assert_eq!(m, original);
        }
        // Only reasoning after the last public block goes.
        let mut m = vec![
            user.clone(),
            json!({"role":"assistant","content":[text.clone(), thinking.clone()]}),
        ];
        drop_trailing_reasoning(&mut m);
        assert_eq!(
            m,
            vec![user.clone(), json!({"role":"assistant","content":[text]})]
        );
        // Reasoning in an earlier assistant message is untouched when a user message ends the window.
        let original = vec![
            user.clone(),
            json!({"role":"assistant","content":[thinking]}),
            user,
        ];
        let mut m = original.clone();
        drop_trailing_reasoning(&mut m);
        assert_eq!(m, original);
    }
}
