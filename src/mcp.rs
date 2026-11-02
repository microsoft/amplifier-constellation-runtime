//! Session-local MCP connections. Credentials are never stored in session history.
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const CANCEL_TIMEOUT: Duration = Duration::from_millis(500);
const STDIO_CANCEL_GRACE: Duration = Duration::from_millis(100);

pub struct Tools {
    peers: Vec<Mutex<Connection>>,
    pub tools: BTreeMap<String, Tool>,
}
pub struct Tool {
    pub server: String,
    pub name: String,
    pub definition: Value,
    pub readonly: bool,
    pub endpoint: Option<String>,
    index: usize,
}

/// Delivery is best-effort: even a delivered cancellation does not prove that an
/// external side effect stopped. This report intentionally contains no endpoint
/// URLs, headers, or tool arguments.
#[derive(Debug, serde::Serialize)]
pub struct CancellationReport {
    pub server_index: usize,
    pub request_id: u64,
    pub transport: &'static str,
    pub notification_sent: bool,
    pub quarantined: bool,
    pub outcome_uncertain: bool,
}

struct Connection {
    transport: Option<Transport>,
    next: u64,
    pending: Option<u64>,
}

enum Transport {
    Http {
        client: reqwest::Client,
        url: String,
        headers: reqwest::header::HeaderMap,
        session: Option<String>,
        protocol: String,
    },
    Stdio {
        child: Box<Child>,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
        #[cfg(unix)]
        pid: u32,
    },
}
impl Drop for Transport {
    fn drop(&mut self) {
        if let Self::Stdio {
            child,
            #[cfg(unix)]
            pid,
            ..
        } = self
        {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(*pid as i32), libc::SIGKILL);
            }
            let _ = child.start_kill();
        }
    }
}
impl Tools {
    pub async fn connect(specs: &[Value]) -> Result<Self> {
        ensure!(specs.len() <= 8, "At most eight MCP servers per session");
        let mut set = Self {
            peers: vec![],
            tools: BTreeMap::new(),
        };
        for (i, spec) in specs.iter().enumerate() {
            let name = spec["name"].as_str().context("MCP server name required")?;
            ensure!(
                !name.is_empty() && name.len() <= 128,
                "Invalid MCP server name"
            );
            let mut peer = Connection::open(spec).await?;
            let init=peer.request("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"agent-runtime","version":env!("CARGO_PKG_VERSION")}})).await?;
            if let Some(Transport::Http { protocol, .. }) = &mut peer.transport {
                let version = init["protocolVersion"]
                    .as_str()
                    .context("MCP protocol version missing")?;
                ensure!(
                    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].contains(&version),
                    "Unsupported MCP version"
                );
                *protocol = version.into();
            }
            peer.notify("notifications/initialized", json!({})).await?;
            let mut cursor = Value::Null;
            for _ in 0..8 {
                let params = if cursor.is_null() {
                    json!({})
                } else {
                    json!({"cursor":cursor})
                };
                let list = peer.request("tools/list", params).await?;
                for tool in list["tools"]
                    .as_array()
                    .context("MCP did not return a tool list")?
                {
                    ensure!(set.tools.len() < 120, "MCP tool budget exceeded");
                    let original = tool["name"].as_str().context("MCP tool name missing")?;
                    let alias = format!(
                        "mcp_{i}_{}",
                        original
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                                c
                            } else {
                                '_'
                            })
                            .take(52)
                            .collect::<String>()
                    );
                    ensure!(!set.tools.contains_key(&alias), "MCP tool name collision");
                    let definition = json!({"type":"function","name":alias,"description":format!("[{}] {}",name,tool["description"].as_str().unwrap_or(original)),"parameters":tool["inputSchema"],"strict":false});
                    set.tools.insert(
                        alias,
                        Tool {
                            server: name.into(),
                            name: original.into(),
                            definition,
                            readonly: tool["annotations"]["readOnlyHint"] == true,
                            endpoint: if spec["type"] == "http" {
                                spec["url"].as_str().map(str::to_owned)
                            } else {
                                None
                            },
                            index: i,
                        },
                    );
                }
                cursor = list.get("nextCursor").cloned().unwrap_or(Value::Null);
                if cursor.is_null() {
                    break;
                }
            }
            ensure!(cursor.is_null(), "MCP tool pages exceeded limit");
            set.peers.push(Mutex::new(peer));
        }
        Ok(set)
    }
    pub async fn call(&self, alias: &str, args: Value) -> Result<Value> {
        let tool = self.tools.get(alias).context("MCP tool not available")?;
        self.peers[tool.index]
            .lock()
            .await
            .request("tools/call", json!({"name":tool.name,"arguments":args}))
            .await
    }

    /// Call after the turn's execution future has been dropped, so no tool call
    /// still holds a peer lock. Notification attempts run concurrently and each
    /// is bounded. No interrupted request is replayed or retried here.
    pub async fn cancel_pending(&self) -> Vec<CancellationReport> {
        futures_util::future::join_all(
            self.peers
                .iter()
                .enumerate()
                .map(|(index, peer)| async move { peer.lock().await.cancel_pending(index).await }),
        )
        .await
        .into_iter()
        .flatten()
        .collect()
    }
}
impl Connection {
    async fn open(spec: &Value) -> Result<Self> {
        Ok(Self {
            transport: Some(Transport::open(spec).await?),
            next: 1,
            pending: None,
        })
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.transport
            .as_mut()
            .context("MCP connection quarantined; reload the session to reconnect")?
            .exchange(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        ensure!(
            self.pending.is_none(),
            "Previous MCP request was interrupted; its outcome is unknown and it must not be replayed automatically"
        );
        let transport = self
            .transport
            .as_mut()
            .context("MCP connection quarantined; reload the session to reconnect")?;
        let id = self.next;
        self.next += 1;
        // This state outlives the request future. Dropping a partially read
        // frame must not make the connection look idle and reusable.
        self.pending = Some(id);
        let value = tokio::time::timeout(
            Duration::from_secs(120),
            transport.exchange(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})),
        )
        .await
        .context("MCP operation timed out; outcome is unknown; do not automatically replay")?
        .context("MCP operation interrupted; outcome is unknown; do not automatically replay")?;
        ensure!(
            value["id"] == id,
            "MCP response ID mismatch; outcome is unknown"
        );
        self.pending = None;
        ensure!(value.get("error").is_none(), "MCP rejected the operation");
        Ok(value["result"].clone())
    }

    async fn cancel_pending(&mut self, server_index: usize) -> Option<CancellationReport> {
        let request_id = self.pending.take()?;
        // Remove the transport before awaiting anything: dropping this cleanup
        // future must also drop/kill stdio, never restore its partial frame.
        let mut transport = self.transport.take()?;
        let stdio = matches!(&transport, Transport::Stdio { .. });
        let notification = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":request_id,"reason":"Client stopped waiting; the operation outcome remains uncertain"}});
        let notification_sent = matches!(
            tokio::time::timeout(CANCEL_TIMEOUT, transport.exchange(notification)).await,
            Ok(Ok(_))
        );
        if let Transport::Stdio { child, .. } = &mut transport {
            if notification_sent {
                // Allow a cooperating process to observe the notification and
                // exit. Otherwise Drop terminates it, including its Unix group.
                let _ = tokio::time::timeout(STDIO_CANCEL_GRACE, child.wait()).await;
            }
        } else {
            // HTTP requests own their framing; a later, distinct operation is
            // safe, but this interrupted operation must never be auto-replayed.
            self.transport = Some(transport);
        }
        Some(CancellationReport {
            server_index,
            request_id,
            transport: if stdio { "stdio" } else { "http" },
            notification_sent,
            quarantined: stdio,
            outcome_uncertain: true,
        })
    }
}

impl Transport {
    async fn open(spec: &Value) -> Result<Self> {
        if spec["type"] == "http" {
            let url = spec["url"].as_str().context("MCP URL required")?;
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.fragment().is_none(),
                "Invalid MCP URL"
            );
            ensure!(
                parsed.scheme() == "https"
                    || (parsed.scheme() == "http"
                        && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
                "MCP requires HTTPS or loopback HTTP"
            );
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(h) = spec["headers"].as_array() {
                ensure!(h.len() <= 24, "Too many MCP headers");
                for h in h {
                    let k = reqwest::header::HeaderName::from_bytes(
                        h["name"].as_str().context("Header name")?.as_bytes(),
                    )?;
                    ensure!(
                        !matches!(k.as_str(), "host" | "content-length" | "transfer-encoding"),
                        "Reserved MCP header"
                    );
                    headers.insert(
                        k,
                        reqwest::header::HeaderValue::from_str(
                            h["value"].as_str().context("Header value")?,
                        )?,
                    );
                }
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .connect_timeout(Duration::from_secs(10))
                .build()?;
            Ok(Self::Http {
                client,
                url: url.into(),
                headers,
                session: None,
                protocol: "2025-06-18".into(),
            })
        } else {
            ensure!(
                spec.get("type").is_none() || spec["type"] == "stdio",
                "Unsupported MCP transport"
            );
            let command = spec["command"].as_str().context("MCP command missing")?;
            let args: Vec<String> =
                serde_json::from_value(spec.get("args").cloned().unwrap_or(json!([])))?;
            let mut cmd = Command::new(command);
            cmd.env_clear()
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            for key in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "LANG"] {
                if let Ok(v) = std::env::var(key) {
                    cmd.env(key, v);
                }
            }
            if let Some(env) = spec["env"].as_array() {
                for e in env {
                    cmd.env(
                        e["name"].as_str().context("MCP env name")?,
                        e["value"].as_str().context("MCP env value")?,
                    );
                }
            }
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().context("MCP server could not start")?;
            #[cfg(unix)]
            let pid = child.id().context("MCP server exited")?;
            Ok(Self::Stdio {
                input: child.stdin.take().unwrap(),
                output: BufReader::new(child.stdout.take().unwrap()),
                child: Box::new(child),
                #[cfg(unix)]
                pid,
            })
        }
    }
    async fn exchange(&mut self, msg: Value) -> Result<Value> {
        let id = msg.get("id").cloned();
        match self {
            Self::Http {
                client,
                url,
                headers,
                session,
                protocol,
                ..
            } => {
                let mut request = client
                    .post(url.as_str())
                    .headers(headers.clone())
                    .header("Accept", "application/json, text/event-stream")
                    .header("MCP-Protocol-Version", protocol.as_str())
                    .json(&msg);
                if let Some(sid) = session.as_ref() {
                    request = request.header("Mcp-Session-Id", sid);
                }
                let response = request.send().await.context("MCP endpoint unreachable")?;
                ensure!(
                    response.status().is_success(),
                    "MCP request failed (HTTP {})",
                    response.status().as_u16()
                );
                if let Some(s) = response.headers().get("Mcp-Session-Id") {
                    *session = Some(s.to_str()?.to_owned());
                }
                if id.is_none() {
                    return Ok(Value::Null);
                }
                let sse = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.starts_with("text/event-stream"));
                let mut stream = response.bytes_stream();
                let mut buf = Vec::new();
                let mut total = 0usize;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    total += chunk.len();
                    ensure!(total <= 4 * 1024 * 1024, "MCP response too large");
                    buf.extend_from_slice(&chunk);
                    if sse {
                        while let Some(end) = buf.iter().position(|b| *b == b'\n') {
                            let line = buf.drain(..=end).collect::<Vec<_>>();
                            let line = std::str::from_utf8(&line)?.trim();
                            if let Some(data) = line.strip_prefix("data:") {
                                let v: Value = serde_json::from_str(data.trim())?;
                                if v.get("id") == id.as_ref() && v.get("method").is_none() {
                                    return Ok(v);
                                }
                                ensure!(
                                    v.get("method").is_none() || v.get("id").is_none(),
                                    "MCP server requested an unoffered client capability"
                                );
                            }
                        }
                    }
                }
                ensure!(!sse, "MCP stream ended without response");
                Ok(serde_json::from_slice(&buf)?)
            }
            Self::Stdio { input, output, .. } => {
                input.write_all(format!("{msg}\n").as_bytes()).await?;
                input.flush().await?;
                if id.is_none() {
                    return Ok(Value::Null);
                }
                while let Some(v) = crate::rpc::frame(output).await? {
                    if v.get("method").is_some() {
                        if let Some(callback) = v.get("id") {
                            input.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":callback,"error":{"code":-32601,"message":"MCP client capability not offered"}})).as_bytes()).await?;
                            input.flush().await?;
                        }
                        continue;
                    }
                    if v.get("id") == id.as_ref() {
                        return Ok(v);
                    }
                }
                bail!("MCP process closed its output")
            }
        }
    }
}
