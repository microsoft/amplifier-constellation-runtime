use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type Waiters = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;
#[derive(Clone)]
pub struct Peer {
    pub output: mpsc::Sender<Value>,
    pub compaction_supported: Arc<AtomicBool>,
    pending: Waiters,
    next: Arc<AtomicU64>,
}
struct Pending {
    id: String,
    request_id: String,
    output: mpsc::Sender<Value>,
    waiters: Waiters,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.waiters.lock().unwrap().remove(&self.id).is_some() {
            // A containing turn may be cancelled before call() itself is
            // polled again. Notify the client when this future is dropped too.
            let _ = self.output.try_send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":self.request_id}}));
        }
    }
}
impl Peer {
    pub fn new(output: mpsc::Sender<Value>) -> Self {
        Self {
            output,
            compaction_supported: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicU64::new(1)),
        }
    }
    pub async fn send(&self, v: Value) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), self.output.send(v))
            .await
            .context("Client output stalled")?
            .context("Client disconnected")
    }
    pub async fn compaction(&self, sid: &str, id: &str, status: &str) -> Result<()> {
        let update = if self.compaction_supported.load(Ordering::SeqCst) {
            json!({"sessionUpdate":"compaction_update","compactionId":id,"status":status})
        } else {
            json!({"sessionUpdate":if status=="in_progress"{"tool_call"}else{"tool_call_update"},"toolCallId":id,"title":"Compacting conversation","kind":"think","status":status})
        };
        self.update(sid, update).await
    }
    pub async fn update(&self, sid: &str, update: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":update}})).await
    }
    pub fn response(&self, v: Value) {
        if let Some(key) = v.get("id").map(Value::to_string)
            && let Some(tx) = self.pending.lock().unwrap().remove(&key)
        {
            let _ = tx.send(v);
        }
    }
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let id = format!("runtime-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let key = json!(id).to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(key.clone(), tx);
        let _guard = Pending {
            id: key,
            request_id: id.clone(),
            output: self.output.clone(),
            waiters: self.pending.clone(),
        };
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let result = tokio::select! { _=cancel.cancelled()=> { bail!("Cancelled") }, result=tokio::time::timeout(Duration::from_secs(180),rx)=>result.context("Client request expired")?.context("Client disconnected")? };
        ensure!(
            result.get("error").is_none(),
            "Client declined or could not perform the operation"
        );
        Ok(result["result"].clone())
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
pub async fn frame<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Value>> {
    use tokio::io::AsyncBufReadExt;
    let mut bytes = Vec::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            ensure!(bytes.is_empty(), "Incomplete ACP frame");
            return Ok(None);
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(buf.len());
        ensure!(bytes.len() + n <= 24 * 1024 * 1024, "ACP frame too large");
        bytes.extend_from_slice(&buf[..n]);
        r.consume(n);
        if bytes.last() == Some(&b'\n') {
            break;
        }
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
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
