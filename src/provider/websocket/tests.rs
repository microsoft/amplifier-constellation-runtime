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
    tokio::time::timeout(std::time::Duration::from_secs(10), exercise_recovery())
        .await
        .unwrap();
}

async fn exercise_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::initial(dir.path().into(), "fixture".into(), "fixture".into());
    config.provider.endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
    config.provider.auth = ProviderAuth::Codex;
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
        send(
            &mut edit,
            json!({"type":"error","error":{"code":"server_error"}}),
        )
        .await;
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
