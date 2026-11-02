#![cfg(unix)]
use agent_runtime::{config::Config, tools};
use serde_json::json;

#[tokio::test]
async fn commands_follow_the_current_sessions_root_grant() {
    if tools::command_isolation().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Session grants use canonical paths. On macOS the temporary-directory
    // spelling can pass through /var, an alias outside the Seatbelt grant.
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let primary = canonical.join("primary");
    let shared = canonical.join("shared");
    let excluded = canonical.join("excluded");
    for root in [&primary, &shared, &excluded] {
        std::fs::create_dir(root).unwrap();
    }
    std::fs::write(shared.join("data"), "shared-visible").unwrap();
    std::fs::write(excluded.join("data"), "private-marker").unwrap();
    let mut config = Config::initial(primary.clone(), "Multi-root".into(), "fixture".into());
    config.additional_workspaces = vec![shared.clone()];
    let session = config
        .for_session(
            std::fs::canonicalize(&primary).unwrap(),
            std::slice::from_ref(&shared),
        )
        .unwrap();
    let read = json!({"command":"/bin/cat","args":[shared.join("data")]});
    let result = tools::call(&session, &primary, "workspace_run", &read)
        .await
        .unwrap();
    assert_eq!(result["stdout"], "shared-visible", "{result}");
    let result = tools::call(
        &session,
        &primary,
        "workspace_run",
        &json!({"command":"/bin/cat","args":[excluded.join("data")]}),
    )
    .await
    .unwrap();
    assert_eq!(result["success"], false);
    assert!(
        !result["stdout"]
            .as_str()
            .unwrap()
            .contains("private-marker")
    );
    let resumed = config
        .for_session(std::fs::canonicalize(&primary).unwrap(), &[])
        .unwrap();
    assert_eq!(
        tools::call(&resumed, &primary, "workspace_run", &read)
            .await
            .unwrap()["success"],
        false
    );
}

#[tokio::test]
async fn isolated_commands_keep_file_network_and_environment_boundaries() {
    if tools::command_isolation().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work with spaces");
    std::fs::create_dir(&work).unwrap();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, "private").unwrap();
    std::os::unix::fs::symlink(&outside, work.join("escape")).unwrap();
    let c = Config::initial(work.clone(), "Command test".into(), "fixture".into());
    let args = json!({"command":"/bin/sh","args":["-c","printf hello > note; cat note; env"]});
    let result = tools::call(&c, &work, "workspace_run", &args)
        .await
        .unwrap();
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(std::fs::read_to_string(work.join("note")).unwrap(), "hello");
    let out = result["stdout"].as_str().unwrap();
    assert!(out.starts_with("hello"));
    assert!(!out.contains("API_KEY") && !out.contains("TOKEN") && !out.contains("SSH_AUTH_SOCK"));
    for path in [outside.clone(), work.join("escape")] {
        let result = tools::call(
            &c,
            &work,
            "workspace_run",
            &json!({"command":"/bin/cat","args":[path]}),
        )
        .await
        .unwrap();
        assert_eq!(result["success"], false, "{result}");
        assert!(!result["stdout"].as_str().unwrap().contains("private"));
        let result = tools::call(
            &c,
            &work,
            "workspace_run",
            &json!({"command":"/bin/sh","args":["-c","printf changed > \"$1\"","sh",path]}),
        )
        .await
        .unwrap();
        // Linux's private /tmp may allow writing a different file at the same
        // spelling. The host file must remain unchanged on both platforms.
        #[cfg(target_os = "macos")]
        assert_eq!(result["success"], false, "{result}");
        #[cfg(not(target_os = "macos"))]
        let _ = result;
    }
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "private");
    assert!(
        tools::call(&c, dir.path(), "workspace_run", &args)
            .await
            .is_err()
    );

    #[cfg(target_os = "macos")]
    {
        // A real listening socket establishes that refusal is confinement,
        // rather than an unavailable remote host or missing DNS configuration.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        let result = tools::call(
            &c,
            &work,
            "workspace_run",
            &json!({"command":"/usr/bin/nc","args":["-z","-w","1","127.0.0.1",port]}),
        )
        .await
        .unwrap();
        assert_eq!(result["success"], false, "{result}");
        assert!(listener.accept().is_err());
        // A second spelling of the APFS data volume must not bypass the
        // canonical path restriction.
        let data_alias =
            std::path::Path::new("/System/Volumes/Data").join(outside.strip_prefix("/").unwrap());
        if data_alias.is_file() {
            let result = tools::call(
                &c,
                &work,
                "workspace_run",
                &json!({"command":"/bin/cat","args":[data_alias]}),
            )
            .await
            .unwrap();
            assert_eq!(result["success"], false, "{result}");
        }
    }
}

#[tokio::test]
async fn cancellation_kills_descendants() {
    if tools::command_isolation().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = Config::initial(
        dir.path().to_owned(),
        "Cancellation".into(),
        "fixture".into(),
    );
    let task = tokio::spawn(async move {
        tools::call(&c, &c.workspace, "workspace_run", &json!({"command":"/bin/sh","args":["-c","(while [ ! -f release ]; do sleep 0.05; done; printf leaked > escaped) & printf started > started; wait"]})).await
    });
    for _ in 0..100 {
        if dir.path().join("started").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(dir.path().join("started").exists());
    task.abort();
    let _ = task.await;
    std::fs::write(dir.path().join("release"), "go").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!dir.path().join("escaped").exists());
}
