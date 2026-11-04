use agent_runtime::context::{SNAPSHOT_META, refreshed_view};
use serde_json::{Value, json};
fn turn(key: &str, text: &str, request: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}],"_acp_prompt":[{"type":"text","text":text,"_meta":{SNAPSHOT_META:{"version":1,"key":key,"retainedText":request}}}]})
}
#[test]
fn refresh_keeps_every_request_tool_result_and_original() {
    let old = turn("room-a", "LARGE OLD PROJECTION", "Please create a PR");
    let call =
        json!({"type":"function_call","call_id":"a","name":"workspace_run","arguments":"{}"});
    let result = json!({"type":"function_call_output","call_id":"a","output":"Branch created"});
    let other = turn("room-b", "OTHER ROOM", "Other request");
    let latest = turn("room-a", "LATEST PROJECTION", "Can I talk while you work?");
    let original = vec![
        old.clone(),
        call.clone(),
        result.clone(),
        other.clone(),
        latest.clone(),
    ];
    let view = refreshed_view(&original);
    assert_eq!(view.len(), original.len());
    assert!(
        view[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Please create a PR")
    );
    assert!(
        !view[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("LARGE OLD")
    );
    assert_eq!(view[1], call);
    assert_eq!(view[2], result);
    assert_eq!(view[3], other);
    assert_eq!(view[4], latest);
    assert_eq!(original[0], old);
    assert_eq!(view[0]["_acp_prompt"], old["_acp_prompt"]);
}
#[test]
fn unmarked_malformed_and_multimedia_content_is_not_dropped() {
    let mut a = turn("same", "A", "request");
    a["_acp_prompt"][0]["_meta"][SNAPSHOT_META]["version"] = json!(9);
    let mut b = turn("same", "B", "request");
    b["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"input_image","image_url":"data:example"}));
    let opaque = json!({"type":"provider_state","state":"OPAQUE"});
    let v = vec![a, b, opaque, turn("same", "LATEST", "latest")];
    assert_eq!(refreshed_view(&v), v);
}
