use crate::{
    config::{Config, Mode},
    engine::Turn,
    mcp,
    rpc::{Peer, frame},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

struct State {
    config: Config,
    store: Store,
    peer: Peer,
    initialized: AtomicBool,
    loaded: Mutex<HashMap<String, Loaded>>,
    client_form_elicitation: AtomicBool,
    active: Mutex<HashMap<String, (Value, CancellationToken, CancellationToken)>>,
    slots: Arc<Semaphore>,
}
#[derive(Clone)]
struct Loaded {
    tools: Arc<mcp::Tools>,
    config: Arc<Config>,
}
struct Active {
    state: Arc<State>,
    id: String,
}
#[derive(Debug)]
struct ProtocolError {
    code: i64,
    message: String,
}
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProtocolError {}
fn protocol_error(code: i64, message: impl Into<String>) -> anyhow::Error {
    ProtocolError {
        code,
        message: message.into(),
    }
    .into()
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Some((_, _, done)) = self.state.active.lock().unwrap().remove(&self.id) {
            done.cancel();
        }
    }
}
pub async fn serve(config: Config, store: Store) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(128);
    let peer = Peer::new(tx);
    let state = Arc::new(State {
        slots: Arc::new(Semaphore::new(config.limits.max_parallel_turns)),
        config,
        store,
        peer: peer.clone(),
        initialized: AtomicBool::new(false),
        loaded: Mutex::new(HashMap::new()),
        client_form_elicitation: AtomicBool::new(false),
        active: Mutex::new(HashMap::new()),
    });
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(value) = rx.recv().await {
            out.write_all(format!("{value}\n").as_bytes()).await?;
            out.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    // frame() keeps a partial line in its future. Keep that future alive while
    // requests finish; racing it directly with join_next() could discard bytes.
    let (frames_tx, mut frames_rx) = mpsc::channel(16);
    let reader = tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        loop {
            match frame(&mut input).await {
                Ok(Some(value)) => {
                    if frames_tx.send(Ok(value)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = frames_tx.send(Err(error)).await;
                    break;
                }
            }
        }
    });
    let mut tasks = JoinSet::new();
    let result=async {loop {
        tokio::select! {
            result=frames_rx.recv()=> {
                let Some(msg)=result else {break};
                let msg=msg?;
                if msg.get("method").is_none(){peer.response(msg);continue;}
                ensure!(msg["jsonrpc"]=="2.0","Expected JSON-RPC 2.0");
                let method=msg["method"].as_str().context("Method must be a string")?;
                if matches!(method,"session/cancel"|"$/cancel_request") {
                    for (sid,(request,token,_)) in state.active.lock().unwrap().iter(){if (method=="session/cancel" && msg["params"]["sessionId"]==*sid) || (method=="$/cancel_request" && msg["params"]["requestId"]==*request) {token.cancel();}}
                    if let Some(id)=msg.get("id"){peer.send(json!({"jsonrpc":"2.0","id":id,"result":{}})).await?;}continue;
                }
                if msg.get("id").is_none(){continue;}
                if tasks.len()>=64 {peer.send(json!({"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32000,"message":"Too many pending requests"}})).await?;continue;}
                let state=state.clone();tasks.spawn(async move {let id=msg["id"].clone();let result=handle(state.clone(),&msg).await;let response=match result{Ok(r)=>json!({"jsonrpc":"2.0","id":id,"result":r}),Err(e)=>json!({"jsonrpc":"2.0","id":id,"error":{"code":if msg["method"].as_str().is_some_and(|m|!known(m)){-32601}else{e.downcast_ref::<ProtocolError>().map_or(-32603,|e|e.code)},"message":e.to_string()}})};let _=state.peer.send(response).await;});
            },
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }Ok::<_,anyhow::Error>(())}.await;
    for (_, token, _) in state.active.lock().unwrap().values() {
        token.cancel();
    }
    // Give active turns a bounded chance to notify MCP servers and persist
    // uncertain effects after the caller disconnects.
    let drained = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tasks.abort_all();
    }
    while tasks.join_next().await.is_some() {}
    reader.abort();
    let _ = reader.await;
    drop(state);
    drop(peer);
    let _ = writer.await;
    result
}
fn known(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "session/new"
            | "session/load"
            | "session/resume"
            | "session/close"
            | "session/delete"
            | "session/list"
            | "session/prompt"
            | "session/set_mode"
            | "session/set_config_option"
    )
}
fn modes(mode: Mode) -> Value {
    json!({"currentModeId":mode.id(),"availableModes":[{"id":"read_only","name":"Read only","description":"Inspect without changing files or calling mutating tools"},{"id":"ask","name":"Ask before changes","description":"Ask this client before each write, command, or mutating MCP call"},{"id":"workspace","name":"Workspace access","description":"Allow local workspace writes and isolated Linux commands; other mutations still require permission"}]})
}
fn config_options(mode: Mode) -> Value {
    json!([{"id":"mode","name":"Permissions","description":"Controls changes and tool approvals for this session","category":"mode","type":"select","currentValue":mode.id(),"options":modes(mode)["availableModes"].as_array().unwrap().iter().map(|m| json!({"value":m["id"],"name":m["name"],"description":m["description"]})).collect::<Vec<_>>()}])
}
async fn handle(state: Arc<State>, msg: &Value) -> Result<Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let p = &msg["params"];
    if method == "initialize" {
        ensure!(
            p["protocolVersion"].as_u64().is_some(),
            "Protocol version required"
        );
        ensure!(
            !state.initialized.swap(true, Ordering::SeqCst),
            "Connection already initialized"
        );
        state.client_form_elicitation.store(
            p["clientCapabilities"]["elicitation"]["form"].is_object(),
            Ordering::SeqCst,
        );
        let mut result = json!({"protocolVersion":1,"agentInfo":{"name":"agent-runtime","title":state.config.name,"version":env!("CARGO_PKG_VERSION")},"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"list":{},"resume":{},"close":{},"delete":{},"additionalDirectories":{}},"mcpCapabilities":{"http":true,"sse":false},"promptCapabilities":{"image":state.config.provider.native.image_input,"audio":state.config.provider.native.audio_input,"embeddedContext":true}},"authMethods":[],"_meta":{"io.github.bkrabach.agent-runtime":{"history":{"profile":"agent-runtime.history/1","storeId":state.store.identity()?},"clientStdioMcpAllowed":state.config.allow_client_stdio_mcp}}});
        result["_meta"][crate::context::SNAPSHOT_META] =
            json!({"version":1,"contentBlocks":true,"originalHistoryPreserved":true});
        if state.config.allow_client_system_prompt {
            result["agentCapabilities"]["sessionCapabilities"]["systemPrompt"] = json!({});
        }
        return Ok(result);
    }
    ensure!(
        state.initialized.load(Ordering::SeqCst),
        "Initialize the ACP connection first"
    );
    match method {
        "session/new" | "session/load" | "session/resume" => {
            let cwd = state
                .config
                .cwd(
                    p["cwd"]
                        .as_str()
                        .ok_or_else(|| protocol_error(-32602, "cwd required"))?,
                )
                .map_err(|e| protocol_error(-32602, e.to_string()))?;
            let additional: Vec<std::path::PathBuf> = serde_json::from_value(
                p.get("additionalDirectories")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(json!([])),
            )
            .map_err(|_| {
                protocol_error(-32602, "additionalDirectories must be an array of paths")
            })?;
            let mut config = state
                .config
                .for_session(cwd.clone(), &additional)
                .map_err(|e| protocol_error(-32602, e.to_string()))?;
            config.client_form_elicitation = state.client_form_elicitation.load(Ordering::SeqCst);
            if let Some(prompt) = p.get("systemPrompt").filter(|v| !v.is_null()) {
                if !config.allow_client_system_prompt {
                    return Err(protocol_error(
                        -32602,
                        "Client system instructions require explicit owner authorization in runtime configuration",
                    ));
                }
                if prompt["mode"] != "append" {
                    return Err(protocol_error(
                        -32602,
                        "Only append systemPrompt mode is supported",
                    ));
                }
                let blocks = prompt["content"].as_array().ok_or_else(|| {
                    protocol_error(
                        -32602,
                        "System prompt content must be an array of text blocks",
                    )
                })?;
                let mut text = String::new();
                for block in blocks {
                    if block["type"] != "text" || !block["text"].is_string() {
                        return Err(protocol_error(
                            -32602,
                            "System prompt supports text blocks only",
                        ));
                    }
                    text.push_str(block["text"].as_str().unwrap());
                    text.push('\n');
                    if text.len() + config.instructions.len() > 128 * 1024 {
                        return Err(protocol_error(
                            -32602,
                            "System prompt exceeds instruction budget",
                        ));
                    }
                }
                config
                    .instructions
                    .push_str("\nClient-authorized session instructions:\n");
                config.instructions.push_str(&text);
            }
            ensure!(
                state.loaded.lock().unwrap().len() < 128,
                "Connection session limit reached"
            );
            let specs: Vec<Value> =
                serde_json::from_value(p.get("mcpServers").cloned().unwrap_or(json!([])))?;
            ensure!(
                state.config.allow_client_stdio_mcp || specs.iter().all(|s| s["type"] == "http"),
                "Client-supplied local MCP programs are disabled. Use HTTP MCP, or explicitly trust this ACP client with host command authority in runtime configuration."
            );
            let mcp = Arc::new(mcp::Tools::connect(&specs).await?);
            let session = if method == "session/new" {
                state.store.create(cwd, state.config.mode)?
            } else {
                let sid = p["sessionId"].as_str().context("Session ID required")?;
                let _lock = state.store.lock(sid)?;
                let session = state
                    .store
                    .load(sid)
                    .map_err(|_| protocol_error(-32002, "Session not found"))?;
                ensure!(
                    session.cwd == cwd,
                    "Resume must use the session's original working folder"
                );
                for item in session.items.iter().filter(|_| method == "session/load") {
                    if item["type"] == "function_call" {
                        let name = item["name"].as_str().unwrap_or("tool");
                        state.peer.update(sid,json!({"sessionUpdate":"tool_call","toolCallId":item["call_id"],"title":name.replace('_'," "),"kind":"other","status":"pending","rawInput":serde_json::from_str::<Value>(item["arguments"].as_str().unwrap_or("{}")).unwrap_or(Value::Null)})).await?;
                    }
                    if item["type"] == "function_call_output" {
                        let text = item["output"].as_str().unwrap_or("");
                        let failed = serde_json::from_str::<Value>(text).is_ok_and(|v| {
                            v.get("error").is_some()
                                || v["isError"] == true
                                || v.get("success") == Some(&json!(false))
                        }) || text.starts_with("Execution was interrupted");
                        let mut blocks =
                            vec![json!({"type":"content","content":{"type":"text","text":text}})];
                        if let Some(ms) = item["_acp_content"].as_array() {
                            blocks.extend(ms.iter().map(|m| json!({"type":"content","content":m})));
                        }
                        if let Some(images) = item["_tool_images"].as_array() {
                            blocks.extend(
                                images.iter().map(|m| json!({"type":"content","content":m})),
                            );
                        }
                        state.peer.update(sid,json!({"sessionUpdate":"tool_call_update","toolCallId":item["call_id"],"status":if failed{"failed"}else{"completed"},"content":blocks})).await?;
                    }
                    let mut replay = item.clone();
                    if replay["type"] == "computer_call"
                        && !session.items.iter().any(|result| {
                            result["type"] == "computer_call_output"
                                && result["call_id"] == replay["call_id"]
                        })
                    {
                        // A crash can leave a durable intent without its result.
                        // Display that uncertainty; never replay its side effects.
                        replay["_computer_interrupted"] = json!(true);
                    }
                    for update in crate::media::updates(&replay, true)? {
                        state.peer.update(sid, update).await?;
                    }
                }
                session
            };
            {
                let mut loaded = state.loaded.lock().unwrap();
                ensure!(
                    !state.active.lock().unwrap().contains_key(&session.id),
                    "Cannot change session grants while a turn is running"
                );
                loaded.insert(
                    session.id.clone(),
                    Loaded {
                        tools: mcp,
                        config: Arc::new(config),
                    },
                );
            }
            let mut result =
                json!({"modes":modes(session.mode),"configOptions":config_options(session.mode)});
            if method == "session/new" {
                result["sessionId"] = json!(session.id);
            }
            Ok(result)
        }
        "session/list" => {
            let cwd = p["cwd"].as_str().map(|c| state.config.cwd(c)).transpose()?;
            state.store.list(cwd.as_deref(), p["cursor"].as_str())
        }
        "session/close" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            let done = {
                // Removal and turn admission use the same lock order. A turn
                // cannot start in the gap between detachment and cancellation.
                let mut loaded = state.loaded.lock().unwrap();
                loaded.remove(sid);
                state
                    .active
                    .lock()
                    .unwrap()
                    .get(sid)
                    .map(|(_, token, done)| {
                        token.cancel();
                        done.clone()
                    })
            };
            if let Some(done) = done {
                tokio::time::timeout(std::time::Duration::from_secs(10), done.cancelled())
                    .await
                    .context("Session cancellation is still completing; retry close")?;
            }
            Ok(json!({}))
        }
        "session/delete" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            // Our IDs are UUIDs. An opaque ID from another implementation can
            // never name one of our sessions; deletion remains idempotent.
            if uuid::Uuid::parse_str(sid).is_err() {
                return Ok(json!({}));
            }
            let mut loaded = state.loaded.lock().unwrap();
            ensure!(
                !state.active.lock().unwrap().contains_key(sid),
                "Close the running session before deleting it"
            );
            let _lock = state.store.lock(sid)?;
            state.store.delete(sid)?;
            loaded.remove(sid);
            Ok(json!({}))
        }
        "session/set_mode" | "session/set_config_option" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            ensure!(
                state.loaded.lock().unwrap().contains_key(sid),
                "Load this session first"
            );
            let _lock = state.store.lock(sid)?;
            let mut session = state.store.load(sid)?;
            let value = if method == "session/set_config_option" {
                ensure!(
                    p["configId"] == "mode",
                    "Unknown session configuration option"
                );
                &p["value"]
            } else {
                &p["modeId"]
            };
            session.mode = Mode::parse(value.as_str().context("Mode required")?)?;
            state.store.save(&session)?;
            state.peer.update(sid, json!({"sessionUpdate":"config_option_update","configOptions":config_options(session.mode)})).await?;
            state.peer.update(sid, json!({"sessionUpdate":"current_mode_update","currentModeId":session.mode.id()})).await?;
            Ok(if method == "session/set_config_option" {
                json!({"configOptions":config_options(session.mode)})
            } else {
                json!({})
            })
        }
        "session/prompt" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            let permit = state
                .slots
                .clone()
                .try_acquire_owned()
                .context("Agent turn capacity reached; retry later")?;
            let token = CancellationToken::new();
            let loaded_session = {
                let loaded = state.loaded.lock().unwrap();
                let session = loaded
                    .get(sid)
                    .cloned()
                    .ok_or_else(|| protocol_error(-32002, "Load this session first"))?;
                let mut active = state.active.lock().unwrap();
                ensure!(
                    !active.contains_key(sid),
                    "This session already has a running turn"
                );
                active.insert(
                    sid.into(),
                    (msg["id"].clone(), token.clone(), CancellationToken::new()),
                );
                session
            };
            let _active = Active {
                state: state.clone(),
                id: sid.into(),
            };
            let _permit = permit;
            let turn = Turn {
                config: &loaded_session.config,
                store: &state.store,
                peer: &state.peer,
                session: state.store.load(sid)?,
                mcp: loaded_session.tools,
                cancel: token,
            };
            turn.run(serde_json::from_value(p["prompt"].clone())?).await
        }
        _ => bail!("Method not supported"),
    }
}
