use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Peer {
    wire: amplifier_acp_connection::Sender,
    pub compaction_supported: Arc<AtomicBool>,
    pub message_completion_supported: Arc<AtomicBool>,
}
impl Peer {
    pub fn new(wire: amplifier_acp_connection::Sender) -> Self {
        Self {
            wire,
            compaction_supported: Arc::new(AtomicBool::new(false)),
            message_completion_supported: Arc::new(AtomicBool::new(false)),
        }
    }
    pub async fn send(&self, value: Value) -> Result<()> {
        self.wire.send(value)
    }
    pub async fn compaction(&self, sid: &str, id: &str, status: &str) -> Result<()> {
        let update = if self.compaction_supported.load(Ordering::SeqCst) {
            json!({"sessionUpdate":"compaction_update","compactionId":id,"status":status})
        } else {
            json!({"sessionUpdate":if status=="in_progress"{"tool_call"}else{"tool_call_update"},"toolCallId":id,"title":"Compacting conversation","kind":"think","status":status})
        };
        self.update(sid, update).await
    }
    pub async fn complete_public_message(&self, sid: &str, update: Option<Value>) -> Result<()> {
        if self.message_completion_supported.load(Ordering::SeqCst)
            && let Some(update) = update
        {
            self.update(sid, update).await?;
        }
        Ok(())
    }
    pub async fn update(&self, sid: &str, update: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":update}})).await
    }
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        // The SDK owns correlation and sends request cancellation when the
        // waiting future is dropped, including cancellation of its parent turn.
        tokio::select! {
            _ = cancel.cancelled() => bail!("Cancelled"),
            result = tokio::time::timeout(Duration::from_secs(180), self.wire.call(method,params)) =>
                result.context("Client request expired")?.context("Client declined or disconnected"),
        }
    }
    pub async fn permission(
        &self,
        sid: &str,
        call: &str,
        title: &str,
        input: &Value,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let result=self.call("session/request_permission",json!({"sessionId":sid,"toolCall":{"toolCallId":call,"title":title,"kind":"other","status":"pending","rawInput":input},"options":[{"optionId":"allow_once","name":"Allow once","kind":"allow_once"},{"optionId":"reject_once","name":"Decline","kind":"reject_once"}]}),cancel).await?;
        permission_result(&result)
    }
}

fn permission_result(result: &Value) -> Result<()> {
    if result["outcome"]["outcome"] == "selected" && result["outcome"]["optionId"] == "allow_once" {
        return Ok(());
    }
    // Only recognize fixed reason codes. Never repeat an arbitrary client's text
    // as authority, and never let diagnostic metadata authorize an action.
    let reason = match result["_meta"]["com.microsoft.amplifier.permission"]["reason"].as_str() {
        Some("source_changed") => {
            "The conversation or initiating source changed; this turn's action was cancelled. Re-evaluate unfinished work in the next current turn; this is not an external service permission failure"
        }
        Some("policy_changed") => "The client's approval policy changed; the action was cancelled",
        Some("expired") => "The approval request expired before a decision",
        _ if result["outcome"]["outcome"] == "selected" => "The client declined this action",
        _ if result["outcome"]["outcome"] == "cancelled" => {
            "The client cancelled this permission request"
        }
        _ => "The client did not provide a recognized permission decision",
    };
    bail!("{reason}; no action performed")
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    #[test]
    fn diagnostics_explain_denials_but_never_grant_or_echo_untrusted_text() {
        assert!(
            permission_result(&json!({"outcome":{"outcome":"selected","optionId":"allow_once"}}))
                .is_ok()
        );
        for (outcome, code, expected) in [
            ("cancelled", "source_changed", "not an external service"),
            ("cancelled", "expired", "expired"),
            ("cancelled", "policy_changed", "policy changed"),
            ("selected", "", "declined"),
            ("cancelled", "secret malicious text", "cancelled"),
        ] {
            let error=permission_result(&json!({"outcome":{"outcome":outcome,"optionId":"reject_once"},"_meta":{"com.microsoft.amplifier.permission":{"reason":code}}})).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret malicious"));
        }
    }
}
