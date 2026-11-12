//! Content-free summaries of recorded execution, not estimated cost or savings.
use crate::store::Store;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn inspect(store: &Store) -> Result<Value> {
    let db = store.db()?;
    let total: i64 = db.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
    let mut stmt =
        db.prepare("SELECT seq,kind,at,data FROM events ORDER BY seq DESC LIMIT 20000")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts = BTreeMap::<String, u64>::new();
    let mut reasons = BTreeMap::<String, u64>::new();
    let mut latency = vec![];
    let mut latest_context = Value::Null;
    let mut latest_stop = Value::Null;
    let mut latest_rejection = Value::Null;
    let mut latest_capacity = Value::Null;
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cached = 0u64;
    let mut measured = 0u64;
    let mut cached_samples = 0u64;
    let mut output_samples = 0u64;
    for (_, kind, at, raw) in &rows {
        if !matches!(
            kind.as_str(),
            "model.started"
                | "model.finished"
                | "model.failed"
                | "provider.response.created"
                | "context.compaction.started"
                | "context.compaction.failed"
                | "context.compacted"
                | "context.limit"
                | "context.rejected"
                | "turn.finished"
                | "turn.rejected"
        ) {
            continue;
        }
        *counts.entry(kind.clone()).or_default() += 1;
        let v: Value = serde_json::from_str(raw)?;
        if matches!(kind.as_str(), "context.limit" | "context.rejected")
            && latest_capacity.is_null()
        {
            latest_capacity = capacity_summary(kind, at, &v);
        }
        if kind == "turn.rejected" && latest_rejection.is_null() {
            let known =
                v["stage"] == "provider_binding" && v["reason"] == "provider_identity_mismatch";
            latest_rejection = json!({"at":at,"stage":if known {"provider_binding"} else {"unknown"},"reason":if known {"provider_identity_mismatch"} else {"unknown"},"input_recorded":v["input_recorded"].as_bool(),"model_started":v["model_started"].as_bool()});
        }
        if kind == "model.started" && latest_context.is_null() {
            latest_context = json!({"at":at});
            for k in [
                "history_items",
                "history_bytes",
                "model_items",
                "model_context_bytes",
                "counted_model_context_bytes",
                "instructions_bytes",
                "tool_definitions_bytes",
                "tool_definitions",
                "context_limit_bytes",
            ] {
                if v[k].is_u64() {
                    latest_context[k] = v[k].clone();
                }
            }
        }
        if kind == "model.finished" {
            if let Some(n) = v["elapsed_ms"].as_u64() {
                latency.push(n);
            }
            if let Some(n) = v["usage"]["input_tokens"].as_u64() {
                input += n;
                measured += 1;
            }
            if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                output += n;
                output_samples += 1;
            }
            if let Some(n) = v["usage"]["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .or_else(|| v["usage"]["provider_usage"]["cache_read_input_tokens"].as_u64())
            {
                cached += n;
                cached_samples += 1;
            }
        }
        if kind == "turn.finished" {
            let reason = match v["reason"].as_str() {
                Some("end_turn") => "completed",
                Some("cancelled") => "cancelled",
                Some("turn_deadline") => "turn_deadline",
                Some("tool_call_budget") => "tool_call_budget",
                Some("model_call_budget" | "max_turn_requests") => "model_call_budget",
                Some("active_turn_context_capacity") => "active_turn_context_capacity",
                Some("max_tokens") => "output_limit",
                Some("refusal") => "refusal",
                _ => "failure_or_unknown",
            };
            *reasons.entry(reason.into()).or_default() += 1;
            if latest_stop.is_null() {
                latest_stop =
                    json!({"at":at,"reason":reason,"elapsed_ms":v["elapsed_ms"].as_u64()});
            }
        }
    }
    latency.sort_unstable();
    let percentile = |n: usize| {
        latency
            .get(latency.len().saturating_sub(1) * n / 100)
            .copied()
    };
    let mut report = json!({"profile":"agent-runtime.observability/1","coverage":{"events_in_store":total,"sampled_events":rows.len(),"first_seq":rows.last().map(|r|r.0),"last_seq":rows.first().map(|r|r.0),"complete_store_window":total==rows.len() as i64},"counts":counts,"stop_reasons":reasons,"model_step_latency_ms":{"samples":latency.len(),"p50":percentile(50),"p95":percentile(95)},"reported_usage":{"measured_responses":measured,"input_tokens":(measured>0).then_some(input),"output_tokens":(output_samples>0).then_some(output),"cache_read_tokens":(cached_samples>0).then_some(cached),"cache_measured_responses":cached_samples,"cost":null},"latest_context":latest_context,"latest_stop":latest_stop,"notes":["Model steps are engine requests; provider-created responses are counted separately when observed. Usage updates are not request counts.","Usage is provider-reported. Compaction usage is separate in context.compacted events. Cache tokens are not a cost-savings percentage.","No source text, tool arguments, private reasoning or compaction state is included."]});
    report["latest_rejection"] = latest_rejection;
    report["latest_capacity"] = latest_capacity;
    Ok(report)
}

fn capacity_summary(kind: &str, at: &str, data: &Value) -> Value {
    // Diagnostic labels come from closed vocabularies. Event data can contain
    // private payloads or future fields; never copy their arbitrary strings.
    let reason = match data["reason"].as_str() {
        Some("active_turn_context_capacity") => "active_turn_context_capacity",
        _ => "unknown",
    };
    let boundary = match data["capacity_boundary"].as_str() {
        Some("active_continuation") => "active_continuation",
        Some("pending_inputs") => "pending_inputs",
        _ => "unknown",
    };
    let blocker = match data.get("yield_blocker") {
        Some(Value::Null) => Value::Null,
        Some(Value::String(value))
            if matches!(
                value.as_str(),
                "compaction_disabled"
                    | "initial_request"
                    | "no_tool_iteration"
                    | "tool_outcome_uncertain"
                    | "provider_continuation"
                    | "provider_boundary_unsettled"
                    | "provider_boundary_error"
                    | "pending_input_unsettled"
                    | "cancelled"
            ) =>
        {
            json!(value)
        }
        _ => json!("unknown"),
    };
    let mut input_states = serde_json::Map::new();
    for state in [
        "received",
        "submitted",
        "remoteQueued",
        "incorporated",
        "notAccepted",
        "unknown",
    ] {
        if let Some(count) = data["input_states"][state].as_u64() {
            input_states.insert(state.into(), json!(count));
        }
    }
    let mut summary = json!({
        "at":at,
        "event":kind,
        "outcome":if kind == "context.limit" {"boundary_closed"} else {"rejected"},
        "reason":reason,
        "capacity_boundary":boundary,
        "yield_blocker":blocker,
        "full_history_preserved":data["full_history_preserved"].as_bool(),
        "input_states":input_states,
    });
    for measurement in [
        "model_calls",
        "tool_calls",
        "required_context_bytes",
        "available_context_bytes",
    ] {
        if let Some(count) = data[measurement].as_u64() {
            summary[measurement] = json!(count);
        }
    }
    summary
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_do_not_expose_payloads_or_invent_missing_usage() {
        let temp = tempfile::tempdir().unwrap();
        let s = Store::open(temp.path().into()).unwrap();
        s.event(
            "private-session",
            Some("private-turn"),
            "model.started",
            json!({"model_context_bytes":123,"instructions":"SECRET"}),
        )
        .unwrap();
        s.event("private-session",Some("private-turn"),"model.finished",json!({"elapsed_ms":800,"usage":{"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":80}}})).unwrap();
        s.event(
            "private-session",
            None,
            "turn.finished",
            json!({"reason":"SECRET_ERROR"}),
        )
        .unwrap();
        s.event("private-session", None, "turn.rejected", json!({"stage":"SECRET_STAGE","reason":"SECRET_REASON","input_recorded":false,"model_started":false})).unwrap();
        let report = inspect(&s).unwrap();
        assert!(!report.to_string().contains("SECRET"));
        assert!(!report.to_string().contains("private-session"));
        assert_eq!(report["reported_usage"]["cache_read_tokens"], 80);
        assert_eq!(report["model_step_latency_ms"]["p95"], 800);
        assert!(report["reported_usage"]["cost"].is_null());
        assert_eq!(report["stop_reasons"]["failure_or_unknown"], 1);
        assert_eq!(report["counts"]["turn.rejected"], 1);
        assert_eq!(report["latest_rejection"]["reason"], "unknown");
        assert_eq!(report["latest_rejection"]["model_started"], false);
        assert!(report["latest_capacity"].is_null());
    }

    #[test]
    fn latest_capacity_preserves_known_measurements_and_distinguishes_closed_boundary_from_refusal()
    {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().into()).unwrap();
        let measurements = json!({
            "reason":"active_turn_context_capacity", "capacity_boundary":"active_continuation",
            "model_calls":3, "tool_calls":7, "required_context_bytes":197086,
            "available_context_bytes":196913, "full_history_preserved":true,
            "yield_blocker":null,
            "input_states":{"received":0,"submitted":1,"remoteQueued":2,"incorporated":3,"notAccepted":4,"unknown":5},
        });
        store
            .event(
                "private-session",
                Some("private-turn"),
                "context.limit",
                measurements.clone(),
            )
            .unwrap();
        let report = inspect(&store).unwrap();
        let mut expected = measurements.clone();
        expected["at"] = report["latest_capacity"]["at"].clone();
        expected["event"] = json!("context.limit");
        expected["outcome"] = json!("boundary_closed");
        assert!(expected["at"].is_string());
        assert_eq!(report["latest_capacity"], expected);
        assert_eq!(report["counts"]["context.limit"], 1);
        assert!(report["counts"].get("context.rejected").is_none());

        let mut rejected = measurements;
        rejected["capacity_boundary"] = json!("pending_inputs");
        rejected["yield_blocker"] = json!("pending_input_unsettled");
        store
            .event(
                "private-session",
                Some("private-turn"),
                "context.rejected",
                rejected.clone(),
            )
            .unwrap();
        let report = inspect(&store).unwrap();
        rejected["at"] = report["latest_capacity"]["at"].clone();
        rejected["event"] = json!("context.rejected");
        rejected["outcome"] = json!("rejected");
        assert_eq!(report["latest_capacity"], rejected);
        assert_eq!(report["counts"]["context.limit"], 1);
        assert_eq!(report["counts"]["context.rejected"], 1);
        assert!(report["latest_rejection"].is_null());
        assert!(!report.to_string().contains("private-session"));
        assert!(!report.to_string().contains("private-turn"));
    }

    #[test]
    fn capacity_projection_drops_private_fields_unknown_labels_and_invalid_counts() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().into()).unwrap();
        store.event("PRIVATE_SESSION", Some("PRIVATE_TURN"), "context.rejected", json!({
            "at":"PRIVATE_TIME", "event":"PRIVATE_EVENT", "outcome":"PRIVATE_OUTCOME",
            "reason":"PRIVATE_REASON", "capacity_boundary":"PRIVATE_BOUNDARY", "yield_blocker":"PRIVATE_BLOCKER",
            "full_history_preserved":"PRIVATE_CLAIM", "model_calls":-1, "tool_calls":true,
            "required_context_bytes":"PRIVATE_BYTES", "available_context_bytes":1.5,
            "session_id":"PRIVATE_SESSION", "prompt":"PRIVATE_CONTENT", "provider_error":"PRIVATE_ERROR",
            "input_states":{"received":"PRIVATE_RECEIPT", "submitted":-1,"remoteQueued":false,"incorporated":2.5,"notAccepted":null,"unknown":9,"PRIVATE_INPUT_ID":88},
        })).unwrap();
        store
            .event(
                "PRIVATE_SESSION",
                None,
                "PRIVATE_EVENT_KIND",
                json!({"reason":"PRIVATE_BODY"}),
            )
            .unwrap();
        let report = inspect(&store).unwrap();
        assert!(!report.to_string().contains("PRIVATE"));
        assert_eq!(report["counts"]["context.rejected"], 1);
        let summary = &report["latest_capacity"];
        assert_eq!(summary["event"], "context.rejected");
        assert_eq!(summary["outcome"], "rejected");
        assert_eq!(summary["reason"], "unknown");
        assert_eq!(summary["capacity_boundary"], "unknown");
        assert_eq!(summary["yield_blocker"], "unknown");
        assert!(summary["full_history_preserved"].is_null());
        assert_eq!(summary["input_states"], json!({"unknown":9}));
        for key in [
            "model_calls",
            "tool_calls",
            "required_context_bytes",
            "available_context_bytes",
        ] {
            assert!(summary.get(key).is_none());
        }
    }

    #[test]
    fn capacity_summary_only_reports_the_negotiated_blocker_vocabulary() {
        for blocker in [
            "compaction_disabled",
            "initial_request",
            "no_tool_iteration",
            "tool_outcome_uncertain",
            "provider_continuation",
            "provider_boundary_unsettled",
            "provider_boundary_error",
            "pending_input_unsettled",
            "cancelled",
        ] {
            let summary = capacity_summary(
                "context.rejected",
                "fixture time",
                &json!({"yield_blocker":blocker}),
            );
            assert_eq!(summary["yield_blocker"], blocker);
        }
        for blocker in [
            json!("future-private-detail"),
            json!(true),
            json!({"reason":"private"}),
        ] {
            let summary = capacity_summary(
                "context.rejected",
                "fixture time",
                &json!({"yield_blocker":blocker}),
            );
            assert_eq!(summary["yield_blocker"], "unknown");
        }
        // Absence in older events is unknown, never a claim that yielding was safe.
        assert_eq!(
            capacity_summary("context.limit", "fixture time", &json!({}))["yield_blocker"],
            "unknown"
        );
    }
}
