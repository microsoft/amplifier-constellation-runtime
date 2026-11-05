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

#[derive(Debug)]
pub struct RecoveryRequired;
impl std::fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Previous input outcome requires review. Inspect _session/input_receipts and acknowledge proceeding without replay via _session/acknowledge_inputs, or start a new session. No work was replayed.")
    }
}
impl std::error::Error for RecoveryRequired {}
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
