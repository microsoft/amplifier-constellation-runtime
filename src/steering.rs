//! Durable input admission is independent of the session writer. The gate makes
//! admission and finalization mutually exclusive; only the writer edits history.
use crate::{
    rpc::Peer,
    store::{Session, Store},
};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::Mutex;
use tokio::sync::Notify;

pub const META: &str = "io.github.bkrabach.agent-runtime";
pub struct Inbox {
    pub store: Store,
    pub sid: String,
    pub turn: String,
    gate: Mutex<bool>,
    pub changed: Notify,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub id: String,
    pub sequence: i64,
    pub item: Value,
}
impl Inbox {
    pub fn new(store: Store, sid: &str) -> Result<Self> {
        Ok(Self {
            store,
            sid: sid.into(),
            turn: crate::store::id(),
            gate: Mutex::new(true),
            changed: Notify::new(),
        })
    }
    pub fn admit(&self, id: &str, item: Value) -> Result<Value> {
        ensure!(
            !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
            "Invalid inputId"
        );
        let open = self.gate.lock().unwrap();
        let db = self.store.db()?;
        if let Some((old, turn, sequence, state)) = db
            .query_row(
                "SELECT item,turn,sequence,state FROM turn_inputs WHERE session=?1 AND input_id=?2",
                params![self.sid, id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            ensure!(
                old == serde_json::to_string(&item)?,
                "inputId already belongs to different content"
            );
            return Ok(
                json!({"outcome":"injected","_meta":{"steering":{"inputId":id,"turnId":turn,"sequence":sequence,"state":state,"duplicate":true}}}),
            );
        }
        if !*open {
            return Ok(json!({"outcome":"promptRequired","reason":"noRunningTurn"}));
        }
        let (count, bytes): (i64, i64) = db.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(item)),0) FROM turn_inputs WHERE turn=?1",
            [&self.turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let text = serde_json::to_string(&item)?;
        ensure!(
            count < 256 && bytes + text.len() as i64 <= 4 * 1024 * 1024,
            "Turn input capacity reached; input not accepted"
        );
        db.execute("INSERT INTO turn_inputs(session,turn,input_id,item,state) VALUES(?1,?2,?3,?4,'received')",params![self.sid,self.turn,id,text])?;
        let sequence = db.last_insert_rowid();
        drop(open);
        self.changed.notify_one();
        Ok(
            json!({"outcome":"injected","_meta":{"steering":{"inputId":id,"turnId":self.turn,"sequence":sequence,"state":"received"}}}),
        )
    }
    pub fn queued(&self) -> Result<Vec<Input>> {
        let db = self.store.db()?;
        let mut q=db.prepare("SELECT input_id,sequence,item FROM turn_inputs WHERE turn=?1 AND state='received' ORDER BY sequence LIMIT 16")?;
        q.query_map([&self.turn], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|r| {
            let (id, sequence, item) = r?;
            Ok(Input {
                id,
                sequence,
                item: serde_json::from_str(&item)?,
            })
        })
        .collect()
    }
    pub fn remote(
        &self,
        inputs: &[Input],
        parent: &str,
        remote: Option<&str>,
        successor: Option<&str>,
    ) -> Result<()> {
        let mut db = self.store.db()?;
        let tx = db.transaction()?;
        for i in inputs {
            tx.execute("UPDATE turn_inputs SET remote_parent=?1,remote_id=COALESCE(?2,remote_id),successor=COALESCE(?3,successor) WHERE turn=?4 AND input_id=?5",params![parent,remote,successor,self.turn,i.id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub async fn publish_summary(&self, peer: &Peer) -> Result<()> {
        for receipt in self.summary()?.as_array().unwrap() {
            peer.update(&self.sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""},"_meta":{META:{"inputReceipt":receipt}}})).await?;
        }
        Ok(())
    }
    pub fn state(&self, inputs: &[Input], state: &str, reason: Option<&str>) -> Result<()> {
        let mut db = self.store.db()?;
        let tx = db.transaction()?;
        for i in inputs {
            tx.execute(
                "UPDATE turn_inputs SET state=?1,reason=?2 WHERE turn=?3 AND input_id=?4",
                params![state, reason, self.turn, i.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub async fn receipts(&self, inputs: &[Input], state: &str, peer: &Peer) -> Result<()> {
        for i in inputs {
            peer.update(&self.sid,json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""},"_meta":{META:{"inputReceipt":{"inputId":i.id,"turnId":self.turn,"sequence":i.sequence,"state":state}}}})).await?;
        }
        Ok(())
    }
    /// Save history and mark its associated input receipts in the same commit.
    pub fn save_inputs(&self, session: &Session, inputs: &[Input]) -> Result<()> {
        let mut db = self.store.db()?;
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE sessions SET items=?1,title=?2,mode=?3,updated=?4 WHERE id=?5",
            params![
                serde_json::to_string(&session.items)?,
                session.title,
                session.mode.id(),
                crate::store::now(),
                session.id
            ],
        )?;
        for i in inputs {
            tx.execute("UPDATE turn_inputs SET state='incorporated',recorded=1 WHERE turn=?1 AND input_id=?2",params![self.turn,i.id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn try_close(&self) -> Result<bool> {
        let mut open = self.gate.lock().unwrap();
        let pending:i64=self.store.db()?.query_row("SELECT COUNT(*) FROM turn_inputs WHERE turn=?1 AND (state IN ('received','submitted','remoteQueued') OR (state='incorporated' AND recorded=0))",[&self.turn],|r|r.get(0))?;
        if pending > 0 {
            return Ok(false);
        }
        *open = false;
        Ok(true)
    }
    /// End local input ownership at an already claimed provider capacity boundary.
    /// Submitted or unrecorded incorporation remains uncertain and prevents this
    /// close. Locally received inputs retain their original records without replay.
    pub fn try_close_at_capacity(&self, reason: &str) -> Result<bool> {
        let mut open = self.gate.lock().unwrap();
        let mut db = self.store.db()?;
        let tx = db.transaction()?;
        let uncertain: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turn_inputs WHERE turn=?1 AND (state IN ('submitted','remoteQueued') OR (state='incorporated' AND recorded=0)))",
            [&self.turn],
            |row| row.get(0),
        )?;
        if uncertain {
            return Ok(false);
        }
        tx.execute(
            "UPDATE turn_inputs SET state='notAccepted',reason=?1 WHERE turn=?2 AND state='received'",
            params![reason, self.turn],
        )?;
        tx.commit()?;
        *open = false;
        Ok(true)
    }
    pub fn close(&self, reason: &str) -> Result<()> {
        let mut open = self.gate.lock().unwrap();
        *open = false;
        let mut db = self.store.db()?;
        let tx = db.transaction()?;
        tx.execute("UPDATE turn_inputs SET state='notAccepted',reason=?1 WHERE turn=?2 AND state='received'",params![reason,self.turn])?;
        tx.execute("UPDATE turn_inputs SET state='unknown',reason=?1 WHERE turn=?2 AND state IN ('submitted','remoteQueued')",params![reason,self.turn])?;
        tx.commit()?;
        Ok(())
    }
    pub fn summary(&self) -> Result<Value> {
        let db = self.store.db()?;
        let mut q=db.prepare("SELECT input_id,sequence,state,reason FROM turn_inputs WHERE turn=?1 ORDER BY sequence")?;
        let v=q.query_map([&self.turn],|r|Ok(json!({"inputId":r.get::<_,String>(0)?,"turnId":self.turn,"sequence":r.get::<_,i64>(1)?,"state":r.get::<_,String>(2)?,"reason":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn retained_columns(inbox: &Inbox) -> Vec<Vec<rusqlite::types::Value>> {
        let db = inbox.store.db().unwrap();
        let mut query = db
            .prepare("SELECT sequence,session,turn,input_id,item,recorded,remote_parent,remote_id,successor,reconciled FROM turn_inputs WHERE turn=?1 ORDER BY sequence")
            .unwrap();
        query
            .query_map([&inbox.turn], |row| {
                (0..row.as_ref().column_count())
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn capacity_close_preserves_all_inputs_beyond_one_batch_and_original_deduplication() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).unwrap();
        let mut session = store
            .create(directory.path().into(), crate::config::Mode::ReadOnly)
            .unwrap();
        let inbox = Inbox::new(store.clone(), &session.id).unwrap();
        inbox
            .admit(
                "already-recorded",
                json!({"role":"user","content":"prior input"}),
            )
            .unwrap();
        let recorded = inbox.queued().unwrap();
        inbox
            .remote(
                &recorded,
                "original-parent",
                Some("original-steer"),
                Some("original-successor"),
            )
            .unwrap();
        session.items.push(recorded[0].item.clone());
        inbox.save_inputs(&session, &recorded).unwrap();

        let mut originals = Vec::new();
        for index in 0..40 {
            let id = format!("original-{index}");
            let item = json!({"role":"user","content":format!("correction {index}"),
                "_acp_prompt":[{"type":"text","text":format!("source {index}"),
                    "_meta":{"sourceId":id,"audience":"original conversation"}}]});
            let receipt = inbox.admit(&id, item.clone()).unwrap();
            originals.push((id, item, receipt["_meta"]["steering"]["sequence"].clone()));
        }
        assert_eq!(inbox.queued().unwrap().len(), 16);
        let retained = retained_columns(&inbox);
        assert!(
            inbox
                .try_close_at_capacity("active_turn_context_capacity")
                .unwrap()
        );
        assert_eq!(retained_columns(&inbox), retained);
        assert!(inbox.queued().unwrap().is_empty());
        assert_eq!(store.load(&session.id).unwrap().items, session.items);
        let summary = inbox.summary().unwrap();
        assert_eq!(summary[0]["state"], "incorporated");
        assert!(summary[0]["reason"].is_null());
        assert_eq!(summary.as_array().unwrap().len(), 41);
        assert!(summary.as_array().unwrap()[1..].iter().all(|receipt| {
            receipt["state"] == "notAccepted" && receipt["reason"] == "active_turn_context_capacity"
        }));
        assert!(
            inbox
                .try_close_at_capacity("must_not_replace_prior_reason")
                .unwrap()
        );
        assert_eq!(inbox.summary().unwrap(), summary);
        let receipts = store.input_receipts(&session.id).unwrap();
        let original_turn = inbox.turn.clone();
        drop(inbox);
        drop(store);

        let reopened = Store::open(directory.path().into()).unwrap();
        assert_eq!(reopened.input_receipts(&session.id).unwrap(), receipts);
        assert_eq!(reopened.load(&session.id).unwrap().items, session.items);
        let next = Inbox::new(reopened, &session.id).unwrap();
        for (id, item, sequence) in originals {
            let duplicate = next.admit(&id, item).unwrap();
            let receipt = &duplicate["_meta"]["steering"];
            assert_eq!(receipt["duplicate"], true);
            assert_eq!(receipt["inputId"], id);
            assert_eq!(receipt["turnId"], original_turn);
            assert_eq!(receipt["sequence"], sequence);
            assert_eq!(receipt["state"], "notAccepted");
            assert!(
                next.admit(&id, json!({"content":"changed source"}))
                    .is_err()
            );
        }
        assert!(next.queued().unwrap().is_empty());
        assert_eq!(next.store.input_receipts(&session.id).unwrap(), receipts);
    }

    #[test]
    fn capacity_close_refuses_uncertain_inputs_without_partial_disposition() {
        for state in ["submitted", "remoteQueued", "incorporated"] {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(directory.path().into()).unwrap();
            let inbox = Inbox::new(store, "session").unwrap();
            inbox
                .admit("local", json!({"content":"locally received"}))
                .unwrap();
            inbox
                .admit("uncertain", json!({"content":"not safe to replay"}))
                .unwrap();
            let inputs = inbox.queued().unwrap();
            inbox
                .state(&inputs[1..], state, Some("original-reason"))
                .unwrap();
            let summary = inbox.summary().unwrap();
            let retained = retained_columns(&inbox);
            assert!(
                !inbox
                    .try_close_at_capacity("active_turn_context_capacity")
                    .unwrap()
            );
            assert_eq!(inbox.summary().unwrap(), summary, "state: {state}");
            assert_eq!(retained_columns(&inbox), retained, "state: {state}");
            assert_eq!(
                inbox
                    .admit("still-open", json!({"content":"later"}))
                    .unwrap()["outcome"],
                "injected"
            );
        }
    }

    #[test]
    fn capacity_close_storage_failure_rolls_back_and_leaves_admission_open() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).unwrap();
        let inbox = Inbox::new(store, "session").unwrap();
        for id in ["first", "second"] {
            inbox.admit(id, json!({"content":id})).unwrap();
        }
        inbox.store.db().unwrap().execute_batch(
            "CREATE TRIGGER fail_capacity_disposition BEFORE UPDATE ON turn_inputs WHEN OLD.input_id='second' AND NEW.state='notAccepted' BEGIN SELECT RAISE(ABORT,'fixture storage failure'); END;"
        ).unwrap();
        let summary = inbox.summary().unwrap();
        let retained = retained_columns(&inbox);
        assert!(
            inbox
                .try_close_at_capacity("active_turn_context_capacity")
                .is_err()
        );
        assert_eq!(inbox.summary().unwrap(), summary);
        assert_eq!(retained_columns(&inbox), retained);
        assert_eq!(
            inbox
                .admit("after-failure", json!({"content":"retained"}))
                .unwrap()["outcome"],
            "injected"
        );
        inbox
            .store
            .db()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_capacity_disposition")
            .unwrap();
        assert!(
            inbox
                .try_close_at_capacity("active_turn_context_capacity")
                .unwrap()
        );
    }

    #[test]
    fn capacity_close_and_admission_race_never_strands_received_input() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).unwrap();
        for index in 0..32 {
            let inbox = Arc::new(Inbox::new(store.clone(), "session").unwrap());
            let barrier = Arc::new(Barrier::new(3));
            let id = format!("racing-{index}");
            let admit = {
                let inbox = inbox.clone();
                let barrier = barrier.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    inbox.admit(&id, json!({"content":id})).unwrap()
                })
            };
            let close = {
                let inbox = inbox.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    inbox
                        .try_close_at_capacity("active_turn_context_capacity")
                        .unwrap()
                })
            };
            barrier.wait();
            let admitted = admit.join().unwrap();
            assert!(close.join().unwrap());
            let summary = inbox.summary().unwrap();
            if admitted["outcome"] == "injected" {
                assert_eq!(summary.as_array().unwrap().len(), 1);
                assert_eq!(summary[0]["inputId"], id);
                assert_eq!(summary[0]["state"], "notAccepted");
                assert_eq!(summary[0]["reason"], "active_turn_context_capacity");
                assert_eq!(
                    summary[0]["sequence"],
                    admitted["_meta"]["steering"]["sequence"]
                );
            } else {
                assert_eq!(admitted["outcome"], "promptRequired");
                assert!(summary.as_array().unwrap().is_empty());
            }
            assert!(inbox.queued().unwrap().is_empty());
            assert_eq!(
                inbox.admit("late", json!({"content":"later"})).unwrap()["outcome"],
                "promptRequired"
            );
        }
    }

    #[test]
    fn admission_deduplicates_and_closing_does_not_lose_input() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().into()).unwrap();
        let ss = s
            .create(d.path().into(), crate::config::Mode::ReadOnly)
            .unwrap();
        let inbox = Inbox::new(s, &ss.id).unwrap();
        let m = json!({"role":"user","content":"one"});
        inbox.admit("a", m.clone()).unwrap();
        assert!(!inbox.try_close().unwrap());
        assert_eq!(
            inbox.admit("a", m).unwrap()["_meta"]["steering"]["duplicate"],
            true
        );
        assert!(inbox.admit("a", json!({"content":"different"})).is_err());
        let input = inbox.queued().unwrap();
        let mut ss = ss;
        ss.items.push(input[0].item.clone());
        inbox.save_inputs(&ss, &input).unwrap();
        assert!(inbox.try_close().unwrap());
        assert_eq!(
            inbox.admit("b", json!({})).unwrap()["outcome"],
            "promptRequired"
        );
    }
    #[test]
    fn uncertain_submission_is_not_automatically_queued_again() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().into()).unwrap();
        let i = Inbox::new(s, "s").unwrap();
        i.admit("x", json!({"content":"x"})).unwrap();
        i.state(&i.queued().unwrap(), "submitted", None).unwrap();
        i.close("disconnected").unwrap();
        assert!(i.queued().unwrap().is_empty());
        assert_eq!(i.summary().unwrap()[0]["state"], "unknown");
    }
}
