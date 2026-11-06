//! ACP message identity is independent of a prompt turn. A steering successor
//! may revise an answer in the same turn; its chunks must not join the old answer.
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(super) struct Messages {
    fallback: String,
    phases: HashMap<String, String>,
    texts: HashMap<String, String>,
    sealed: HashSet<String>,
    bytes: usize,
    overflow: bool,
}
impl Default for Messages {
    fn default() -> Self {
        Self {
            fallback: uuid::Uuid::new_v4().to_string(),
            phases: HashMap::new(),
            texts: HashMap::new(),
            sealed: HashSet::new(),
            bytes: 0,
            overflow: false,
        }
    }
}
impl Messages {
    fn id(&self, event: &Value) -> String {
        event["item_id"]
            .as_str()
            .or_else(|| event["item"]["id"].as_str())
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .map(str::to_owned)
            .unwrap_or_else(|| self.fallback.clone())
    }
    pub fn observe(&mut self, event: &Value) {
        if let Some(phase @ ("commentary" | "final_answer")) = event["item"]["phase"].as_str()
            && self.phases.len() < 1024
        {
            self.phases.insert(self.id(event), phase.into());
        }
    }
    pub fn chunk(&mut self, event: &Value, text: &str) -> Value {
        let id = self.id(event);
        if !self.overflow {
            if self.bytes + text.len() > amplifier_acp_connection::speech::MAX_BYTES
                || (self.texts.len() >= 256 && !self.texts.contains_key(&id))
            {
                self.overflow = true;
                self.texts.clear();
            } else {
                self.texts.entry(id.clone()).or_default().push_str(text);
                self.bytes += text.len();
            }
        }
        let mut v = json!({"sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}});
        if let Some(phase) = self.phases.get(&id) {
            v["_meta"]["com.microsoft.amplifier.message"]["phase"] = phase.clone().into();
        }
        v
    }
    /// A provider item may be done while the overall work continues. Only a
    /// completed assistant text item whose snapshot matches the deltas seals it.
    pub fn completed(&mut self, event: &Value) -> Option<Value> {
        let item = &event["item"];
        if event["type"] != "response.output_item.done"
            || item["type"] != "message"
            || item["role"] != "assistant"
            || item["status"] != "completed"
        {
            return None;
        }
        let id = self.id(event);
        let text = item["content"]
            .as_array()?
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("output_text") => p["text"].as_str(),
                Some("refusal") => p["refusal"].as_str(),
                _ => None,
            })
            .collect::<String>();
        if self.texts.get(&id)? != &text || text.is_empty() || !self.sealed.insert(id.clone()) {
            return None;
        }
        Some(amplifier_acp_connection::speech::completion(
            &id,
            self.phases.get(&id).map(String::as_str),
            &text,
        ))
    }
    pub fn finish(&self, response: &mut Value) {
        if let Some(items) = response["output"].as_array_mut() {
            for item in items.iter_mut().filter(|i| i["role"] == "assistant") {
                item["_acp_message_id"] = item["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&self.fallback)
                    .into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_matching_completed_public_items_seal_commentary() {
        let mut messages = Messages::default();
        let item = json!({"id":"first","type":"message","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"I found the cause."}]});
        messages.observe(&json!({"item":item}));
        let mut speech = amplifier_acp_connection::speech::Commentary::default();
        assert!(
            speech
                .observe(
                    &messages.chunk(&json!({"item_id":"first"}), "I found "),
                    true
                )
                .unwrap()
                .is_none()
        );
        assert!(
            speech
                .observe(
                    &messages.chunk(&json!({"item_id":"first"}), "the cause."),
                    true
                )
                .unwrap()
                .is_none()
        );
        let mut event = json!({"type":"response.output_item.done","item":item});
        event["item"]["status"] = "incomplete".into();
        assert!(messages.completed(&event).is_none());
        event["item"]["status"] = "completed".into();
        event["item"]["content"][0]["text"] = "different snapshot".into();
        assert!(messages.completed(&event).is_none());
        event["item"]["content"][0]["text"] = "I found the cause.".into();
        let done = messages.completed(&event).unwrap();
        assert_eq!(
            speech.observe(&done, true).unwrap().unwrap().text,
            "I found the cause."
        );
        assert!(messages.completed(&event).is_none());
        event["item"]["type"] = "reasoning".into();
        assert!(messages.completed(&event).is_none());
    }

    #[test]
    fn identities_survive_interleaving_and_replay_but_not_successors() {
        let mut messages = Messages::default();
        messages.observe(&json!({"item":{"id":"a","phase":"final_answer"}}));
        let a = messages.chunk(&json!({"item_id":"a"}), "old");
        let b = messages.chunk(&json!({"item_id":"b"}), "new");
        assert_ne!(a["messageId"], b["messageId"]);
        assert_eq!(
            a["messageId"],
            messages.chunk(&json!({"item_id":"a"}), "tail")["messageId"]
        );
        assert_eq!(
            a["_meta"]["com.microsoft.amplifier.message"]["phase"],
            "final_answer"
        );
        let mut response = json!({"output":[{"role":"assistant","id":"a"},{"role":"assistant"}]});
        messages.finish(&mut response);
        assert_eq!(response["output"][0]["_acp_message_id"], a["messageId"]);
        assert_eq!(
            response["output"][1]["_acp_message_id"],
            messages.chunk(&json!({}), "x")["messageId"]
        );
        assert_ne!(
            messages.chunk(&json!({}), "old")["messageId"],
            Messages::default().chunk(&json!({}), "new")["messageId"]
        );
    }
}
