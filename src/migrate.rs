//! Explicit owner migration of public ACP conversation content, never execution state.
use crate::{
    config::Config,
    store::{Session, Store},
};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    schema: String,
    source: String,
    sessions: Vec<ImportedSession>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedSession {
    session_id: String,
    cwd: PathBuf,
    title: String,
    messages: Vec<Message>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    role: String,
    content: Vec<Value>,
}

pub fn from_file(store: &Store, config: &Config, file: &Path) -> Result<Value> {
    ensure!(
        std::fs::metadata(file)?.len() <= 64 * 1024 * 1024,
        "Import exceeds 64 MiB"
    );
    let bytes = std::fs::read(file)?;
    let input: Import = serde_json::from_slice(&bytes)?;
    ensure!(
        input.schema == "constellation.public-history/1",
        "Unsupported public history schema"
    );
    ensure!(
        !input.source.is_empty() && input.source.len() <= 200,
        "Invalid source label"
    );
    ensure!(input.sessions.len() <= 1000, "Too many sessions");
    let root = std::fs::canonicalize(&config.workspace)?;
    let mut prepared = Vec::new();
    for s in input.sessions {
        uuid::Uuid::parse_str(&s.session_id)?;
        let cwd = std::fs::canonicalize(&s.cwd)?;
        ensure!(
            cwd.starts_with(&root),
            "Imported working directory must be in this agent's workspace"
        );
        ensure!(
            s.title.len() <= 1024 && s.messages.len() <= 10000,
            "Imported session exceeds limit"
        );
        let mut items = Vec::new();
        for m in s.messages {
            ensure!(
                m.role == "user" || m.role == "assistant",
                "Only public user/assistant messages may be imported"
            );
            ensure!(
                !m.content.is_empty() && m.content.len() <= 32,
                "Invalid imported content count"
            );
            // Import text only. The exporting adapter must explicitly describe
            // omitted media; it may not smuggle tool calls or privileged roles.
            let mut parts = Vec::new();
            for p in m.content {
                ensure!(
                    p["type"] == "text" && p.as_object().is_some_and(|o| o.len() == 2),
                    "Only plain ACP text is accepted in this import format"
                );
                let text = p["text"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Text missing"))?;
                ensure!(text.len() <= 1024 * 1024, "Imported message too large");
                parts.push(json!({"type":"text","text":text}));
            }
            let content: Vec<Value> = parts.iter().map(|p|json!({"type":if m.role=="user" {"input_text"}else{"output_text"},"text":p["text"]})).collect();
            let mut item = json!({"type":"message","role":m.role,"content":content});
            if m.role == "user" {
                item["_acp_prompt"] = json!(parts);
            }
            items.push(item);
        }
        ensure!(
            serde_json::to_vec(&items)?.len() <= 48 * 1024 * 1024,
            "Session history exceeds storage limit"
        );
        prepared.push(Session {
            id: s.session_id,
            cwd,
            mode: config.mode,
            items,
            title: s.title,
        });
    }
    let fingerprint = format!("{:x}", Sha256::digest(&bytes));
    store.import_public(
        &prepared,
        &config.provider.fingerprint(),
        &input.source,
        &fingerprint,
    )?;
    Ok(
        json!({"schema":"constellation.public-history-import/1","sessions":prepared.len(),"source":input.source,"sha256":fingerprint,"projection":"public text only","execution_state_imported":false,"permissions_imported":false,"idempotent":true}),
    )
}
