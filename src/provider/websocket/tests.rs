use super::*;
use crate::config::{Config, Mode, ProviderAuth};
use crate::store::Store;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, accept_async};

async fn receive(socket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn send(socket: &mut WebSocketStream<TcpStream>, event: Value) {
    socket
        .send(Message::Text(event.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn retained_artwork_recovers_in_same_turn_without_reoffering_hosted_operations() {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        exercise_recovery(None, attributed_failure(), true),
    )
    .await
    .unwrap();
}

fn attributed_failure() -> Value {
    json!({"type":"error","response_id":"failed-edit","error":{"code":"server_error"}})
}

#[tokio::test]
async fn partial_terminal_and_ambiguous_failures_stop_without_a_successor() {
    let cases = [
        (
            Some(
                json!({"type":"response.image_generation_call.partial_image","partial_image_b64":"YWJj"}),
            ),
            attributed_failure(),
        ),
        (
            None,
            json!({"type":"response.failed","response":{"id":"failed-edit","output":[{"type":"image_generation_call","result":"YWJj"}],"error":{"code":"server_error"}}}),
        ),
        (
            None,
            json!({"type":"error","error":{"code":"server_error"}}),
        ),
        (
            None,
            json!({"type":"response.failed","response_id":"other","response":{"id":"failed-edit","output":[],"error":{"code":"server_error"}}}),
        ),
    ];
    for (partial, failure) in cases {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            exercise_recovery(partial, failure, false),
        )
        .await
        .unwrap();
    }
}

async fn exercise_recovery(partial: Option<Value>, failure: Value, recover: bool) {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::initial(dir.path().into(), "fixture".into(), "fixture".into());
    config.provider.endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    config.provider.auth = ProviderAuth::Codex;
    config.provider.codex_account_id = Some("fixture".into());
    let credentials = dir.path().join("synthetic.json");
    std::fs::write(
        &credentials,
        r#"{"access_token":"fixture","account_id":"fixture"}"#,
    )
    .unwrap();
    config.provider.codex_credentials_file = Some(credentials);
    config.provider.native.tools = vec![json!({"type":"image_generation"})];
    let store = Store::open(dir.path().into()).unwrap();
    let session = store.create(dir.path().into(), Mode::ReadOnly).unwrap();
    let inbox = Arc::new(Inbox::new(store, &session.id).unwrap());
    let (agent, _client) = tokio::io::duplex(16384);
    let (reader, writer) = tokio::io::split(agent);
    let connection = amplifier_acp_connection::Connection::agent(reader, writer, 16384)
        .await
        .unwrap();
    let provider = Provider::new(&config).unwrap();
    let scene = json!({"role":"user","content":"Use my previously saved image"});
    let read =
        json!({"type":"function_call","name":"read_image","call_id":"read","arguments":"{}"});
    let result = json!({"type":"function_call_output","call_id":"read","output":"saved original","_tool_images":[{"mimeType":"image/png","data":"YWJj"}]});
    let source = vec![scene.clone(), read.clone(), result.clone()];
    let canonical = history::responses(&source);
    let expected = canonical.clone();
    let server = tokio::spawn(async move {
        let mut first = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        receive(&mut first).await;
        send(
            &mut first,
            json!({"type":"response.created","response":{"id":"read"}}),
        )
        .await;
        send(&mut first,json!({"type":"response.completed","response":{"id":"read","status":"completed","output":[read]}})).await;
        let mut edit = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        let attempted = receive(&mut edit).await;
        assert!(attempted.get("previous_response_id").is_none());
        assert_eq!(attempted["input"], json!(expected));
        assert_eq!(attempted["tools"][0]["type"], "image_generation");
        send(
            &mut edit,
            json!({"type":"response.created","response":{"id":"failed-edit"}}),
        )
        .await;
        if let Some(partial) = partial {
            send(&mut edit, partial).await;
        }
        send(&mut edit, failure).await;
        if !recover {
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
                    .await
                    .is_err(),
                "unexpected successor connection"
            );
            return;
        }
        let mut fresh = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        let recovered = receive(&mut fresh).await;
        assert!(recovered.get("previous_response_id").is_none());
        assert_eq!(recovered["input"], json!(expected));
        assert_eq!(recovered["tools"].as_array().unwrap().len(), 1);
        assert_eq!(recovered["tools"][0]["name"], "read_image");
        send(
            &mut fresh,
            json!({"type":"response.created","response":{"id":"finish"}}),
        )
        .await;
        send(&mut fresh,json!({"type":"response.completed","response":{"id":"finish","status":"completed","output":[]}})).await;
        let last = receive(&mut fresh).await;
        assert_eq!(last["tools"], recovered["tools"]);
        send(
            &mut fresh,
            json!({"type":"response.created","response":{"id":"second-error"}}),
        )
        .await;
        send(
            &mut fresh,
            json!({"type":"error","error":{"code":"server_error"}}),
        )
        .await;
    });
    let mut client = Client::start(
        &provider,
        Peer::new(connection.sender.clone()),
        inbox,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut body = json!({"instructions":"fixture","input":[scene],"tools":[{"type":"image_generation"},{"type":"function","name":"read_image"}]});
    client.complete(body.clone(), vec![]).await.unwrap();
    body["input"] = json!(canonical);
    let error = client
        .complete(body.clone(), vec![result])
        .await
        .unwrap_err();
    if !recover {
        assert!(!error.is::<crate::provider::ResumeArtworkLocally>());
        server.await.unwrap();
        return;
    }
    assert!(
        error.is::<crate::provider::ResumeArtworkLocally>(),
        "{error}"
    );
    assert_eq!(
        client.complete(body.clone(), vec![]).await.unwrap()["status"],
        "completed"
    );
    let final_error = client.complete(body, vec![]).await.unwrap_err();
    assert!(!final_error.is::<crate::provider::ResumeArtworkLocally>());
    server.await.unwrap();
}

#[tokio::test]
async fn recovered_turn_cannot_yield_at_capacity_after_successful_local_read() {
    tokio::time::timeout(std::time::Duration::from_secs(12), run_to_capacity())
        .await
        .unwrap();
}

async fn run_to_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("large"), "A".repeat(65536)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = Config::initial(workspace.clone(), "fixture".into(), "test".into());
    cfg.limits.max_context_bytes = 32768;
    cfg.limits.tool_output_bytes = 100000;
    cfg.provider.endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    cfg.provider.auth = ProviderAuth::Codex;
    cfg.provider.codex_account_id = Some("fixture".into());
    let creds = dir.path().join("credentials");
    std::fs::write(
        &creds,
        r#"{"account_id":"fixture","access_token":"synthetic"}"#,
    )
    .unwrap();
    cfg.provider.codex_credentials_file = Some(creds);
    cfg.provider.native.tools = vec![json!({"type":"image_generation"})];
    let store = Store::open(dir.path().join("engine")).unwrap();
    let session = store.create(workspace, Mode::ReadOnly).unwrap();
    let sid = session.id.clone();
    let inbox = Arc::new(Inbox::new(store.clone(), &sid).unwrap());
    let (agent, _client) = tokio::io::duplex(256 * 1024);
    let (rx, tx) = tokio::io::split(agent);
    let connection = amplifier_acp_connection::Connection::agent(rx, tx, 1024 * 1024)
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        let list = json!({"type":"function_call","name":"workspace_list","arguments":"{\"path\":\".\"}","call_id":"list"});
        let image = json!({"type":"image_generation_call","id":"original","output_format":"png","result":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="});
        let mut first = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        receive(&mut first).await;
        send(
            &mut first,
            json!({"type":"response.created","response":{"id":"original"}}),
        )
        .await;
        send(&mut first,json!({"type":"response.completed","response":{"id":"original","status":"completed","output":[image,list]}})).await;
        let mut edit = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        receive(&mut edit).await;
        send(
            &mut edit,
            json!({"type":"response.created","response":{"id":"failed-edit"}}),
        )
        .await;
        send(&mut edit, attributed_failure()).await;
        let mut local = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        receive(&mut local).await;
        let read = json!({"type":"function_call","name":"workspace_read","arguments":"{\"path\":\"large\"}","call_id":"local-read"});
        send(
            &mut local,
            json!({"type":"response.created","response":{"id":"local"}}),
        )
        .await;
        send(&mut local,json!({"type":"response.completed","response":{"id":"local","status":"completed","output":[read]}})).await;
        while let Some(Ok(msg)) = local.next().await {
            assert!(!msg.is_text(), "unexpected further model call");
        }
    });
    let turn = crate::engine::Turn {
        config: &cfg,
        store: &store,
        session,
        peer: &Peer::new(connection.sender.clone()),
        inbox,
        mcp: Arc::new(crate::mcp::Tools::connect(&[]).await.unwrap()),
        cancel: CancellationToken::new(),
    };
    let outcome = turn
        .run(vec![json!({"type":"text","text":"inspect and edit"})])
        .await;
    let events = store.events(&sid).unwrap();
    let events = events.as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["kind"] == "provider.artwork.recovery"),
        "{outcome:?}"
    );
    let saved = store.load(&sid).unwrap();
    assert!(saved.items.iter().any(|i| {
        i["call_id"] == "local-read"
            && i["output"]
                .as_str()
                .is_some_and(|s| s.contains(&"A".repeat(65536)))
    }));
    assert!(events.iter().any(|e| e["kind"] == "context.rejected"
        && e["data"]["yield_blocker"] == "tool_outcome_uncertain"));
    assert!(!events.iter().any(|e| e["kind"] == "context.limit"));
    assert!(outcome.is_err());
    server.await.unwrap();
}
