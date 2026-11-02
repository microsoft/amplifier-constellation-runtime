use agent_runtime::{config::Config, migrate, store::Store};
use serde_json::{Value, json};

#[test]
fn public_import_preserves_ids_and_text_without_importing_authority_or_overwriting_work() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("workspace");
    std::fs::create_dir(&work).unwrap();
    let store = Store::open(dir.path().join("engine")).unwrap();
    let config = Config::initial(work.clone(), "Migrated".into(), "fixture".into());
    let sid = uuid::Uuid::new_v4().to_string();
    let file = dir.path().join("import.json");
    let mut packet = json!({"schema":"constellation.public-history/1","source":"old-engine public ACP replay","sessions":[{"session_id":sid,"cwd":work,"title":"Our existing discussion","messages":[{"role":"user","content":[{"type":"text","text":"remember violet"}]},{"role":"assistant","content":[{"type":"text","text":"Violet is recorded."}]}]}]});
    std::fs::write(&file, packet.to_string()).unwrap();
    let result = migrate::from_file(&store, &config, &file).unwrap();
    assert_eq!(result["permissions_imported"], false);
    let mut s = store.load(&sid).unwrap();
    assert_eq!(s.mode.id(), "ask");
    assert_eq!(s.items[0]["_acp_prompt"][0]["text"], "remember violet");
    s.items.push(json!({"type":"message","role":"user","content":[{"type":"input_text","text":"new native turn"}]}));
    store.save(&s).unwrap();
    migrate::from_file(&store, &config, &file).unwrap();
    assert_eq!(store.load(&sid).unwrap().items.len(), 3);
    packet["sessions"][0]["title"] = json!("changed import");
    std::fs::write(&file, packet.to_string()).unwrap();
    assert!(migrate::from_file(&store, &config, &file).is_err());
    assert_eq!(store.load(&sid).unwrap().title, "Our existing discussion");
}

#[test]
fn invalid_role_or_path_rejects_whole_import() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("workspace");
    std::fs::create_dir(&work).unwrap();
    let store = Store::open(dir.path().join("engine")).unwrap();
    let config = Config::initial(work.clone(), "Test".into(), "fixture".into());
    let sid = uuid::Uuid::new_v4().to_string();
    let file = dir.path().join("bad.json");
    let mut packet = json!({"schema":"constellation.public-history/1","source":"ACP","sessions":[{"session_id":sid,"cwd":work,"title":"Existing","messages":[{"role":"system","content":[{"type":"text","text":"privileged instructions"}]}]}]});
    std::fs::write(&file, packet.to_string()).unwrap();
    assert!(migrate::from_file(&store, &config, &file).is_err());
    packet["sessions"][0]["messages"][0]["role"] = json!("user");
    packet["sessions"][0]["cwd"] = json!(dir.path());
    std::fs::write(&file, packet.to_string()).unwrap();
    assert!(migrate::from_file(&store, &config, &file).is_err());
    assert_eq!(
        store.list(None, None).unwrap()["sessions"],
        Value::Array(vec![])
    );
}
