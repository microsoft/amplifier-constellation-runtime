use crate::{
    config::{Config, Mode},
    engine::Turn,
    mcp,
    rpc::Peer,
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
use tokio::{sync::Semaphore, task::JoinSet};
use tokio_util::sync::CancellationToken;

type ActiveTurn = (
    Value,
    CancellationToken,
    CancellationToken,
    Arc<crate::steering::Inbox>,
);

struct State {
    config: Config,
    store: Store,
    peer: Peer,
    initialized: AtomicBool,
    loaded: Mutex<HashMap<String, Loaded>>,
    client_form_elicitation: AtomicBool,
    active: Mutex<HashMap<String, ActiveTurn>>,
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
fn response_for(method: &str, id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":if !known(method){-32601}else{error.downcast_ref::<ProtocolError>().map_or(-32603,|e|e.code)},"message":error.to_string()}})
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Some((_, _, done, inbox)) = self.state.active.lock().unwrap().remove(&self.id) {
            let _ = inbox.close("turn_closed");
            done.cancel();
        }
    }
}
pub async fn serve(config: Config, store: Store) -> Result<()> {
    let mut connection = amplifier_acp_connection::Connection::agent(
        tokio::io::stdin(),
        tokio::io::stdout(),
        24 * 1024 * 1024,
    )
    .await?;
    let peer = Peer::new(connection.sender.clone());
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
    let mut tasks = JoinSet::new();
    let result=async {loop {
        tokio::select! {
            result=connection.recv()=> {
                let Some(msg)=result? else {break};
                ensure!(msg["jsonrpc"]=="2.0","Expected JSON-RPC 2.0");
                let method=msg["method"].as_str().context("Method must be a string")?;
                if matches!(method,"session/cancel"|"$/cancel_request") {
                    for (sid,(request,token,_,_)) in state.active.lock().unwrap().iter(){if (method=="session/cancel" && msg["params"]["sessionId"]==*sid) || (method=="$/cancel_request" && msg["params"]["requestId"]==*request) {token.cancel();}}
                    if let Some(id)=msg.get("id"){peer.send(json!({"jsonrpc":"2.0","id":id,"result":{}})).await?;}continue;
                }
                if msg.get("id").is_none(){continue;}
                if tasks.len()>=64 {peer.send(json!({"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32000,"message":"Too many pending requests"}})).await?;continue;}
                // Admission is synchronous in frame order. Parallel prompt/model/tool
                // work must not reorder a burst before its durable sequence is assigned.
                if method=="_session/steering" {
                    let result=handle(state.clone(),&msg).await;
                    peer.send(response_for(method,msg["id"].clone(),result)).await?;
                    continue;
                }
                let state=state.clone();tasks.spawn(async move {
                    let result=handle(state.clone(),&msg).await;
                    let response=response_for(msg["method"].as_str().unwrap(),msg["id"].clone(),result);
                    let _=state.peer.send(response).await;
                });
            },
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }Ok::<_,anyhow::Error>(())}.await;
    for (_, token, _, _) in state.active.lock().unwrap().values() {
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
    drop(state);
    drop(peer);
    drop(connection);
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
            | "_session/steering"
            | "_session/input_receipts"
            | "_session/acknowledge_inputs"
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
        state.peer.compaction_supported.store(
            p["clientCapabilities"]["session"]["compaction"].is_object(),
            Ordering::SeqCst,
        );
        state.peer.message_completion_supported.store(
            amplifier_acp_connection::speech::supported(&p["clientCapabilities"]),
            Ordering::SeqCst,
        );
        state.client_form_elicitation.store(
            p["clientCapabilities"]["elicitation"]["form"].is_object(),
            Ordering::SeqCst,
        );
        let mut result = json!({"protocolVersion":1,"agentInfo":{"name":"agent-runtime","title":state.config.name,"version":env!("CARGO_PKG_VERSION")},"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"list":{},"resume":{},"close":{},"delete":{},"additionalDirectories":{}},"mcpCapabilities":{"http":true,"sse":false},"promptCapabilities":{"image":state.config.provider.native.image_input,"audio":state.config.provider.native.audio_input,"embeddedContext":true}},"authMethods":[],"_meta":{"io.github.bkrabach.agent-runtime":{"history":{"profile":"agent-runtime.history/1","storeId":state.store.identity()?},"clientStdioMcpAllowed":state.config.allow_client_stdio_mcp}}});
        amplifier_acp_connection::speech::advertise(&mut result);
        result["_meta"]["steering"] = json!({"supported":true,"version":1,"idleBehavior":["promptRequired"],"receiptsMethod":"_session/input_receipts","acknowledgeMethod":"_session/acknowledge_inputs"});
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
                state.store.recover_inputs(sid, None)?;
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
            result["_meta"][crate::steering::META]["inputReceipts"] =
                state.store.input_receipts(&session.id)?;
            Ok(result)
        }
        "_session/input_receipts" | "_session/acknowledge_inputs" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            ensure!(
                state.loaded.lock().unwrap().contains_key(sid),
                "Load the session before inspecting input receipts"
            );
            if method == "_session/acknowledge_inputs" {
                let _lock = state.store.lock(sid)?;
                ensure!(
                    p["disposition"] == "proceed_without_replay",
                    "Explicit proceed_without_replay disposition required"
                );
                let ids: Vec<String> = serde_json::from_value(p["inputIds"].clone())?;
                state.store.acknowledge_inputs(sid, &ids)?;
            }
            Ok(json!({"inputReceipts":state.store.input_receipts(sid)?}))
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
                    .map(|(_, token, done, _)| {
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
        "_session/steering" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            ensure!(
                p["_meta"]["steering"]["idleBehavior"]
                    .as_str()
                    .is_none_or(|b| b == "promptRequired"),
                "Only promptRequired idle behavior is supported"
            );
            let loaded = state
                .loaded
                .lock()
                .unwrap()
                .get(sid)
                .cloned()
                .context("Load this session first")?;
            let blocks: Vec<Value> = serde_json::from_value(p["prompt"].clone())?;
            ensure!(!blocks.is_empty(), "Steering input must not be empty");
            let content = crate::media::prompt(&loaded.config, &blocks)?;
            let item = json!({"role":"user","content":content,"_acp_prompt":blocks});
            let inbox = state
                .active
                .lock()
                .unwrap()
                .get(sid)
                .map(|(_, _, _, i)| i.clone());
            let Some(inbox) = inbox else {
                return Ok(json!({"outcome":"promptRequired","reason":"noRunningTurn"}));
            };
            let id = p["_meta"]["steering"]["inputId"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(crate::store::id);
            inbox.admit(&id, item)
        }
        "session/prompt" => {
            let sid = p["sessionId"].as_str().context("Session ID required")?;
            let permit = state
                .slots
                .clone()
                .try_acquire_owned()
                .context("Agent turn capacity reached; retry later")?;
            let token = CancellationToken::new();
            let inbox = Arc::new(crate::steering::Inbox::new(state.store.clone(), sid)?);
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
                    (
                        msg["id"].clone(),
                        token.clone(),
                        CancellationToken::new(),
                        inbox.clone(),
                    ),
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
                inbox,
            };
            turn.run(serde_json::from_value(p["prompt"].clone())?).await
        }
        _ => bail!("Method not supported"),
    }
}
