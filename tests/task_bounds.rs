use agent_runtime::{continuation, task_bounds::Bound, watches};
use serde_json::{Value, json};

fn continuation_input() -> Value {
    serde_json::from_str(include_str!("../examples/helpers/continuation-v2.json")).unwrap()
}

fn advice(value: Value) -> Value {
    continuation::assess_v2(serde_json::from_value(value).unwrap()).unwrap()
}

#[test]
fn unconfigured_limits_preserve_status_receipt_and_recovery_constraints() {
    let base = continuation_input();
    assert_eq!(advice(base.clone())["action"], "continue");
    assert_eq!(advice(base.clone())["policy"], "bounded-continuation/2");
    for status in [
        "stopped",
        "cancelled",
        "review",
        "blocked",
        "budget_exhausted",
        "unknown",
    ] {
        let mut input = base.clone();
        input["status"] = json!(status);
        assert_eq!(advice(input)["action"], "hold");
    }
    for flag in [
        "idle",
        "observation_current",
        "receipts_complete",
        "actions_succeeded",
    ] {
        let mut input = base.clone();
        input["status"] = json!("needs_review");
        input[flag] = json!(false);
        assert_ne!(advice(input)["action"], "inspect_then_continue");
    }
    let mut input = base;
    input["status"] = json!("needs_review");
    input["recovery_attempts"] = json!(2);
    assert_eq!(advice(input)["action"], "review");
}

#[test]
fn finite_dimensions_still_stop_work_when_the_other_is_unconfigured() {
    for (field, value) in [("remaining_ms", 119_999), ("remaining_turns", 0)] {
        let mut input = continuation_input();
        input[field] = json!({"kind":"finite","value":value});
        assert_eq!(advice(input)["action"], "checkpoint");
    }
    let mut input = continuation_input();
    input["remaining_ms"] = json!({"kind":"finite","value":120_000});
    input["remaining_turns"] = json!({"kind":"finite","value":1});
    let answer = advice(input);
    assert_eq!(answer["action"], "continue");
    assert_eq!(answer["extend_budget"], false);
    assert_eq!(answer["replay_actions"], false);
}

#[test]
fn missing_null_unknown_and_numeric_v2_limits_are_rejected() {
    for field in ["remaining_ms", "remaining_turns"] {
        for invalid in [
            Value::Null,
            json!(123),
            json!({"kind":"unknown"}),
            json!({}),
        ] {
            let mut input = continuation_input();
            input[field] = invalid;
            assert!(
                serde_json::from_value::<continuation::Input<Bound<u64>, Bound<u32>>>(input)
                    .is_err()
            );
        }
        let mut input = continuation_input();
        input.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<continuation::Input<Bound<u64>, Bound<u32>>>(input).is_err()
        );
    }
    assert!(serde_json::from_value::<continuation::Input>(continuation_input()).is_err());
}

fn watch_input() -> Value {
    serde_json::from_str(include_str!("../examples/helpers/watch-v2.json")).unwrap()
}

fn apply(path: &std::path::Path, value: Value, now: u64) -> Value {
    watches::apply(
        path,
        serde_json::from_value::<watches::Input<watches::TaskV2>>(value).unwrap(),
        now,
    )
    .unwrap()
}

#[test]
fn completion_bound_watch_keeps_finite_ttl_across_reload_and_does_not_revive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watches.json");
    let mut input = watch_input();
    input["task"]["remaining_turns"] = json!({"kind":"no_configured_bound"});
    let armed = apply(&path, input.clone(), 1000);
    assert_eq!(armed["watch"]["expires_ms"], 301_000);
    let id = armed["watch"]["id"].clone();
    input["action"] = json!({"operation":"list"});
    let listed = apply(&path, input.clone(), 200_000);
    assert_eq!(listed["watches"][0]["id"], id);
    assert_eq!(listed["watches"][0]["expires_ms"], 301_000);
    assert_eq!(listed["calls_this_hour"], 0);
    assert_eq!(
        apply(&path, input.clone(), 301_000)["watches"][0]["status"],
        "expired"
    );
    input["task"]["deadline_ms"] = json!({"kind":"finite","value":9_000_000});
    assert_eq!(
        apply(&path, input, 302_000)["watches"][0]["status"],
        "expired"
    );
}

#[test]
fn dropping_a_task_deadline_does_not_extend_an_existing_watch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watches.json");
    let mut input = watch_input();
    input["task"]["deadline_ms"] = json!({"kind":"finite","value":10_000});
    assert_eq!(
        apply(&path, input.clone(), 1000)["watch"]["expires_ms"],
        10_000
    );
    input["task"]["deadline_ms"] = json!({"kind":"no_configured_bound"});
    input["action"] = json!({"operation":"list"});
    assert_eq!(
        apply(&path, input.clone(), 5000)["watches"][0]["expires_ms"],
        10_000
    );
    assert_eq!(
        apply(&path, input, 10_000)["watches"][0]["status"],
        "expired"
    );
}

#[test]
fn exhausted_or_stopped_task_cancels_watch_without_rewriting_authority() {
    for (field, restriction) in [
        ("remaining_turns", json!({"kind":"finite","value":0})),
        ("deadline_ms", json!({"kind":"finite","value":1000})),
        ("status", json!("stopped")),
        ("status", json!("budget_exhausted")),
        ("id", json!("another-task")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watches.json");
        let base = watch_input();
        apply(&path, base.clone(), 1000);
        let mut restricted = base.clone();
        restricted["task"][field] = restriction;
        restricted["action"] = json!({"operation":"list"});
        assert_eq!(
            apply(&path, restricted, 2000)["watches"][0]["status"],
            "cancelled"
        );
        let mut restored = base;
        restored["action"] = json!({"operation":"list"});
        assert_eq!(
            apply(&path, restored, 3000)["watches"][0]["status"],
            "cancelled"
        );
    }
}

#[test]
fn legacy_numeric_watch_and_continuation_still_use_original_contract() {
    let mut input = continuation_input();
    input["remaining_ms"] = json!(200_000);
    input["remaining_turns"] = json!(4);
    let result = continuation::assess(serde_json::from_value(input).unwrap()).unwrap();
    assert_eq!(result["policy"], "bounded-continuation/1");
    assert_eq!(result["action"], "continue");
    let mut input = watch_input();
    input["task"]["deadline_ms"] = json!(50_000);
    input["task"]["remaining_turns"] = json!(4);
    let legacy: watches::Input = serde_json::from_value(input).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let result = watches::apply(&dir.path().join("watches.json"), legacy, 1000).unwrap();
    assert_eq!(result["watch"]["expires_ms"], 50_000);
}
