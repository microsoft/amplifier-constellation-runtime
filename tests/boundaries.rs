use agent_runtime::{
    config::{Config, Mode},
    engine::recover_pending_tools,
    store::Store,
    tools,
};
use serde_json::json;

fn setup() -> (tempfile::TempDir, Config, Store) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("runtime");
    let work = dir.path().join("workspace");
    std::fs::create_dir_all(&work).unwrap();
    let store = Store::open(home).unwrap();
    let config = Config::initial(work, "Test agent".into(), "fixture".into());
    (dir, config, store)
}
#[tokio::test]
async fn session_roots_are_explicit_confined_and_not_restored_from_config() {
    let (dir, mut config, store) = setup();
    let primary = config.workspace.join("project");
    let sibling = config.workspace.join("private-project");
    let additional = dir.path().join("shared");
    for root in [&primary, &sibling, &additional] {
        std::fs::create_dir(root).unwrap();
    }
    std::fs::write(sibling.join("private.txt"), "not granted").unwrap();
    std::fs::write(additional.join("shared.txt"), "granted").unwrap();
    assert!(
        config
            .for_session(primary.clone(), std::slice::from_ref(&additional))
            .is_err()
    );
    config.additional_workspaces.push(additional.clone());
    config.validate(&store.home).unwrap();
    let session = config
        .for_session(
            std::fs::canonicalize(&primary).unwrap(),
            std::slice::from_ref(&additional),
        )
        .unwrap();
    assert_eq!(
        tools::call(
            &session,
            &primary,
            "workspace_read",
            &json!({"path":additional.join("shared.txt")})
        )
        .await
        .unwrap()["content"],
        "granted"
    );
    assert!(
        tools::call(
            &session,
            &primary,
            "workspace_read",
            &json!({"path":sibling.join("private.txt")})
        )
        .await
        .is_err()
    );
    let resumed = config
        .for_session(std::fs::canonicalize(&primary).unwrap(), &[])
        .unwrap();
    assert!(
        tools::call(
            &resumed,
            &primary,
            "workspace_read",
            &json!({"path":additional.join("shared.txt")})
        )
        .await
        .is_err()
    );
    assert!(
        serde_json::to_value(&session)
            .unwrap()
            .get("session_roots")
            .is_none()
    );
    config
        .additional_workspaces
        .push(store.home.parent().unwrap().to_owned());
    assert!(config.validate(&store.home).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&sibling, additional.join("escape")).unwrap();
        assert!(
            tools::call(
                &session,
                &primary,
                "workspace_read",
                &json!({"path":additional.join("escape/private.txt")})
            )
            .await
            .is_err()
        );
        std::fs::rename(&additional, dir.path().join("moved-shared")).unwrap();
        std::os::unix::fs::symlink(&sibling, &additional).unwrap();
        assert!(session.effective_roots().is_err());
    }
}
#[test]
fn state_is_outside_workspace_and_provider_urls_do_not_leak_credentials() {
    let (_dir, mut c, s) = setup();
    c.validate(&s.home).unwrap();
    c.provider.endpoint = "http://example.com/responses".into();
    assert!(c.validate(&s.home).is_err());
    c.provider.endpoint = "https://user:secret@example.com/responses".into();
    assert!(c.validate(&s.home).is_err());
    c.provider.endpoint = "http://127.0.0.1:8123/responses".into();
    c.validate(&s.home).unwrap();
    c.workspace = s.home.parent().unwrap().to_path_buf();
    assert!(c.validate(&s.home).is_err());
}
#[test]
fn saved_sessions_survive_connections_and_exclude_another_workspace() {
    let (_dir, c, s) = setup();
    let mut a = s.create(c.workspace.clone(), Mode::Ask).unwrap();
    a.items.push(json!({"role":"user","content":"persistent"}));
    s.save(&a).unwrap();
    let reopened = Store::open(s.home.clone()).unwrap();
    assert_eq!(reopened.load(&a.id).unwrap().items, a.items);
    assert_eq!(
        reopened.list(Some(&c.workspace), None).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        reopened.list(Some(&s.home), None).unwrap()["sessions"],
        json!([])
    );
    let _lock = s.lock(&a.id).unwrap();
    assert!(reopened.lock(&a.id).is_err());
    assert!(s.lock("../../outside").is_err());
}
#[test]
fn provider_binding_survives_restart_and_allows_credential_rotation_only() {
    let (_dir, mut c, s) = setup();
    let a = s.create(c.workspace.clone(), Mode::Ask).unwrap();
    let key = c.provider.fingerprint();
    s.bind_provider(&a.id, &key, false).unwrap();
    let reopened = Store::open(s.home.clone()).unwrap();
    c.provider.api_key_env = "ROTATED_KEY".into();
    reopened
        .bind_provider(&a.id, &c.provider.fingerprint(), false)
        .unwrap();
    c.provider.endpoint = "https://different.example/v1/responses".into();
    assert!(
        reopened
            .bind_provider(&a.id, &c.provider.fingerprint(), false)
            .is_err()
    );
    assert_eq!(reopened.load(&a.id).unwrap().items, a.items);
}
#[test]
fn legacy_history_schema_is_migrated_without_rewriting_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("state");
    std::fs::create_dir(&home).unwrap();
    let db = rusqlite::Connection::open(home.join("history.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,cwd TEXT NOT NULL,mode TEXT NOT NULL,title TEXT NOT NULL,updated TEXT NOT NULL,items TEXT NOT NULL);").unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let items = json!([{"role":"user","content":"legacy private context"}]);
    db.execute(
        "INSERT INTO sessions VALUES (?1,?2,'ask','Legacy','2026-11-01',?3)",
        rusqlite::params![id, dir.path().to_string_lossy(), items.to_string()],
    )
    .unwrap();
    drop(db);
    let store = Store::open(home).unwrap();
    assert!(store.bind_provider(&id, "other-provider", false).is_err());
    store.pin_legacy_provider("old-openai").unwrap();
    assert!(store.bind_provider(&id, "other-provider", true).is_err());
    store.bind_provider(&id, "old-openai", false).unwrap();
    assert_eq!(
        serde_json::to_value(store.load(&id).unwrap().items).unwrap(),
        items
    );
    // The released 0.1 binary inserts six values without listing columns.
    // Adding provider metadata must not prevent it creating sessions on rollback.
    let db = rusqlite::Connection::open(store.home.join("history.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO sessions VALUES (?1,?2,'ask','After rollback','2026-11-01','[]')",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            dir.path().to_string_lossy()
        ],
    )
    .unwrap();
}
#[test]
fn interrupted_tool_intent_gets_unknown_result_without_reexecution() {
    let mut items = vec![
        json!({"type":"function_call","call_id":"c1","name":"workspace_write","arguments":"{}"}),
        json!({"type":"function_call","call_id":"c2","name":"clock","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"c2","output":"done"}),
    ];
    recover_pending_tools(&mut items);
    assert_eq!(items.len(), 4);
    assert_eq!(items[3]["call_id"], "c1");
    assert!(items[3]["output"].as_str().unwrap().contains("unknown"));
    recover_pending_tools(&mut items);
    assert_eq!(items.len(), 4);
}
#[tokio::test]
async fn workspace_tools_are_confined_and_bounded() {
    let (_dir, c, _s) = setup();
    tools::call(
        &c,
        &c.workspace,
        "workspace_write",
        &json!({"path":"a/file.txt","content":"hello"}),
    )
    .await
    .unwrap();
    assert_eq!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_read",
            &json!({"path":c.workspace.join("a/file.txt")})
        )
        .await
        .unwrap()["content"],
        "hello"
    );
    assert_eq!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_read",
            &json!({"path":"a/file.txt"})
        )
        .await
        .unwrap()["content"],
        "hello"
    );
    assert!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_read",
            &json!({"path":"../runtime/runtime.json"})
        )
        .await
        .is_err()
    );
    assert!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_write",
            &json!({"path":"/etc/outside","content":"bad"})
        )
        .await
        .is_err()
    );
    let mut small = c.clone();
    small.limits.tool_output_bytes = 2;
    assert!(
        tools::call(
            &small,
            &c.workspace,
            "workspace_read",
            &json!({"path":"a/file.txt"})
        )
        .await
        .is_err()
    );
}
#[cfg(unix)]
#[tokio::test]
async fn symlinks_and_hardlinks_do_not_allow_secret_reads_or_outside_mutation() {
    let (dir, c, _s) = setup();
    let outside = dir.path().join("secret");
    std::fs::write(&outside, "private").unwrap();
    std::os::unix::fs::symlink(&outside, c.workspace.join("link")).unwrap();
    assert!(
        tools::call(&c, &c.workspace, "workspace_read", &json!({"path":"link"}))
            .await
            .is_err()
    );
    assert!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_write",
            &json!({"path":"link","content":"bad"})
        )
        .await
        .is_err()
    );
    std::fs::hard_link(&outside, c.workspace.join("hard")).unwrap();
    assert!(
        tools::call(
            &c,
            &c.workspace,
            "workspace_write",
            &json!({"path":"hard","content":"bad"})
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "private");
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_write_preserves_ordinary_execute_bits_without_privileged_bits() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, c, _s) = setup();
    let path = c.workspace.join("script.sh");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o4750)).unwrap();
    tools::call(
        &c,
        &c.workspace,
        "workspace_write",
        &json!({"path":"script.sh","content":"new"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
        0o750
    );
}

#[cfg(windows)]
#[tokio::test]
async fn atomic_write_does_not_modify_a_windows_hardlink_target() {
    let (dir, c, _s) = setup();
    let outside = dir.path().join("private.txt");
    std::fs::write(&outside, "private").unwrap();
    std::fs::hard_link(&outside, c.workspace.join("hard.txt")).unwrap();
    tools::call(
        &c,
        &c.workspace,
        "workspace_write",
        &json!({"path":"hard.txt","content":"workspace only"}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "private");
    assert_eq!(
        std::fs::read_to_string(c.workspace.join("hard.txt")).unwrap(),
        "workspace only"
    );
}
