use agent_runtime::{context, mcp};
use serde_json::{Value, json};

fn items(result: Value) -> Vec<Value> {
    vec![
        json!({"role":"user","content":"Inspect the supplied receipt; do not retry effects."}),
        json!({"type":"function_call","call_id":"one","name":"mcp_1_context_read","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"one","output":result.to_string(),"_tool_images":[],"_acp_content":[]}),
    ]
}
fn result() -> Value {
    let facts = json!({"world":"selected","submitted":true,"recipient_delivery_verified":false,"unknown":null});
    json!({"content":[{"type":"text","text":facts.to_string()}],"structuredContent":facts,"isError":false})
}
fn output(view: &[Value]) -> Value {
    serde_json::from_str(view[2]["output"].as_str().unwrap()).unwrap()
}

#[test]
fn exact_fallback_is_projected_without_changing_original_or_receipt_fields() {
    let original = items(result());
    let serialized = serde_json::to_vec(&original).unwrap();
    let view = mcp::model_view(&original);
    assert_eq!(output(&view)["content"], json!([]));
    assert_eq!(
        output(&view)["structuredContent"],
        result()["structuredContent"]
    );
    assert_eq!(output(&view)["isError"], false);
    assert_eq!(serde_json::to_vec(&original).unwrap(), serialized);
    assert_eq!(view[2]["_tool_images"], original[2]["_tool_images"]);
    assert_eq!(view[2]["call_id"], original[2]["call_id"]);
    assert_eq!(mcp::model_view(&view), view);
    let history = context::history(
        &original,
        &json!({"start_item":2,"include_tools":true,"limit":1}),
    )
    .unwrap();
    assert_eq!(history["items"][0]["text"], original[2]["output"]);
}

#[test]
fn additional_text_media_errors_annotations_and_unknown_fields_survive() {
    let mut r = result();
    let retained = json!([
        {"type":"text","text":"Fresh caveat, not covered by structured facts"},
        {"type":"image","mimeType":"image/png","data":"preserved"},
        {"type":"resource_link","uri":"https://example.invalid/original","name":"original"},
        {"type":"text","text":r["content"][0]["text"],"annotations":{"audience":["user"]}},
        {"type":"text","text":r["content"][0]["text"],"futureField":true}
    ]);
    r["content"]
        .as_array_mut()
        .unwrap()
        .extend(retained.as_array().unwrap().clone());
    r["isError"] = json!(true);
    r["_meta"] = json!({"origin":"private-evidence"});
    r["futureField"] = json!({"unknown":true});
    let projected = output(&mcp::model_view(&items(r.clone())));
    assert_eq!(projected["content"], retained);
    for key in ["structuredContent", "isError", "_meta", "futureField"] {
        assert_eq!(projected[key], r[key]);
    }
}

#[test]
fn uncertain_or_noncanonical_text_is_never_removed() {
    for text in [
        "a separate observation",
        "{\"submitted\":false,\"submitted\":true}",
        "{broken JSON",
        &serde_json::to_string_pretty(&result()["structuredContent"]).unwrap(),
    ] {
        let mut r = result();
        r["content"][0]["text"] = json!(text);
        let original = items(r);
        assert_eq!(mcp::model_view(&original), original);
    }
}

#[test]
fn ordinary_tool_outputs_and_unmatched_calls_are_unchanged() {
    for name in [
        "context_read",
        "workspace_read",
        "mcp_x_read",
        "mcp__read",
        "mcp_1_",
    ] {
        let mut original = items(result());
        original[1]["name"] = json!(name);
        assert_eq!(mcp::model_view(&original), original);
    }
    let mut original = items(result());
    original[2]["call_id"] = json!("another");
    assert_eq!(mcp::model_view(&original), original);
    for name in ["workspace_read", "mcp_1_context_read"] {
        let mut original = items(result());
        original.insert(
            1,
            json!({"type":"function_call","call_id":"one","name":name,"arguments":"{}"}),
        );
        assert_eq!(mcp::model_view(&original), original);
    }
}

#[test]
fn invalid_or_truncated_envelopes_remain_verbatim() {
    for raw in [
        "{truncated",
        "null",
        "[]",
        "{\"structuredContent\":{}}",
        "{\"content\":[],\"structuredContent\":null}",
    ] {
        let mut original = items(result());
        original[2]["output"] = json!(raw);
        assert_eq!(mcp::model_view(&original), original);
    }
}

#[test]
fn byte_budget_uses_unique_facts_before_discarding_history() {
    let facts = json!({"receipt":"evidence".repeat(1000),"completed":false});
    let original = items(
        json!({"content":[{"type":"text","text":facts.to_string()}],"structuredContent":facts}),
    );
    let projected = mcp::model_view(&original);
    let budget = agent_runtime::media::context_bytes(&projected).unwrap() + 16;
    assert!(agent_runtime::media::context_bytes(&original).unwrap() > budget);
    let view = context::prepare(&original, &context::Policy::default(), budget, 0).unwrap();
    assert!(view.checkpoint.is_none());
    assert_eq!(view.items, projected);
}

#[test]
fn provider_private_continuation_is_never_rewritten() {
    let mut original = items(result());
    original.push(json!({"type":"provider_state","provider":"anthropic","message":{"content":[{"type":"thinking","signature":"opaque","thinking":result().to_string()}]}}));
    let view = mcp::model_view(&original);
    assert_eq!(view[3], original[3]);
}
