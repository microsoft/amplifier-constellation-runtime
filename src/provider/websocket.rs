//! One connection and response chain per logical turn. The reader remains alive
//! during local tools/approvals. It never retries a response or a tool effect.
use super::{Provider, history};
use crate::{
    rpc::Peer,
    steering::{Inbox, Input},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    Connector,
    tungstenite::{Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Request {
    body: Value,
    continuation_items: Vec<Value>,
    reply: oneshot::Sender<Result<Value>>,
}
enum Command {
    Response(Request),
    BeginCompaction(oneshot::Sender<bool>),
}
pub struct Client {
    requests: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Client {
    pub async fn start(
        provider: &Provider,
        peer: Peer,
        inbox: Arc<Inbox>,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let mut url = reqwest::Url::parse(&provider.config.provider.endpoint)?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| anyhow::anyhow!("Invalid WebSocket URL"))?;
        let mut request = url.as_str().into_client_request()?;
        let auth = provider
            .headers(provider.client.get(&provider.config.provider.endpoint))
            .await?
            .build()?;
        for (name, value) in auth.headers() {
            request.headers_mut().insert(name, value.clone());
        }
        let connector = if let Some(path) = &provider.config.provider.tls_ca_file {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let pem = std::fs::read(path)?;
            for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
                roots.add(cert?)?;
            }
            Some(Connector::Rustls(Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )))
        } else {
            None
        };
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(24 * 1024 * 1024))
            .max_frame_size(Some(24 * 1024 * 1024));
        let (socket, _) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async_tls_with_config(
                request.clone(),
                Some(config),
                false,
                connector.clone(),
            ),
        )
        .await
        .context("Model WebSocket connection timed out")?
        .map_err(|_| {
            anyhow::anyhow!(
                "Model WebSocket connection failed; check endpoint, access and proxy support"
            )
        })?;
        let (tx, rx) = mpsc::channel(1);
        let max = provider.config.limits.max_model_calls;
        let provider_context_limit = provider.config.limits.max_context_bytes;
        let idle = std::time::Duration::from_secs(provider.config.limits.provider_idle_seconds);
        let task = tokio::spawn(async move {
            let mut driver = Driver {
                socket,
                connection_request: request,
                connector,
                opened_at: tokio::time::Instant::now(),
                requests: rx,
                peer,
                inbox: inbox.clone(),
                cancel,
                waiter: None,
                ready: VecDeque::new(),
                history: vec![],
                window_start: 0,
                id: None,
                running: false,
                awaiting_created: false,
                auto_wait: false,
                needs_tools: false,
                pending: None,
                inputs: vec![],
                created: 0,
                max,
                total_bytes: 0,
                context_limit: provider_context_limit,
                fixed_bytes: 0,
                inline_inputs: vec![],
                messages: super::identity::Messages::default(),
                seen_created: HashMap::new(),
                seen_completed: HashMap::new(),
                seen_accepted: HashMap::new(),
                failure_stage: "connect",
                failure_category: "transport",
                idle,
                maintenance: false,
            };
            if let Err(e) = driver.run().await {
                // Provider bodies, source text, URLs and credentials never enter
                // failure telemetry. Record where the chain stopped, even if
                // it failed between engine requests while a tool was running.
                let _ = inbox.store.event(&inbox.sid, Some(&inbox.turn), "model.failed", json!({
                    "stage":driver.failure_stage,"category":driver.failure_category,
                    "response_id":driver.id,"pending_inputs":driver.pending.as_ref().map_or(0,|p|p.inputs.len()),
                    "steering_accepted":driver.pending.as_ref().is_some_and(|p|p.remote_id.is_some()),
                    "awaiting_created":driver.awaiting_created,"needs_tools":driver.needs_tools,
                    "code":failure_code(&e),
                }));
                let _ = inbox.close("transport_or_provider_failure");
                let _ = inbox.publish_summary(&driver.peer).await;
                if let Some(reply) = driver.waiter.take() {
                    let _ = reply.send(Err(e));
                }
            }
        });
        Ok(Self { requests: tx, task })
    }
    pub async fn complete(&mut self, body: Value, continuation_items: Vec<Value>) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Command::Response(Request {
                body,
                continuation_items,
                reply: tx,
            }))
            .await
            .context("Model WebSocket closed; saved input receipts remain available for review")?;
        rx.await
            .context("Model WebSocket interrupted; no response or tool was replayed")?
    }
    /// The socket owner serializes this boundary with steering delivery. A
    /// successful claim holds newly arriving inputs locally until the next
    /// explicit response, while preserving this turn's response-call budget.
    pub async fn begin_compaction(&mut self) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Command::BeginCompaction(tx))
            .await
            .context("Model connection closed before context maintenance")?;
        rx.await
            .context("Model connection interrupted before context maintenance")
    }
}
struct Pending {
    inputs: Vec<Input>,
    parent: String,
    remote_id: Option<String>,
}
struct Driver {
    idle: std::time::Duration,
    failure_stage: &'static str,
    failure_category: &'static str,
    messages: super::identity::Messages,
    socket: Socket,
    connection_request: tokio_tungstenite::tungstenite::http::Request<()>,
    connector: Option<Connector>,
    opened_at: tokio::time::Instant,
    requests: mpsc::Receiver<Command>,
    peer: Peer,
    inbox: Arc<Inbox>,
    cancel: CancellationToken,
    waiter: Option<oneshot::Sender<Result<Value>>>,
    ready: VecDeque<Value>,
    history: Vec<Value>,
    window_start: usize,
    id: Option<String>,
    running: bool,
    awaiting_created: bool,
    auto_wait: bool,
    needs_tools: bool,
    pending: Option<Pending>,
    inputs: Vec<Input>,
    created: usize,
    max: usize,
    total_bytes: usize,
    context_limit: usize,
    fixed_bytes: usize,
    inline_inputs: Vec<Input>,
    seen_created: HashMap<String, Option<String>>,
    seen_completed: HashMap<String, Value>,
    seen_accepted: HashMap<String, String>,
    maintenance: bool,
}
fn failure_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "Active model chain reached its context budget while steering is pending; no work was replayed" => {
            "pending_context_capacity"
        }
        "Only one engine response request may be outstanding" => "overlapping_response_requests",
        "Unexpected response completion" | "Unexpected steering successor" => {
            "response_identity_mismatch"
        }
        "Successor arrived without steering acknowledgment; input outcome is unknown" => {
            "missing_steering_acknowledgment"
        }
        "Model WebSocket reported failure; check model, quota and supported settings; no work was replayed" => {
            "provider_reported_failure"
        }
        "Model WebSocket closed; no pending input was replayed" => "connection_closed",
        "Logical turn model-call budget exhausted" => "model_call_budget",
        "Model WebSocket stopped answering; no pending input was replayed" => "transport_idle",
        "Model rejected steering; input retained, not retried automatically" => "steering_rejected",
        _ => "request_failed",
    }
}
impl Driver {
    async fn send(&mut self, value: Value) -> Result<()> {
        tokio::time::timeout(
            self.idle,
            self.socket.send(Message::Text(value.to_string().into())),
        )
        .await
        .context("Model WebSocket send timed out; outcome may be unknown")?
        .context("Model WebSocket send failed; outcome may be unknown")
    }
    async fn fits(&self, inputs: &[Input], history: &[Value]) -> Result<bool> {
        let mut all = history.to_vec();
        all.extend(history::responses(
            &inputs.iter().map(|i| i.item.clone()).collect::<Vec<_>>(),
        ));
        if serde_json::to_vec(&all)?.len() + self.fixed_bytes > self.context_limit {
            self.inbox
                .state(inputs, "notAccepted", Some("context_capacity"))?;
            self.inbox
                .receipts(inputs, "notAccepted", &self.peer)
                .await?;
            return Ok(false);
        }
        Ok(true)
    }
    async fn steer(&mut self) -> Result<()> {
        if self.maintenance
            || self.pending.is_some()
            || self.auto_wait
            || self.awaiting_created
            || self.created >= self.max
        {
            return Ok(());
        }
        let Some(parent) = self.id.clone() else {
            return Ok(());
        };
        let inputs = self.inbox.queued()?;
        if inputs.is_empty() {
            return Ok(());
        }
        if !self
            .fits(&inputs, &self.history[self.window_start..])
            .await?
        {
            return Ok(());
        }
        let wire = history::responses(&inputs.iter().map(|i| i.item.clone()).collect::<Vec<_>>());
        self.inbox.remote(&inputs, &parent, None, None)?;
        self.inbox.state(&inputs, "submitted", None)?;
        self.pending = Some(Pending {
            inputs: inputs.clone(),
            parent: parent.clone(),
            remote_id: None,
        });
        self.send(json!({"type":"response.steer","previous_response_id":parent,"input":wire}))
            .await?;
        self.inbox
            .receipts(&inputs, "submitted", &self.peer)
            .await?;
        if !self.running && !self.needs_tools {
            self.auto_wait = true;
        }
        Ok(())
    }
    async fn request(&mut self, request: Request) -> Result<()> {
        ensure!(
            self.waiter.is_none(),
            "Only one engine response request may be outstanding"
        );
        if let Some(value) = self.ready.pop_front() {
            let _ = request.reply.send(Ok(value));
            return Ok(());
        }
        self.waiter = Some(request.reply);
        if self.running || self.auto_wait {
            return Ok(());
        }
        self.maintenance = false;
        // A late arrival after a final response belongs to its automatic successor.
        if self.id.is_some() && !self.needs_tools {
            self.steer().await?;
            if self.pending.is_some() || self.auto_wait {
                return Ok(());
            }
        }
        ensure!(
            self.created < self.max,
            "Logical turn model-call budget exhausted"
        );
        let mut body = request.body;
        let mut all = body["input"]
            .as_array()
            .context("Responses input must be an array")?
            .clone();
        self.fixed_bytes = body["instructions"].as_str().unwrap_or("").len()
            + serde_json::to_vec(&body["tools"])?.len();
        if self.id.is_none() {
            let pending = self.inbox.queued()?;
            if !pending.is_empty() && self.fits(&pending, &all).await? {
                all.extend(history::responses(
                    &pending.iter().map(|i| i.item.clone()).collect::<Vec<_>>(),
                ));
                body["input"] = json!(all);
                self.inbox.state(&pending, "submitted", None)?;
                self.inline_inputs = pending;
            }
        }
        if let Some(id) = &self.id {
            let current = &self.history[self.window_start..];
            if all.starts_with(current) {
                body["previous_response_id"] = json!(id);
                body["input"] = json!(&all[current.len()..]);
            } else if self.pending.is_some() {
                // Refreshed source snapshots and compaction can change an old
                // prefix without changing the active work. Finish the existing
                // chain with only the engine's new local observations; never
                // replay its input, assistant output, or completed tool calls.
                let observations = history::responses(&request.continuation_items);
                all = self.history[self.window_start..].to_vec();
                all.extend(observations.iter().cloned());
                let pending = history::responses(
                    &self
                        .pending
                        .as_ref()
                        .unwrap()
                        .inputs
                        .iter()
                        .map(|input| input.item.clone())
                        .collect::<Vec<_>>(),
                );
                ensure!(
                    serde_json::to_vec(&all)?.len()
                        + serde_json::to_vec(&pending)?.len()
                        + self.fixed_bytes
                        <= self.context_limit,
                    "Active model chain reached its context budget while steering is pending; no work was replayed"
                );
                body["previous_response_id"] = json!(id);
                body["input"] = json!(observations);
                self.inbox.store.event(&self.inbox.sid, Some(&self.inbox.turn),
                    "context.refresh.deferred", json!({"reason":"pending_steering","local_observations":request.continuation_items.len()}))?;
            } else {
                // A local compaction checkpoint starts a settled chain with full input.
                self.id = None;
            }
        }
        // Responses WebSockets have a bounded lifetime. Rotate only between
        // settled responses, never while a steering successor may be running.
        // Send the canonical context, not a replay of a possibly accepted call.
        if self.opened_at.elapsed() >= std::time::Duration::from_secs(55 * 60)
            && !self.running
            && !self.awaiting_created
            && !self.auto_wait
            && self.pending.is_none()
        {
            let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(24 * 1024 * 1024))
                .max_frame_size(Some(24 * 1024 * 1024));
            let (socket, _) = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                tokio_tungstenite::connect_async_tls_with_config(
                    self.connection_request.clone(),
                    Some(config),
                    false,
                    self.connector.clone(),
                ),
            )
            .await
            .context("Settled model connection rotation timed out")?
            .map_err(|_| {
                anyhow::anyhow!("Settled model connection rotation failed; no work was replayed")
            })?;
            let _ = self.socket.close(None).await;
            self.socket = socket;
            self.opened_at = tokio::time::Instant::now();
            self.id = None;
            body.as_object_mut().unwrap().remove("previous_response_id");
            body["input"] = json!(all);
            self.inbox.store.event(
                &self.inbox.sid,
                Some(&self.inbox.turn),
                "provider.connection.rotated",
                json!({"reason":"settled_lifetime_boundary"}),
            )?;
        }
        self.history = all;
        self.window_start = 0;
        body.as_object_mut().unwrap().remove("stream");
        body.as_object_mut().unwrap().remove("background");
        body["type"] = json!("response.create");
        self.running = true;
        self.awaiting_created = true;
        self.needs_tools = false;
        self.send(body).await
    }
    async fn event(&mut self, event: Value) -> Result<()> {
        if matches!(
            event["type"].as_str(),
            Some("response.steer.failed" | "response.failed" | "error")
        ) {
            // Known codes are actionable; arbitrary provider strings can echo
            // private inputs. Never log the response body or error message.
            let raw = event["error"]["code"]
                .as_str()
                .or_else(|| event["response"]["error"]["code"].as_str());
            let code = match raw {
                Some(
                    "invalid_input"
                    | "steering_not_supported"
                    | "response_not_found"
                    | "too_many_pending_steers"
                    | "rate_limit_exceeded"
                    | "server_error"
                    | "invalid_request_error"
                    | "context_length_exceeded",
                ) => raw.unwrap(),
                _ => "unclassified",
            };
            self.inbox.store.event(&self.inbox.sid, Some(&self.inbox.turn), "provider.error",
                json!({"event":event["type"],"code":code,
                    "pending_inputs":self.pending.as_ref().map_or(0,|p|p.inputs.len()),
                    "steering_accepted":self.pending.as_ref().is_some_and(|p|p.remote_id.is_some())}))?;
        }
        match event["type"].as_str().unwrap_or("") {
            "response.created" => {
                let id = event["response"]["id"]
                    .as_str()
                    .context("Response identity missing")?
                    .to_owned();
                let parent = event["response"]["previous_response_id"]
                    .as_str()
                    .map(str::to_owned);
                if let Some(previous) = self.seen_created.get(&id) {
                    ensure!(
                        *previous == parent,
                        "Conflicting duplicate response identity"
                    );
                    return Ok(());
                }
                self.seen_created.insert(id.clone(), parent);
                self.inbox.store.event(
                    &self.inbox.sid,
                    Some(&self.inbox.turn),
                    "provider.response.created",
                    json!({"response_id":id,"source":"responses_websocket"}),
                )?;
                self.messages = super::identity::Messages::default();
                self.created += 1;
                ensure!(
                    self.created <= self.max,
                    "Logical turn model-call budget exhausted"
                );
                if let Some(p) = self.pending.take() {
                    ensure!(
                        p.remote_id.is_some(),
                        "Successor arrived without steering acknowledgment; input outcome is unknown"
                    );
                    if let Some(parent) = event["response"]["previous_response_id"].as_str() {
                        ensure!(parent == p.parent, "Unexpected steering successor");
                    }
                    self.inbox
                        .remote(&p.inputs, &p.parent, p.remote_id.as_deref(), Some(&id))?;
                    self.inbox.state(&p.inputs, "incorporated", None)?;
                    self.inbox
                        .receipts(&p.inputs, "incorporated", &self.peer)
                        .await?;
                    self.history.extend(history::responses(
                        &p.inputs.iter().map(|i| i.item.clone()).collect::<Vec<_>>(),
                    ));
                    self.inputs.extend(p.inputs);
                }
                if !self.inline_inputs.is_empty() {
                    self.inbox
                        .remote(&self.inline_inputs, "", None, Some(&id))?;
                    self.inbox
                        .state(&self.inline_inputs, "incorporated", None)?;
                    self.inbox
                        .receipts(&self.inline_inputs, "incorporated", &self.peer)
                        .await?;
                    self.inputs.append(&mut self.inline_inputs);
                }
                self.id = Some(id);
                self.awaiting_created = false;
                self.running = true;
                self.auto_wait = false;
                self.needs_tools = false;
                self.steer().await?;
            }
            "response.steer.accepted" => {
                let remote = event["steer"]["id"]
                    .as_str()
                    .context("Steering ID missing")?
                    .to_owned();
                let parent = event["steer"]["previous_response_id"]
                    .as_str()
                    .context("Steering parent missing")?
                    .to_owned();
                if let Some(old) = self.seen_accepted.get(&remote) {
                    ensure!(*old == parent, "Conflicting duplicate steering identity");
                    return Ok(());
                }
                self.seen_accepted.insert(remote, parent);
                let p = self
                    .pending
                    .as_mut()
                    .context("Unexpected steering acknowledgment")?;
                ensure!(
                    event["steer"]["previous_response_id"] == p.parent,
                    "Steering parent mismatch"
                );
                p.remote_id = Some(
                    event["steer"]["id"]
                        .as_str()
                        .context("Steering ID missing")?
                        .into(),
                );
                self.inbox
                    .remote(&p.inputs, &p.parent, p.remote_id.as_deref(), None)?;
                self.inbox.state(&p.inputs, "remoteQueued", None)?;
                self.inbox
                    .receipts(&p.inputs, "remoteQueued", &self.peer)
                    .await?;
            }
            "response.steer.pending" => {
                let p = self
                    .pending
                    .as_ref()
                    .context("Unexpected pending steering")?;
                ensure!(
                    event["steer"]["id"].as_str() == p.remote_id.as_deref(),
                    "Pending steering ID mismatch"
                );
                ensure!(
                    event["reason"] == "waiting_for_required_input",
                    "Unsupported steering pending reason"
                );
                self.needs_tools = true;
                self.auto_wait = false;
            }
            "response.steer.failed" => {
                let p = self.pending.take().context("Unexpected steering failure")?;
                if let Some(id) = &p.remote_id {
                    ensure!(event["steer"]["id"] == *id, "Failed steering ID mismatch");
                }
                self.inbox
                    .state(&p.inputs, "notAccepted", Some("steering_rejected"))?;
                self.inbox
                    .receipts(&p.inputs, "notAccepted", &self.peer)
                    .await?;
                bail!("Model rejected steering; input retained, not retried automatically")
            }
            "response.output_item.added" | "response.output_item.done" => {
                if event["type"] == "response.output_item.added"
                    && event["item"]["type"] == "compaction"
                    && let Some(id) = event["item"]["id"].as_str()
                {
                    self.peer
                        .compaction(&self.inbox.sid, &format!("server-{id}"), "in_progress")
                        .await?;
                }
                self.messages.observe(&event);
                let completed = self.messages.completed(&event);
                self.peer
                    .complete_public_message(&self.inbox.sid, completed)
                    .await?;
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(text) = event["delta"].as_str() {
                    self.peer
                        .update(&self.inbox.sid, self.messages.chunk(&event, text))
                        .await?;
                }
            }
            "response.completed" | "response.incomplete" => {
                let mut response = event["response"].clone();
                let id = response["id"]
                    .as_str()
                    .context("Response ID missing")?
                    .to_owned();
                if let Some(old) = self.seen_completed.get(&id) {
                    ensure!(
                        *old == response,
                        "Conflicting duplicate response completion"
                    );
                    return Ok(());
                }
                self.seen_completed.insert(id, response.clone());
                self.messages.finish(&mut response);
                ensure!(
                    response["id"].as_str() == self.id.as_deref(),
                    "Unexpected response completion"
                );
                let output = response["output"]
                    .as_array()
                    .context("Response output missing")?;
                if let Some(index) = output.iter().rposition(|v| v["type"] == "compaction") {
                    self.window_start = self.history.len() + index;
                }
                self.history.extend(history::responses(output));
                self.needs_tools = output.iter().any(|v| {
                    matches!(
                        v["type"].as_str(),
                        Some("function_call" | "computer_call" | "mcp_approval_request")
                    )
                });
                self.running = false;
                self.auto_wait = !self.needs_tools
                    && (self.pending.is_some()
                        || response["incomplete_details"]["reason"] == "steered");
                response["_continue"] = json!(self.auto_wait);
                response["_steering_inputs"] =
                    serde_json::to_value(std::mem::take(&mut self.inputs))?;
                if let Some(reply) = self.waiter.take() {
                    let _ = reply.send(Ok(response));
                } else {
                    ensure!(
                        self.ready.len() < self.max,
                        "Unconsumed response budget exceeded"
                    );
                    self.ready.push_back(response);
                }
                // Already-running local work is not cancelled. New user input may queue for
                // the response while its real tool results are being produced.
                if self.needs_tools {
                    self.steer().await?;
                }
            }
            "response.failed" | "error" => bail!(
                "Model WebSocket reported failure; check model, quota and supported settings; no work was replayed"
            ),
            _ => {}
        }
        Ok(())
    }
    async fn run(&mut self) -> Result<()> {
        // A live connection may legitimately spend many minutes thinking or
        // compacting. Ping/pong detects a dead peer without imposing a response
        // completion deadline. It never starts/replays a model request.
        let mut last_received = tokio::time::Instant::now();
        let heartbeat_every = (self.idle / 3).min(std::time::Duration::from_secs(30));
        let mut heartbeat = tokio::time::interval(heartbeat_every);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
             biased;
             _=self.cancel.cancelled()=>{let _=self.socket.close(None).await;return Ok(())},
             command=self.requests.recv()=>{let Some(command)=command else {return Ok(())};
                match command {
                    Command::Response(request)=>{
                        self.failure_stage="request";self.failure_category="runtime_boundary";
                        let opened=self.opened_at;self.request(request).await?;
                        if self.opened_at!=opened{last_received=self.opened_at;}
                    }
                    Command::BeginCompaction(reply)=>{
                        let settled=!self.running && !self.awaiting_created && !self.auto_wait
                            && self.pending.is_none() && self.waiter.is_none() && self.ready.is_empty();
                        if settled { self.maintenance=true; self.id=None; }
                        let _=reply.send(settled);
                    }
                }
             },
             event=self.socket.next()=>{last_received=tokio::time::Instant::now();self.failure_stage="receive";self.failure_category="transport";match event.context("Model WebSocket closed before work settled")?.context("Model WebSocket interrupted")? {
              Message::Text(text)=>{self.total_bytes+=text.len();ensure!(self.total_bytes<=128*1024*1024,"Logical turn model stream exceeded byte budget");let event:Value=serde_json::from_str(&text).context("Invalid model WebSocket event")?;self.failure_stage="event";self.failure_category=if matches!(event["type"].as_str(),Some("response.failed"|"error"|"response.steer.failed")){"provider"}else{"protocol"};self.event(event).await?;},
              Message::Ping(data)=>self.socket.send(Message::Pong(data)).await?,
              Message::Close(_)=>bail!("Model WebSocket closed; no pending input was replayed"),
              Message::Binary(_)=>bail!("Unexpected binary model event"),_=>{}
             }},
             _=heartbeat.tick()=>{
                self.failure_stage="liveness";self.failure_category="transport";
                ensure!(last_received.elapsed()<self.idle,"Model WebSocket stopped answering; no pending input was replayed");
                tokio::time::timeout(self.idle,self.socket.send(Message::Ping(Vec::new().into()))).await.context("Model WebSocket heartbeat send timed out")??;
             },
             _=self.inbox.changed.notified()=>{self.failure_stage="steering";self.failure_category="input_delivery";self.steer().await?;},
            }
        }
    }
}
