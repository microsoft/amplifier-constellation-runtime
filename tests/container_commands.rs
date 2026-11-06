#![cfg(target_os = "linux")]
use agent_runtime::{
    config::{CommandBoundary, CommandNetwork, Config},
    tools,
};
use serde_json::json;

#[tokio::test]
async fn container_commands_require_owner_boundary_and_preserve_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut config = Config::initial(
        dir.path().into(),
        "Container fixture".into(),
        "gpt-6.1-sol".into(),
    );
    assert_eq!(config.command_boundary, CommandBoundary::Workspace);
    config.command_boundary = CommandBoundary::Container;
    assert!(
        config.validate(home.path()).is_err(),
        "network policy must also be explicit"
    );
    config.command_network = CommandNetwork::Inherit;
    if std::env::var("CONTAINER_COMMAND_TEST").as_deref() == Ok("enabled") {
        assert!(
            tools::container_boundary_available(),
            "positive fixture requires real owner boundary"
        );
    }
    if !tools::container_boundary_available() {
        assert!(config.validate(home.path()).is_err());
        assert!(
            tools::call(
                &config,
                dir.path(),
                "workspace_run",
                &json!({"command":"/usr/bin/id","args":["-u"]})
            )
            .await
            .is_err()
        );
        return;
    }
    config.validate(home.path()).unwrap();
    let result = tools::call(&config, dir.path(), "workspace_run", &json!({"command":"/bin/sh","args":["-c","id -u; sudo -n id -u; sudo -n sh -c 'printf fixture > /usr/local/share/constellation-command-test'; cat /usr/local/share/constellation-command-test; sudo -n rm /usr/local/share/constellation-command-test; env"]})).await.unwrap();
    assert_eq!(result["success"], true, "{result}");
    let out = result["stdout"].as_str().unwrap();
    assert!(
        out.starts_with(&format!("{}\n0\nfixture", unsafe { libc::geteuid() })),
        "{out}"
    );
    assert!(!out.contains("COMMAND_TEST_SECRET") && !out.contains("COMMAND_TEST_TOKEN"));
    assert!(tools::definitions_for_config(&config).iter().any(|d| {
        d["description"]
            .as_str()
            .is_some_and(|s| s.contains("sudo -n"))
    }));
    let c = config.clone();
    let task = tokio::spawn(async move {
        tools::call(&c, &c.workspace, "workspace_run", &json!({"command":"/usr/bin/sudo","args":["-n","/bin/sh","-c","(while [ ! -f release ]; do sleep 0.05; done; printf leaked > escaped) & printf started > started; wait"]})).await
    });
    for _ in 0..200 {
        if dir.path().join("started").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(dir.path().join("started").exists());
    task.abort();
    let _ = task.await;
    std::fs::write(dir.path().join("release"), "go").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        !dir.path().join("escaped").exists(),
        "cancelled root child escaped its command lifetime"
    );
    config.limits.command_seconds = 1;
    let timed = tools::call(&config, dir.path(), "workspace_run", &json!({"command":"/usr/bin/sudo","args":["-n","/bin/sh","-c","sleep 2; printf leaked > after-timeout"]})).await;
    assert!(timed.is_err());
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    assert!(!dir.path().join("after-timeout").exists());
}
