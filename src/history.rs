//! Read-only public conversation projection for authorized context providers.
//! This is a local management contract, not an ACP extension or a second store.
//! The calling service selects authorized IDs; this process has OS-owner access.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

pub const PROFILE: &str = "agent-runtime.history/1";
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Describe,
    List {
        store_id: String,
        session_ids: Vec<String>,
        #[serde(default = "limit")]
        limit: usize,
        #[serde(default)]
        cursor: Option<String>,
    },
    Read {
        store_id: String,
        session_id: String,
        #[serde(default = "limit")]
        limit: usize,
        #[serde(default)]
        before: Option<usize>,
    },
}
fn limit() -> usize {
    20
}
fn check(store: &Store, expected: &str, limit: usize) -> Result<()> {
    ensure!(
        store.identity()? == expected,
        "History store changed; reconnect to establish its identity"
    );
    ensure!((1..=40).contains(&limit), "History limit must be 1 to 40");
    Ok(())
}
fn public_block(part: &Value) -> Option<Value> {
    if part["_meta"]["io.github.bkrabach.agent-instance"]["context"] == true {
        return None;
    }
    let kind = part["type"].as_str()?;
    if matches!(kind, "text" | "input_text" | "output_text") {
        let text = part["text"].as_str()?;
        if text.is_empty() {
            return None;
        }
        let clipped: String = text.chars().take(12_000).collect();
        return Some(json!({"type":"text","text":clipped,"truncated":clipped.len()<text.len()}));
    }
    match kind {
        "resource_link" => Some(json!({"type":kind,"uri":part["uri"],"name":part["name"]})),
        "resource" => Some(
            json!({"type":"resource_link","uri":part["resource"]["uri"],"name":"Embedded resource","content_status":"See the original session for resource contents"}),
        ),
        "image" | "audio" => Some(
            json!({"type":kind,"mimeType":part["mimeType"],"content_status":"See the original session for media"}),
        ),
        _ => None,
    }
}
fn message(item: &Value) -> Result<Vec<Value>> {
    if item["role"] == "user"
        && let Some(parts) = item["_acp_prompt"].as_array()
    {
        return Ok(parts.iter().filter_map(public_block).collect());
    }
    Ok(crate::media::updates(item, true)?
        .iter()
        .filter(|u| {
            matches!(
                u["sessionUpdate"].as_str(),
                Some("user_message_chunk" | "agent_message_chunk")
            )
        })
        .filter_map(|u| {
            let mut part = u["content"].clone();
            // Only legacy user items lack original ACP parts. New user text,
            // including a literal copy of this envelope, must stay intact.
            if item["role"] == "user"
                && let Some(rest) = part["text"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("[Configured agent instance context]"))
            {
                part["text"] = json!(
                    rest.split_once("[/Configured agent instance context]")
                        .map(|(_, s)| s.trim_start())
                        .unwrap_or("")
                );
            }
            public_block(&part)
        })
        .collect())
}
pub fn query(store: &Store, q: Query) -> Result<Value> {
    match q {
        Query::Describe => Ok(
            json!({"profile":PROFILE,"store_id":store.identity()?,"operations":["list","read"],"projection":"public messages; no reasoning, tool traces, injected profiles or credentials"}),
        ),
        Query::List {
            store_id,
            session_ids,
            limit,
            cursor,
        } => {
            check(store, &store_id, limit)?;
            ensure!(session_ids.len() <= 1000, "Too many authorized history IDs");
            for sid in &session_ids {
                uuid::Uuid::parse_str(sid)?;
            }
            let mut result = store.history_list(&session_ids, limit, cursor.as_deref())?;
            for row in result["items"].as_array_mut().unwrap() {
                if row["label"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("[Configured agent instance context]"))
                {
                    row["label"] = json!("Saved conversation");
                }
                row["surface"] = json!("native");
                row["url"] = json!(format!(
                    "agent-history://{store_id}/{}",
                    row["conversation_id"].as_str().unwrap()
                ));
            }
            result["store_id"] = json!(store_id);
            result["profile"] = json!(PROFILE);
            Ok(result)
        }
        Query::Read {
            store_id,
            session_id,
            limit,
            before,
        } => {
            check(store, &store_id, limit)?;
            let session = store.load(&session_id)?;
            let end = before
                .unwrap_or(session.items.len())
                .min(session.items.len());
            let mut messages = vec![];
            let mut bytes = 0;
            let mut next = None;
            for (i, item) in session.items[..end].iter().enumerate().rev() {
                let content = message(item)?;
                if content.is_empty() {
                    continue;
                }
                let msg = json!({"id":format!("{session_id}:{i}"),"actor":if item["role"]=="user" {"user"}else{"assistant"},"content":content,"text":content.iter().filter_map(|p|p["text"].as_str()).collect::<Vec<_>>().join("\n\n"),"url":format!("agent-history://{store_id}/{session_id}#message-{i}")});
                let size = serde_json::to_vec(&msg)?.len();
                if messages.len() >= limit || bytes + size > 200 * 1024 {
                    ensure!(
                        !messages.is_empty(),
                        "This message exceeds the history excerpt budget; load its original ACP session"
                    );
                    next = Some(i + 1);
                    break;
                }
                bytes += size;
                messages.push(msg);
            }
            messages.reverse();
            Ok(
                json!({"profile":PROFILE,"store_id":store_id,"conversation_id":session_id,"messages":messages,"next_before":next,"complete":next.is_none(),"observed_at":crate::store::now(),"coverage":"Public messages only; resource bodies, tool traces and injected context are omitted. Text excerpts carry a truncated flag."}),
            )
        }
    }
}

pub fn from_file(store: &Store, path: &std::path::Path) -> Result<Value> {
    ensure!(
        std::fs::metadata(path)?.len() <= 128 * 1024,
        "History query exceeds limit"
    );
    query(
        store,
        serde_json::from_slice(&std::fs::read(path)?).context("Invalid history query")?,
    )
}
