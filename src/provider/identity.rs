//! ACP message identity is independent of a prompt turn. A steering successor
//! may revise an answer in the same turn; its chunks must not join the old answer.
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) struct Messages {
    fallback: String,
    phases: HashMap<String, String>,
}
impl Default for Messages {
    fn default() -> Self {
        Self {
            fallback: uuid::Uuid::new_v4().to_string(),
            phases: HashMap::new(),
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
    pub fn chunk(&self, event: &Value, text: &str) -> Value {
        let id = self.id(event);
        let mut v = json!({"sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}});
        if let Some(phase) = self.phases.get(&id) {
            v["_meta"]["com.microsoft.amplifier.message"]["phase"] = phase.clone().into();
        }
        v
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
