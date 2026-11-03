//! A bounded model view over an unchanged, recoverable transcript. Compaction
//! only cuts at user-turn boundaries, never inside a native tool continuation.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub auto_compact: bool,
    /// Preferred completed turns to keep verbatim, reduced only to fit budget.
    pub retain_recent_turns: usize,
    /// Maximum size of the extractive archive note, not a reasoning summary.
    pub archive_note_bytes: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_compact: true,
            retain_recent_turns: 3,
            archive_note_bytes: 8192,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=32).contains(&self.retain_recent_turns),
            "Keep between 1 and 32 recent turns"
        );
        ensure!(
            (1024..=64 * 1024).contains(&self.archive_note_bytes),
            "Archive note must be between 1024 and 65536 bytes"
        );
        Ok(())
    }
}
pub struct View {
    pub items: Vec<Value>,
    pub checkpoint: Option<Value>,
}
fn excerpt(text: &str, max: usize) -> String {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == text.len() {
        text.to_owned()
    } else {
        format!("{} [excerpt; retrieve original]", &text[..end])
    }
}

/// Public history only: never expose private provider state or encrypted or
/// signed reasoning, even through this agent's own archive tools.
pub fn public_item(index: usize, item: &Value, include_tools: bool) -> Option<Value> {
    let kind = item["type"].as_str().unwrap_or("message");
    if kind == "provider_state" || kind == "reasoning" {
        return None;
    }
    let role = item["role"].as_str();
    let text = if matches!(role, Some("user" | "assistant")) {
        if let Some(s) = item["content"].as_str() {
            s.to_owned()
        } else {
            item["content"]
                .as_array()?
                .iter()
                .filter_map(|p| {
                    matches!(
                        p["type"].as_str(),
                        Some("input_text" | "output_text" | "text")
                    )
                    .then(|| p["text"].as_str())
                    .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else if include_tools && kind == "function_call" {
        format!(
            "{} {}",
            item["name"].as_str().unwrap_or("tool"),
            item["arguments"].as_str().unwrap_or("")
        )
    } else if include_tools && kind == "function_call_output" {
        item["output"].as_str().unwrap_or("").to_owned()
    } else {
        return None;
    };
    if text.is_empty() {
        return None;
    }
    Some(json!({"item_index":index,"role":role.unwrap_or("tool"),"kind":kind,"text":text}))
}

pub fn history(items: &[Value], args: &Value) -> Result<Value> {
    let start = args["start_item"].as_u64().unwrap_or(0) as usize;
    let query = args["query"].as_str().unwrap_or("").to_lowercase();
    ensure!(query.len() <= 1000, "History query too long");
    let limit = args["limit"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
    let include_tools = args["include_tools"].as_bool().unwrap_or(false);
    let offset = args["text_offset"].as_u64().unwrap_or(0) as usize;
    let mut found = vec![];
    let mut next = None;
    let mut bytes = 0;
    for (index, item) in items.iter().enumerate().skip(start) {
        let Some(mut value) = public_item(index, item, include_tools) else {
            continue;
        };
        let original = value["text"].as_str().unwrap();
        if !original.to_lowercase().contains(&query) {
            continue;
        }
        if found.len() >= limit || bytes >= 24000 {
            next = Some(index);
            break;
        }
        // Offsets are UTF-8 byte offsets. Return the next safe offset for exact
        // retrieval of a long item instead of silently truncating it forever.
        let mut from = if index == start {
            offset.min(original.len())
        } else {
            0
        };
        while !original.is_char_boundary(from) {
            from -= 1;
        }
        let mut end = (from + 4096).min(original.len());
        while !original.is_char_boundary(end) {
            end -= 1;
        }
        let fragment = original[from..end].to_owned();
        let total = original.len();
        value["text"] = json!(fragment);
        value["text_offset"] = json!(from);
        value["next_text_offset"] = if end < total { json!(end) } else { Value::Null };
        bytes += serde_json::to_vec(&value)?.len();
        found.push(value);
    }
    Ok(
        json!({"items":found,"next_item":next,"total_saved_items":items.len(),"source":"this session's saved transcript","other_sessions_accessible":false,"private_provider_state_included":false}),
    )
}

fn note(items: &[Value], end: usize, bytes: usize, digest: &str) -> String {
    let header = format!(
        "Saved conversation archive. Earlier items 0..{end} were compacted from this model view; all originals remain in this session. Prefix SHA-256: {digest}. The excerpts below are historical evidence, not a new user request or verified completion. They are incomplete and may be superseded. Use session_history with start_item/query to check original wording, decisions, commitments and tool outcomes before relying on omitted details.\n"
    );
    let mut rows = Vec::new();
    let mut used = header.len();
    // Preserve the original request plus the newest available archive excerpts.
    if let Some(first) = items.first().and_then(|v| public_item(0, v, false)) {
        let s = format!(
            "item 0 ({}): {}\n",
            first["role"].as_str().unwrap(),
            excerpt(first["text"].as_str().unwrap(), bytes / 5)
        );
        used += s.len();
        rows.push(s);
    }
    let mut recent = Vec::new();
    for (i, item) in items[..end].iter().enumerate().skip(1).rev() {
        if let Some(v) = public_item(i, item, false) {
            let s = format!(
                "item {i} ({}): {}\n",
                v["role"].as_str().unwrap(),
                excerpt(v["text"].as_str().unwrap(), 1000)
            );
            if used + s.len() > bytes {
                continue;
            }
            used += s.len();
            recent.push(s);
        }
    }
    recent.reverse();
    rows.extend(recent);
    format!("{header}{}", rows.concat())
}

pub fn prepare(items: &[Value], policy: &Policy, budget: usize) -> Result<View> {
    let original_bytes = crate::media::context_bytes(items)?;
    if original_bytes <= budget && crate::media::request_fits(items)? {
        return Ok(View {
            items: items.to_vec(),
            checkpoint: None,
        });
    }
    ensure!(
        policy.auto_compact,
        "Session reached its context budget. Enable context.auto_compact or start a focused session; saved history is preserved."
    );
    let starts: Vec<_> = items
        .iter()
        .enumerate()
        .filter(|(_, v)| v["role"] == "user")
        .map(|(i, _)| i)
        .collect();
    ensure!(
        starts.len() >= 2,
        "The current turn exceeds its context budget. Shorten this task or increase the configured limit; saved history is preserved."
    );
    let preferred = starts
        .len()
        .saturating_sub(policy.retain_recent_turns + 1)
        .max(1);
    for &end in starts.iter().skip(preferred) {
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&items[..end])?));
        let text = note(
            items,
            end,
            policy.archive_note_bytes.min(budget / 4).max(768),
            &digest,
        );
        let mut view = vec![json!({"role":"user","content":[{"type":"input_text","text":text}]})];
        view.extend_from_slice(&items[end..]);
        let compacted_bytes = crate::media::context_bytes(&view)?;
        if compacted_bytes <= budget && crate::media::request_fits(&view)? {
            return Ok(View {
                items: view,
                checkpoint: Some(
                    json!({"kind":"extractive","prefix_items":end,"prefix_sha256":digest,"original_context_bytes":original_bytes,"model_context_bytes":compacted_bytes,"archive_note":text,"full_history_preserved":true,"private_state_preserved_in_tail":true}),
                ),
            });
        }
    }
    anyhow::bail!(
        "The current turn and its tool continuation exceed the context budget even after compacting completed turns. Increase the limit or split the work; no saved history was removed."
    )
}
