use agent_runtime::mcp::Tools;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify},
    task::{JoinHandle, JoinSet},
};

struct HttpFixture {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
    release: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpFixture {
    async fn start(ignore_cancellation: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let copies = (
            requests.clone(),
            started.clone(),
            cancelled.clone(),
            release.clone(),
        );
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    stream = listener.accept() => {
                        let (stream, _) = stream.unwrap();
                        let (requests, started, cancelled, release) = copies.clone();
                        clients.spawn(async move {
                            serve_http(stream, requests, started, cancelled, release, ignore_cancellation).await;
                        });
                    }
                    result = clients.join_next(), if !clients.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            url,
            requests,
            started,
            cancelled,
            release,
            task,
        }
    }

    async fn connect(&self) -> Arc<Tools> {
        Arc::new(
            Tools::connect(&[json!({"type":"http","name":"fixture","url":self.url,
                "headers":[{"name":"Authorization","value":"Bearer fixture-only"}]})])
            .await
            .unwrap(),
        )
    }
}

async fn serve_http(
    stream: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
    release: Arc<Notify>,
    ignore_cancellation: bool,
) {
    let mut stream = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await.unwrap() == 0 {
            return;
        }
        headers.push_str(&line.to_ascii_lowercase());
        if line == "\r\n" {
            break;
        }
    }
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    let message: Value = serde_json::from_slice(&body).unwrap();
    assert!(headers.contains("authorization: bearer fixture-only"));
    if message["method"] != "initialize" {
        assert!(headers.contains("mcp-session-id: fixture-session"));
    }
    requests.lock().await.push(message.clone());
    let result = match message["method"].as_str().unwrap() {
        "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}),
        "tools/list" => json!({"tools":[
            {"name":"slow","inputSchema":{"type":"object"}},
            {"name":"fast","inputSchema":{"type":"object"}}
        ]}),
        "tools/call" if message["params"]["name"] == "slow" => {
            started.notify_one();
            release.notified().await;
            json!({"content":[{"type":"text","text":"slow tool stopped waiting"}]})
        }
        "tools/call" => json!({"content":[{"type":"text","text":"new operation"}]}),
        "notifications/cancelled" => {
            cancelled.notify_one();
            if ignore_cancellation {
                // A server can ignore cancellation and keep performing work.
                // It also withholds the HTTP acknowledgement in this case.
                std::future::pending::<()>().await;
            }
            release.notify_one();
            Value::Null
        }
        "notifications/initialized" => Value::Null,
        other => panic!("Unexpected MCP method {other}"),
    };
    let (status, body) = if message.get("id").is_some() {
        (
            "200 OK",
            json!({"jsonrpc":"2.0","id":message["id"],"result":result}).to_string(),
        )
    } else {
        ("202 Accepted", String::new())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: fixture-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The interrupted request's client may already have closed its socket.
    let _ = stream.get_mut().write_all(response.as_bytes()).await;
}

async fn interrupt_slow(tools: Arc<Tools>, started: &Notify) {
    let call = tokio::spawn(async move { tools.call("mcp_0_slow", json!({})).await });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn actual_http_tool_receives_cancellation_without_replay() {
    let fixture = HttpFixture::start(false).await;
    let tools = fixture.connect().await;
    interrupt_slow(tools.clone(), &fixture.started).await;

    // Until cleanup runs, even another call is rejected rather than pretending
    // that the interrupted operation completed.
    assert!(
        tools
            .call("mcp_0_fast", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome is unknown")
    );
    let reports = tools.cancel_pending().await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].request_id, 3);
    assert_eq!(reports[0].transport, "http");
    assert!(reports[0].notification_sent);
    assert!(!reports[0].quarantined);
    assert!(reports[0].outcome_uncertain);
    tokio::time::timeout(Duration::from_secs(1), fixture.cancelled.notified())
        .await
        .unwrap();

    let result = tools.call("mcp_0_fast", json!({})).await.unwrap();
    assert_eq!(result["content"][0]["text"], "new operation");
    assert!(tools.cancel_pending().await.is_empty());
    let requests = fixture.requests.lock().await;
    let cancellation = requests
        .iter()
        .find(|r| r["method"] == "notifications/cancelled")
        .unwrap();
    assert_eq!(cancellation["params"]["requestId"], 3);
    assert!(cancellation.get("id").is_none());
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tools/call" && r["params"]["name"] == "slow")
            .count(),
        1
    );
}

#[tokio::test]
async fn unacknowledged_http_cancellation_is_bounded_and_remains_uncertain() {
    let fixture = HttpFixture::start(true).await;
    let tools = fixture.connect().await;
    interrupt_slow(tools.clone(), &fixture.started).await;
    let reports = tokio::time::timeout(Duration::from_secs(2), tools.cancel_pending())
        .await
        .expect("cleanup must not wait for the remote operation");
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].notification_sent);
    assert!(reports[0].outcome_uncertain);
    assert!(!reports[0].quarantined);
    assert!(tools.cancel_pending().await.is_empty());
    assert_eq!(
        fixture
            .requests
            .lock()
            .await
            .iter()
            .filter(|r| r["method"] == "tools/call")
            .count(),
        1
    );
    // The remote request can finish later; cleanup does not claim otherwise.
    fixture.release.notify_one();
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_partial_stdio_frame_is_cancelled_and_quarantined() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("server.sh");
    let started = directory.path().join("started");
    let cancelled = directory.path().join("cancelled.json");
    std::fs::write(
        &script,
        r#"
while IFS= read -r message; do
    case "$message" in
        *'"method":"initialize"'*)
            printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}'
            ;;
        *'"method":"tools/list"'*)
            printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"slow","inputSchema":{"type":"object"}}]}}'
            ;;
        *'"method":"tools/call"'*)
            printf '%s' '{"jsonrpc":"2.0","id":3,'
            printf 'started' > "$FIXTURE_STARTED"
            ;;
        *'"method":"notifications/cancelled"'*)
            printf '%s\n' "$message" > "$FIXTURE_CANCELLED"
            exit 0
            ;;
    esac
done
"#,
    )
    .unwrap();
    let tools = Arc::new(
        Tools::connect(&[json!({"type":"stdio","name":"partial-frame-fixture",
        "command":"/bin/sh","args":[script],"env":[
            {"name":"FIXTURE_STARTED","value":started},
            {"name":"FIXTURE_CANCELLED","value":cancelled}
        ]})])
        .await
        .unwrap(),
    );
    let calling_tools = tools.clone();
    let call = tokio::spawn(async move { calling_tools.call("mcp_0_slow", json!({})).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());

    let reports = tools.cancel_pending().await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].transport, "stdio");
    assert!(reports[0].notification_sent);
    assert!(reports[0].quarantined);
    assert!(reports[0].outcome_uncertain);
    let notification: Value = serde_json::from_slice(&std::fs::read(cancelled).unwrap()).unwrap();
    assert_eq!(notification["method"], "notifications/cancelled");
    assert_eq!(notification["params"]["requestId"], 3);

    let error = tools.call("mcp_0_slow", json!({})).await.unwrap_err();
    assert!(error.to_string().contains("quarantined"));
    assert!(tools.cancel_pending().await.is_empty());
}

#[tokio::test]
async fn optional_failure_keeps_following_server_alias_and_required_failure_stays_fatal() {
    let fixture = HttpFixture::start(false).await;
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", dead.local_addr().unwrap());
    drop(dead);
    let mut broken = json!({"type":"http","name":"optional-world","url":url,"headers":[{"name":"Authorization","value":"PRIVATE-TOKEN"}],"_meta":{"io.github.bkrabach.agent-runtime":{"optional":true}}});
    let healthy = json!({"type":"http","name":"project","url":fixture.url,"headers":[{"name":"Authorization","value":"Bearer fixture-only"}]});
    let tools = Tools::connect(&[broken.clone(), healthy.clone()])
        .await
        .unwrap();
    assert_eq!(tools.unavailable.len(), 1);
    assert!(
        !serde_json::to_string(&tools.unavailable)
            .unwrap()
            .contains("PRIVATE-TOKEN")
    );
    assert!(
        !serde_json::to_string(&tools.unavailable)
            .unwrap()
            .contains(&url)
    );
    assert!(tools.tools.contains_key("mcp_1_fast"));
    assert_eq!(
        tools.call("mcp_1_fast", json!({})).await.unwrap()["content"][0]["text"],
        "new operation"
    );
    let tools = Arc::new(tools);
    let calling = tools.clone();
    let call = tokio::spawn(async move { calling.call("mcp_1_slow", json!({})).await });
    tokio::time::timeout(Duration::from_secs(3), fixture.started.notified())
        .await
        .unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    let reports = tools.cancel_pending().await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_index, 1);
    broken.as_object_mut().unwrap().remove("_meta");
    assert!(Tools::connect(&[broken, healthy]).await.is_err());
}
