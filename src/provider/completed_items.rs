//! Some Responses endpoints finish with a sparse or empty output snapshot.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
pub(super) struct CompletedItems(BTreeMap<usize, Value>);

impl CompletedItems {
    pub(super) fn observe(&mut self, event: &Value) -> Result<()> {
        let index = event["output_index"]
            .as_u64()
            .context("Completed item has no output index")?;
        let index = usize::try_from(index)?;
        let item = &event["item"];
        ensure!(
            index < 4096 && item.is_object(),
            "Invalid completed output item"
        );
        if let Some(old) = self.0.get(&index) {
            ensure!(old == item, "Conflicting completed output item");
        } else {
            self.0.insert(index, item.clone());
        }
        Ok(())
    }

    pub(super) fn finish(&mut self, response: &mut Value) -> Result<()> {
        let mut done = std::mem::take(&mut self.0);
        if done.is_empty() {
            return Ok(());
        }
        let terminal = response["output"]
            .as_array_mut()
            .context("Response output missing")?;
        let mut merged_indices = HashSet::new();
        for (position, item) in terminal.drain(..).enumerate() {
            let index = item["id"]
                .as_str()
                .and_then(|id| done.iter().find(|(_, v)| v["id"] == id).map(|(n, _)| *n))
                .unwrap_or(position);
            ensure!(
                merged_indices.insert(index),
                "Duplicate terminal output index"
            );
            if let Some(existing) = done.get(&index) {
                ensure!(existing == &item, "Conflicting terminal output item");
            } else {
                done.insert(index, item);
            }
        }
        let mut identities = HashSet::new();
        for (index, item) in done {
            if let Some(id) = item["id"].as_str() {
                ensure!(
                    identities.insert(id.to_owned()),
                    "Duplicate completed output identity"
                );
            }
            ensure!(
                index == terminal.len(),
                "Incomplete response output indices"
            );
            terminal.push(item);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn sparse_terminal_preserves_order_and_refuses_conflict_or_gaps() {
        let event = |n, id| json!({"output_index":n,"item":{"id":id,"type":"function_call"}});
        for count in [0, 1, 2] {
            let mut c = CompletedItems::default();
            let a = event(0, "a");
            let b = event(1, "b");
            for e in [&b, &a, &a] {
                c.observe(e).unwrap();
            }
            let mut r = json!({"output":match count {0=>vec![],1=>vec![&b["item"]],_=>vec![&a["item"],&b["item"]]}});
            c.finish(&mut r).unwrap();
            assert_eq!(r["output"], json!([a["item"], b["item"]]));
        }
        let mut c = CompletedItems::default();
        c.observe(&event(1, "b")).unwrap();
        assert!(c.observe(&event(1, "c")).is_err());
        assert!(c.finish(&mut json!({"output":[]})).is_err());
        c.observe(&event(0, "a")).unwrap();
        assert!(
            c.finish(&mut json!({"output":[{"id":"a","type":"message"}]}))
                .is_err()
        );
    }
}
