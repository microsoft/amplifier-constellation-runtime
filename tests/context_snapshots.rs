use agent_runtime::context::{SNAPSHOT_META, carries_snapshot, project_snapshots, refreshed_view};
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
#[test]
fn projection_alone_refreshes_a_slice_without_pairing_tools() {
    // The call for this result is in a provider window outside the slice. The
    // whole-window tool view decides what to do with it, not this projection.
    let orphan = json!({"type":"function_call_output","call_id":"mcp-call","output":"{\"content\":[{\"type\":\"text\",\"text\":\"{}\"}],\"structuredContent\":{}}"});
    let old = turn("room", "LARGE OLD PROJECTION", "Please create a PR");
    let latest = turn("room", "LATEST PROJECTION", "Can I talk while you work?");
    let mut tail = vec![orphan.clone(), old.clone(), latest.clone()];
    project_snapshots(&mut tail);
    assert_eq!(tail[0], orphan);
    assert_eq!(
        tail[1]["content"][0]["text"],
        "Historical request/context (the refreshed projection is supplied later; original text remains in session_history):\nPlease create a PR"
    );
    assert_eq!(tail[1]["_acp_prompt"], old["_acp_prompt"]);
    assert_eq!(tail[2], latest);
    // Applying it to its own result changes nothing: a replaced block is no
    // longer the declared text, so it is neither replaced again nor counted.
    let once = tail.clone();
    project_snapshots(&mut tail);
    assert_eq!(tail, once);
}
#[test]
fn reasoning_bound_to_a_replaced_prompt_is_left_out() {
    let thinking = json!({"type":"thinking","thinking":"","signature":"SIGNED"});
    let redacted = json!({"type":"redacted_thinking","data":"OPAQUE"});
    let call = json!({"type":"tool_use","id":"a","name":"clock","input":{}});
    let text = json!({"type":"text","text":"Done"});
    let reply = |content: Vec<Value>| json!({"type":"provider_state","provider":"anthropic","message":{"role":"assistant","content":content}});
    let before = reply(vec![thinking.clone(), text.clone()]);
    let first = reply(vec![thinking.clone(), call.clone()]);
    let result = json!({"type":"function_call_output","call_id":"a","output":"12:00"});
    let second = reply(vec![redacted.clone(), thinking.clone(), text.clone()]);
    let silent = reply(vec![thinking.clone()]);
    let other = json!({"type":"provider_state","provider":"gemini","message":{"parts":[{"thought":true,"text":"kept"}]}});
    let current = reply(vec![thinking.clone(), call.clone()]);
    let original = vec![
        json!({"role":"user","content":"An unmarked request"}),
        before.clone(),
        turn("room", "OLD ROOM", "First request"),
        first,
        result.clone(),
        second,
        turn("other-room", "OTHER ROOM", "Unrelated request"),
        silent,
        other.clone(),
        turn("room", "NEW ROOM", "Second request"),
        current.clone(),
    ];
    let view = refreshed_view(&original);
    // Nothing before the replaced prompt changed for this reply.
    assert_eq!(view[1], before);
    // Every reply between the replaced prompt and its refresh loses only its
    // reasoning, including one that follows a prompt that was not replaced.
    assert_eq!(view[3]["message"]["content"], json!([call]));
    assert_eq!(view[4], result);
    assert_eq!(view[5]["message"]["content"], json!([text]));
    assert_eq!(view[6], original[6]);
    assert_eq!(view[7]["message"]["content"], json!([]));
    assert_eq!(view[8], other);
    // The reply to the newest prompt was produced after the replacement.
    assert_eq!(view[10], current);
    assert_eq!(original[3]["message"]["content"][0], thinking);
    // Without a later block for the same key no prompt is replaced, and no
    // reasoning is removed.
    let unrelated = vec![
        turn("room-a", "A", "First"),
        current.clone(),
        turn("room-b", "B", "Second"),
        current.clone(),
    ];
    assert_eq!(refreshed_view(&unrelated), unrelated);
}
#[test]
fn only_a_valid_declaration_marks_a_prompt_as_replaceable() {
    assert!(carries_snapshot(&turn("room", "ROOM", "request")));
    let mut wrong_version = turn("room", "ROOM", "request");
    wrong_version["_acp_prompt"][0]["_meta"][SNAPSHOT_META]["version"] = json!(9);
    assert!(!carries_snapshot(&wrong_version));
    assert!(!carries_snapshot(&json!({"role":"user","content":"plain"})));
    let mut reply = turn("room", "ROOM", "request");
    reply["role"] = json!("assistant");
    assert!(!carries_snapshot(&reply));
}
