use crate::config::{Mode, private_dir};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
#[derive(Clone)]
pub struct Store {
    pub home: PathBuf,
    connection: Arc<Mutex<Connection>>,
}
#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub cwd: PathBuf,
    pub mode: Mode,
    pub items: Vec<Value>,
    pub title: String,
}

#[derive(Debug)]
pub struct ProviderIdentityMismatch;
impl std::fmt::Display for ProviderIdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("This session belongs to a different provider, endpoint, or model. Start a new session; saved history is preserved.")
    }
}
impl std::error::Error for ProviderIdentityMismatch {}

impl Store {
    /// Open once per home in a process; clone the Store for concurrent callers.
    pub fn open(home: PathBuf) -> Result<Self> {
        private_dir(&home)?;
        private_dir(&home.join("locks"))?;
        let connection = Connection::open(home.join("history.sqlite3"))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let s = Self {
            home,
            connection: Arc::new(Mutex::new(connection)),
        };
        let db = s.db()?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,cwd TEXT NOT NULL,mode TEXT NOT NULL,title TEXT NOT NULL,updated TEXT NOT NULL,items TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT NOT NULL,turn TEXT,kind TEXT NOT NULL,at TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS turns(id TEXT PRIMARY KEY,session TEXT NOT NULL,state TEXT NOT NULL,started TEXT NOT NULL,finished TEXT);")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS history_imports(session TEXT PRIMARY KEY,source TEXT NOT NULL,digest TEXT NOT NULL)")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS turn_inputs(sequence INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT NOT NULL,turn TEXT NOT NULL,input_id TEXT NOT NULL,item TEXT NOT NULL,state TEXT NOT NULL,recorded INTEGER NOT NULL DEFAULT 0,reason TEXT,remote_parent TEXT,remote_id TEXT,successor TEXT,reconciled INTEGER NOT NULL DEFAULT 0,UNIQUE(session,input_id))")?;
        // Keep the original sessions table compatible with 0.1 for rollback.
        db.execute_batch("CREATE TABLE IF NOT EXISTS session_providers(session TEXT PRIMARY KEY,provider_key TEXT NOT NULL)")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS context_checkpoints(session TEXT NOT NULL,prefix_sha256 TEXT NOT NULL,provider_key TEXT NOT NULL,created TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(session,prefix_sha256))")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS history_identity(id INTEGER PRIMARY KEY CHECK(id=1),value TEXT NOT NULL)")?;
        db.execute(
            "INSERT OR IGNORE INTO history_identity(id,value) VALUES(1,?1)",
            [id()],
        )?;
        drop(db);
        Ok(s)
    }
    // All clones share one connection. Concurrent receipt/history work must not
    // race SQLite WAL connection close against another connection open. Keep
    // guards inside synchronous operations, never across model/tool awaits.
    pub(crate) fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("History connection lock poisoned"))
    }
    pub fn identity(&self) -> Result<String> {
        Ok(self
            .db()?
            .query_row("SELECT value FROM history_identity WHERE id=1", [], |r| {
                r.get(0)
            })?)
    }
    /// Caller holds the cross-process session lock. A repeated delete is harmless.
    pub fn delete(&self, sid: &str) -> Result<()> {
        uuid::Uuid::parse_str(sid)?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        for table in [
            "events",
            "turns",
            "session_providers",
            "context_checkpoints",
            "history_imports",
            "turn_inputs",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE session=?1"), [sid])?;
        }
        tx.execute("DELETE FROM sessions WHERE id=?1", [sid])?;
        tx.commit()?;
        Ok(())
    }
    pub fn create(&self, cwd: PathBuf, mode: Mode) -> Result<Session> {
        let s = Session {
            id: id(),
            cwd,
            mode,
            items: vec![],
            title: "New conversation".into(),
        };
        self.db()?.execute(
            "INSERT INTO sessions (id,cwd,mode,title,updated,items) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                s.id,
                s.cwd.to_string_lossy(),
                s.mode.id(),
                s.title,
                now(),
                "[]"
            ],
        )?;
        Ok(s)
    }
    /// A whole import is transactional. Repeating an identical import is a no-op;
    /// an existing unrelated ID or changed import cannot overwrite native work.
    pub fn import_public(
        &self,
        sessions: &[Session],
        key: &str,
        source: &str,
        digest: &str,
    ) -> Result<()> {
        let mut locks = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        for s in sessions {
            ensure!(ids.insert(s.id.clone()), "Duplicate import session ID");
            locks.push(self.lock(&s.id)?);
        }
        let mut db = self.db()?;
        let tx = db.transaction()?;
        for s in sessions {
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                [&s.id],
                |r| r.get(0),
            )?;
            if existing {
                let matches: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM history_imports WHERE session=?1 AND source=?2 AND digest=?3)",params![s.id,source,digest],|r|r.get(0))?;
                ensure!(
                    matches,
                    "Session ID already exists; its history was preserved"
                );
                continue;
            }
            tx.execute(
                "INSERT INTO sessions(id,cwd,mode,title,updated,items) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    s.id,
                    s.cwd.to_string_lossy(),
                    s.mode.id(),
                    s.title,
                    now(),
                    serde_json::to_string(&s.items)?
                ],
            )?;
            tx.execute(
                "INSERT INTO session_providers(session,provider_key) VALUES(?1,?2)",
                params![s.id, key],
            )?;
            tx.execute(
                "INSERT INTO history_imports(session,source,digest) VALUES(?1,?2,?3)",
                params![s.id, source, digest],
            )?;
            tx.execute("INSERT INTO events(session,kind,at,data) VALUES(?1,'history.imported',?2,?3)",params![s.id,now(),json!({"source":source,"sha256":digest,"projection":"public text only","execution_state_imported":false,"permissions_imported":false}).to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn bind_provider(&self, id: &str, key: &str, allow_legacy_openai: bool) -> Result<()> {
        self.bind_provider_keys(id, key, None, allow_legacy_openai)
    }

    pub fn bind_configured_provider(
        &self,
        id: &str,
        provider: &crate::config::Provider,
    ) -> Result<()> {
        self.bind_provider_keys(
            id,
            &provider.fingerprint(),
            Some(&provider.insertion_order_fingerprint()),
            provider.kind == crate::config::ProviderKind::Openai,
        )
    }

    fn bind_provider_keys(
        &self,
        id: &str,
        key: &str,
        insertion_order_key: Option<&str>,
        allow_legacy_openai: bool,
    ) -> Result<()> {
        let db = self.db()?;
        let (old, items): (Option<String>, String) = db.query_row(
            "SELECT p.provider_key,s.items FROM sessions s LEFT JOIN session_providers p ON p.session=s.id WHERE s.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if let Some(old) = old {
            if old != key && Some(old.as_str()) != insertion_order_key {
                return Err(ProviderIdentityMismatch.into());
            }
            // Preserve the existing binding and transcript, including the
            // short-lived SDK-era encoding. This is equivalence, not migration.
        } else {
            ensure!(
                items == "[]" || allow_legacy_openai,
                "This saved session predates provider selection and contains OpenAI history. Start a new session for this provider."
            );
            db.execute(
                "INSERT INTO session_providers(session,provider_key) VALUES (?1,?2) ON CONFLICT(session) DO NOTHING",
                params![id, key],
            )?;
            // A concurrent configuration change may have pinned this legacy
            // session. Never overwrite that binding or silently ignore it.
            let actual: String = db.query_row(
                "SELECT provider_key FROM session_providers WHERE session=?1",
                [id],
                |r| r.get(0),
            )?;
            if actual != key && Some(actual.as_str()) != insertion_order_key {
                return Err(ProviderIdentityMismatch.into());
            }
        }
        Ok(())
    }
    pub fn pin_legacy_provider(&self, key: &str) -> Result<()> {
        // Before editing a pre-0.2 configuration, bind its existing transcripts
        // to the old destination. Empty sessions have no private state to pin.
        self.db()?.execute(
            "INSERT INTO session_providers(session,provider_key) SELECT id,?1 FROM sessions WHERE items!='[]' ON CONFLICT(session) DO NOTHING",
            [key],
        )?;
        Ok(())
    }
    pub fn load(&self, id: &str) -> Result<Session> {
        uuid::Uuid::parse_str(id)?;
        let (cwd, mode, title, items): (String, String, String, String) = self
            .db()?
            .query_row(
                "SELECT cwd,mode,title,items FROM sessions WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .context("Session not found")?;
        Ok(Session {
            id: id.into(),
            cwd: cwd.into(),
            mode: Mode::parse(&mode)?,
            title,
            items: serde_json::from_str(&items)?,
        })
    }
    pub fn save(&self, s: &Session) -> Result<()> {
        ensure!(
            self.db()?.execute(
                "UPDATE sessions SET mode=?1,title=?2,updated=?3,items=?4 WHERE id=?5",
                params![
                    s.mode.id(),
                    s.title,
                    now(),
                    serde_json::to_string(&s.items)?,
                    s.id
                ]
            )? == 1,
            "Session not found"
        );
        Ok(())
    }
    pub fn list(&self, cwd: Option<&Path>, cursor: Option<&str>) -> Result<Value> {
        let db = self.db()?;
        let mut q=db.prepare("SELECT id,cwd,title,updated FROM sessions WHERE (?1 IS NULL OR cwd=?1) AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT 101")?;
        let values=q.query_map(params![cwd.map(|p|p.to_string_lossy().to_string()),cursor],|r|Ok(json!({"sessionId":r.get::<_,String>(0)?,"cwd":r.get::<_,String>(1)?,"title":r.get::<_,String>(2)?,"updatedAt":r.get::<_,String>(3)?})))?.collect::<Result<Vec<_>,_>>()?;
        let next = (values.len() > 100).then(|| values[99]["sessionId"].clone());
        let mut v = json!({"sessions":values.into_iter().take(100).collect::<Vec<_>>()});
        if let Some(n) = next {
            v["nextCursor"] = n;
        };
        Ok(v)
    }
    pub fn lock(&self, id: &str) -> Result<std::fs::File> {
        uuid::Uuid::parse_str(id)?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.home.join("locks").join(id))?;
        fs2::FileExt::try_lock_exclusive(&f)
            .context("This session is already running in another connection")?;
        Ok(f)
    }
    pub fn history_list(
        &self,
        ids: &[String],
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Value> {
        let cursor: Option<(String, String)> = cursor.map(serde_json::from_str).transpose()?;
        let db = self.db()?;
        let mut q=db.prepare("SELECT id,title,updated FROM sessions WHERE id IN (SELECT value FROM json_each(?1)) AND (?2 IS NULL OR (updated,id)<(?2,?3)) ORDER BY updated DESC,id DESC LIMIT ?4")?;
        let mut rows=q.query_map(params![serde_json::to_string(ids)?,cursor.as_ref().map(|c|&c.0),cursor.as_ref().map(|c|&c.1),(limit+1) as i64],|r| Ok(json!({"conversation_id":r.get::<_,String>(0)?,"label":r.get::<_,String>(1)?,"updated_at":r.get::<_,String>(2)?})))?.collect::<Result<Vec<_>,_>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last()
                .map(|r| json!([r["updated_at"], r["conversation_id"]]).to_string())
        } else {
            None
        };
        Ok(json!({"items":rows,"next_cursor":next,"complete":!more}))
    }
    pub fn event(&self, sid: &str, turn: Option<&str>, kind: &str, data: Value) -> Result<()> {
        self.db()?.execute(
            "INSERT INTO events(session,turn,kind,at,data) VALUES(?1,?2,?3,?4,?5)",
            params![sid, turn, kind, now(), data.to_string()],
        )?;
        Ok(())
    }
    /// Caller owns the session lock. Retain uncertain submissions, never replay them.
    pub fn recover_inputs(&self, sid: &str, current: Option<&str>) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        tx.execute("UPDATE turn_inputs SET state='notAccepted',reason='process_interrupted' WHERE session=?1 AND turn!=COALESCE(?2,'') AND state='received'",params![sid,current])?;
        tx.execute("UPDATE turn_inputs SET state='unknown',reason='process_interrupted' WHERE session=?1 AND turn!=COALESCE(?2,'') AND state IN ('submitted','remoteQueued')",params![sid,current])?;
        tx.commit()?;
        Ok(())
    }
    /// Metadata for authorized session owners; source content remains in the durable inbox.
    pub fn input_receipts(&self, sid: &str) -> Result<Value> {
        let db = self.db()?;
        let mut q=db.prepare("SELECT input_id,turn,sequence,state,recorded,reason,remote_parent,remote_id,successor,reconciled,item FROM turn_inputs WHERE session=?1 ORDER BY reconciled,(state='unknown' OR (state='incorporated' AND recorded=0)) DESC,sequence DESC LIMIT 256")?;
        let rows=q.query_map([sid],|r|Ok(json!({"inputId":r.get::<_,String>(0)?,"turnId":r.get::<_,String>(1)?,"sequence":r.get::<_,i64>(2)?,"state":r.get::<_,String>(3)?,"recorded":r.get::<_,bool>(4)?,"reason":r.get::<_,Option<String>>(5)?,"responseId":r.get::<_,Option<String>>(6)?,"steeringId":r.get::<_,Option<String>>(7)?,"successorId":r.get::<_,Option<String>>(8)?,"reconciled":r.get::<_,bool>(9)?,"prompt":serde_json::from_str::<Value>(&r.get::<_,String>(10)?).unwrap_or(Value::Null)["_acp_prompt"]})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!(rows))
    }
    /// Bounded metadata for the next independent request. The uncertain source
    /// text stays quarantined; it is never silently inserted as a fresh prompt.
    pub fn input_uncertainty(&self, sid: &str) -> Result<Value> {
        let db = self.db()?;
        let count:i64=db.query_row("SELECT count(*) FROM turn_inputs WHERE session=?1 AND reconciled=0 AND (state='unknown' OR (state='incorporated' AND recorded=0))",[sid],|r|r.get(0))?;
        let mut q=db.prepare("SELECT input_id,state,recorded FROM turn_inputs WHERE session=?1 AND reconciled=0 AND (state='unknown' OR (state='incorporated' AND recorded=0)) ORDER BY sequence DESC LIMIT 8")?;
        let items=q.query_map([sid],|r|Ok(json!({"inputId":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"recorded":r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            json!({"count":count,"items":items,"complete":count<=8,"disposition":"quarantined_without_replay"}),
        )
    }
    pub fn acknowledge_inputs(&self, sid: &str, ids: &[String]) -> Result<()> {
        ensure!(
            !ids.is_empty() && ids.len() <= 256,
            "Supply between 1 and 256 input IDs"
        );
        let mut db = self.db()?;
        let tx = db.transaction()?;
        for id in ids {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM turn_inputs WHERE session=?1 AND input_id=?2)",
                params![sid, id],
                |r| r.get(0),
            )?;
            ensure!(exists, "Input receipt does not belong to this session");
            tx.execute(
                "UPDATE turn_inputs SET reconciled=1 WHERE session=?1 AND input_id=?2",
                params![sid, id],
            )?;
        }
        tx.commit()?;
        drop(db);
        self.event(
            sid,
            None,
            "input.reconciled",
            json!({"inputIds":ids,"disposition":"proceed_without_replay"}),
        )?;
        Ok(())
    }
    pub fn begin(&self, sid: &str) -> Result<String> {
        self.begin_id(sid, &id())
    }
    pub fn begin_id(&self, sid: &str, turn: &str) -> Result<String> {
        self.recover_inputs(sid, Some(turn))?;
        // Caller holds the cross-process session lock. Earlier running turns are
        // interrupted; no tool intent is silently replayed after a crash.
        self.db()?.execute(
            "UPDATE turns SET state='interrupted',finished=?1 WHERE session=?2 AND state='running'",
            params![now(), sid],
        )?;
        let id = turn.to_owned();
        self.db()?.execute(
            "INSERT INTO turns VALUES (?1,?2,'running',?3,NULL)",
            params![id, sid, now()],
        )?;
        Ok(id)
    }
    pub fn finish(&self, id: &str, state: &str) -> Result<()> {
        self.db()?.execute(
            "UPDATE turns SET state=?1,finished=?2 WHERE id=?3",
            params![state, now(), id],
        )?;
        Ok(())
    }
    pub fn events(&self, sid: &str) -> Result<Value> {
        let db = self.db()?;
        let mut q = db.prepare(
            "SELECT seq,turn,kind,at,data FROM events WHERE session=?1 ORDER BY seq DESC LIMIT 200",
        )?;
        let v=q.query_map([sid],|r| Ok(json!({"sequence":r.get::<_,i64>(0)?,"turn":r.get::<_,Option<String>>(1)?,"kind":r.get::<_,String>(2)?,"at":r.get::<_,String>(3)?,"data":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)})))?.collect::<Result<Vec<_>,_>>()?;
        Ok(json!(v))
    }
    pub fn checkpoint(&self, sid: &str, provider: &str, data: &Value) -> Result<bool> {
        let prefix = data["prefix_sha256"]
            .as_str()
            .context("Missing checkpoint provenance")?;
        // Different settings or providers can compact the same durable prefix.
        // Keep each canonical result independently; the source digest stays in data.
        let key = if data["kind"] == "provider_native" {
            format!(
                "{:x}",
                Sha256::digest(json!([provider, prefix, data["settings_sha256"]]).to_string())
            )
        } else {
            prefix.to_owned()
        };
        Ok(self.db()?.execute(
            "INSERT INTO context_checkpoints VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session,prefix_sha256) DO NOTHING",
            params![sid, key, provider, now(), data.to_string()],
        )? == 1)
    }
    pub fn provider_checkpoints(&self, sid: &str, provider: &str) -> Result<Vec<Value>> {
        let db = self.db()?;
        let mut q=db.prepare("SELECT data FROM context_checkpoints WHERE session=?1 AND provider_key=?2 ORDER BY created DESC LIMIT 32")?;
        let rows = q
            .query_map(params![sid, provider], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
}
