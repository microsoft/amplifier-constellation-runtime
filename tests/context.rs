use agent_runtime::{
    config::{Config, Mode, ProviderKind},
    context::{Policy, history, prepare},
    management,
    store::Store,
};
use serde_json::json;

#[test]
fn large_archives_compact_without_discarding_original_messages() {
    let items = vec![
        json!({"role":"user","content":"archived detail ".repeat(3_500_000)}),
        json!({"role":"assistant","content":"Previous work saved."}),
        json!({"role":"user","content":"Inspect the remaining work."}),
    ];
    assert!(!agent_runtime::media::request_fits(&items).unwrap());
    let original_len = items[0]["content"].as_str().unwrap().len();
    let view = prepare(&items, &Policy::default(), 16000).unwrap();
    assert!(agent_runtime::media::request_fits(&view.items).unwrap());
    assert_eq!(view.items.last(), items.last());
    assert_eq!(items[0]["content"].as_str().unwrap().len(), original_len);
    assert_eq!(view.checkpoint.unwrap()["full_history_preserved"], true);
    assert!(
        prepare(
            &items,
            &Policy {
                auto_compact: false,
                ..Policy::default()
            },
            16000
        )
        .is_err()
    );
}

#[test]
fn small_media_estimate_does_not_bypass_serialized_request_limit() {
    let image = json!({"type":"input_image","image_url":format!("data:image/png;base64,{}", "A".repeat(49 * 1024 * 1024))});
    let current = json!({"role":"user","content":[image]});
    assert!(agent_runtime::media::context_bytes(std::slice::from_ref(&current)).unwrap() < 16000);
    assert!(prepare(std::slice::from_ref(&current), &Policy::default(), 16000).is_err());
    let items = vec![current, json!({"role":"user","content":"New task"})];
    let view = prepare(&items, &Policy::default(), 16000).unwrap();
    assert!(agent_runtime::media::request_fits(&view.items).unwrap());
}

#[test]
fn compaction_preserves_native_tool_pairs_and_complete_original_history() {
    let mut items = vec![];
    for i in 0..8 {
        items.push(
            json!({"role":"user","content":format!("request {i}: {}", "detail ".repeat(800))}),
        );
        items.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("reported result {i}")}]}));
    }
    items.push(json!({"role":"user","content":"Current task"}));
    let native = json!({"type":"provider_state","provider":"anthropic","message":{"content":[{"type":"thinking","thinking":"private reasoning","signature":"signed"},{"type":"tool_use","id":"c1","name":"clock","input":{}}]}});
    items.push(native.clone());
    items.push(json!({"type":"function_call","call_id":"c1","name":"clock","arguments":"{}","_provider_shadow":true}));
    items.push(json!({"type":"function_call_output","call_id":"c1","output":"actual observation"}));
    let original = items.clone();
    let view = prepare(&items, &Policy::default(), 7000).unwrap();
    assert_eq!(items, original);
    assert!(view.items.contains(&native));
    assert!(
        view.items
            .iter()
            .any(|v| v["type"] == "function_call" && v["call_id"] == "c1")
    );
    assert!(
        view.items
            .iter()
            .any(|v| v["type"] == "function_call_output" && v["call_id"] == "c1")
    );
    assert!(view.checkpoint.as_ref().unwrap()["full_history_preserved"] == true);
    assert!(!view.items[0].to_string().contains("private reasoning"));
    let archive = history(&items, &json!({"query":"request 0"})).unwrap();
    assert_eq!(archive["items"][0]["item_index"], 0);
    assert!(
        !history(&items, &json!({"include_tools":true}))
            .unwrap()
            .to_string()
            .contains("private reasoning")
    );
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().to_owned()).unwrap();
    let mut session = store.create(temp.path().join("work"), Mode::Ask).unwrap();
    session.items = items;
    store.save(&session).unwrap();
    assert!(
        store
            .checkpoint(&session.id, "provider1", view.checkpoint.as_ref().unwrap())
            .unwrap()
    );
    assert!(
        !store
            .checkpoint(&session.id, "provider1", view.checkpoint.as_ref().unwrap())
            .unwrap()
    );
    assert_eq!(store.load(&session.id).unwrap().items, original);
}

#[test]
fn active_turn_is_never_pruned_and_archives_are_retrievable_in_utf8_chunks() {
    let large = "🦊".repeat(5000);
    let items = vec![
        json!({"role":"user","content":"old task"}),
        json!({"role":"user","content":large}),
    ];
    assert!(prepare(&items, &Policy::default(), 4096).is_err());
    let first = history(&items, &json!({"start_item":1,"limit":1})).unwrap();
    let next = first["items"][0]["next_text_offset"].as_u64().unwrap();
    let second = history(
        &items,
        &json!({"start_item":1,"text_offset":next,"limit":1}),
    )
    .unwrap();
    assert_eq!(second["items"][0]["text_offset"], next);
    assert_eq!(first["items"][0]["text"].as_str().unwrap().len(), 4096);
}

#[test]
fn configuration_rejects_stale_writes_and_preserves_provider_binding() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("state");
    let work = temp.path().join("work");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&work).unwrap();
    let config = Config::initial(work.clone(), "One".into(), "fixture".into());
    std::fs::write(
        home.join("runtime.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let store = Store::open(home.clone()).unwrap();
    let mut session = store.create(work, Mode::Ask).unwrap();
    session.items = vec![json!({"role":"user","content":"preserved"})];
    store.save(&session).unwrap();
    let old = management::inspect(&home).unwrap();
    let revision = old["revision"].as_str().unwrap();
    let mut replacement = config.clone();
    replacement.provider =
        agent_runtime::config::Provider::preset(ProviderKind::Mock, "mock".into());
    let result = management::configure(&home, &replacement, revision).unwrap();
    assert_eq!(result["provider_changed"], true);
    assert!(management::configure(&home, &replacement, revision).is_err());
    assert!(
        store
            .bind_provider(&session.id, &replacement.provider.fingerprint(), false)
            .is_err()
    );
    assert_eq!(store.load(&session.id).unwrap().items, session.items);
    assert!(management::schema()["$defs"]["Native"].is_object());
}
