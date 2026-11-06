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
                | "turn.finished"
                | "turn.rejected"
        ) {
            continue;
        }
        *counts.entry(kind.clone()).or_default() += 1;
        let v: Value = serde_json::from_str(raw)?;
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
    Ok(report)
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
    }
}
