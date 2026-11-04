#![cfg(target_os = "linux")]
use agent_runtime::{
    config::{CommandNetwork, Config},
    tools,
};
use serde_json::json;

#[tokio::test]
async fn owner_can_enable_network_without_exposing_host_files_or_environment() {
    assert_eq!(
        Config::initial("/tmp".into(), "fixture".into(), "fixture".into()).command_network,
        CommandNetwork::Isolated
    );
    if tools::command_isolation().is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("workspace");
    std::fs::create_dir(&work).unwrap();
    let private = root.path().join("private");
    std::fs::write(&private, "PRIVATE_SENTINEL").unwrap();
    std::os::unix::fs::symlink(&private, work.join("escape")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = Config::initial(work.clone(), "fixture".into(), "fixture".into());
    let command = json!({"command":"/usr/bin/python3", "args":["-c",format!("import socket; socket.create_connection(('127.0.0.1',{port}),timeout=1).close()") ]});
    let blocked = tools::call(&config, &work, "workspace_run", &command)
        .await
        .unwrap();
    assert_eq!(blocked["success"], false, "{blocked}");
    assert!(listener.accept().is_err());
    config.command_network = CommandNetwork::Inherit;
    let allowed = tools::call(&config, &work, "workspace_run", &command)
        .await
        .unwrap();
    assert_eq!(allowed["success"], true, "{allowed}");
    assert!(listener.accept().is_ok());
    for path in [&private, &work.join("escape")] {
        let denied = tools::call(
            &config,
            &work,
            "workspace_run",
            &json!({"command":"/bin/cat","args":[path]}),
        )
        .await
        .unwrap();
        assert_eq!(denied["success"], false, "{denied}");
        assert!(
            !denied["stdout"]
                .as_str()
                .unwrap()
                .contains("PRIVATE_SENTINEL")
        );
    }
    let env = tools::call(
        &config,
        &work,
        "workspace_run",
        &json!({"command":"/usr/bin/env","args":[]}),
    )
    .await
    .unwrap();
    assert!(!env["stdout"].as_str().unwrap().contains("TOKEN="));
    assert!(!env["stdout"].as_str().unwrap().contains("API_KEY="));
    let netfiles = tools::call(&config, &work, "workspace_run", &json!({"command":"/bin/sh","args":["-c","test -r /etc/resolv.conf && test -r /etc/ssl/certs/ca-certificates.crt && ! test -e /etc/shadow"]})).await.unwrap();
    assert_eq!(netfiles["success"], true, "{netfiles}");
    let desc = tools::definitions_for_network(config.command_network);
    assert!(
        desc.iter().find(|t| t["name"] == "workspace_run").unwrap()["description"]
            .as_str()
            .unwrap()
            .contains("Networking uses")
    );
    config.command_network = CommandNetwork::Isolated;
    let blocked_again = tools::call(&config, &work, "workspace_run", &command)
        .await
        .unwrap();
    assert_eq!(blocked_again["success"], false, "{blocked_again}");
}
