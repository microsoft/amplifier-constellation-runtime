use crate::config::{Mode, private_dir};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
#[derive(Clone)]
pub struct Store {
    pub home: PathBuf,
}
#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub cwd: PathBuf,
    pub mode: Mode,
    pub items: Vec<Value>,
    pub title: String,
}
impl Store {
    pub fn open(home: PathBuf) -> Result<Self> {
        private_dir(&home)?;
        private_dir(&home.join("locks"))?;
        let s = Self { home };
        let db = s.db()?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,cwd TEXT NOT NULL,mode TEXT NOT NULL,title TEXT NOT NULL,updated TEXT NOT NULL,items TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT NOT NULL,turn TEXT,kind TEXT NOT NULL,at TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS turns(id TEXT PRIMARY KEY,session TEXT NOT NULL,state TEXT NOT NULL,started TEXT NOT NULL,finished TEXT);")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS history_imports(session TEXT PRIMARY KEY,source TEXT NOT NULL,digest TEXT NOT NULL)")?;
        // Keep the original sessions table compatible with 0.1 for rollback.
        db.execute_batch("CREATE TABLE IF NOT EXISTS session_providers(session TEXT PRIMARY KEY,provider_key TEXT NOT NULL)")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS context_checkpoints(session TEXT NOT NULL,prefix_sha256 TEXT NOT NULL,provider_key TEXT NOT NULL,created TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(session,prefix_sha256))")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS history_identity(id INTEGER PRIMARY KEY CHECK(id=1),value TEXT NOT NULL)")?;
        db.execute(
            "INSERT OR IGNORE INTO history_identity(id,value) VALUES(1,?1)",
            [id()],
        )?;
        Ok(s)
    }
    fn db(&self) -> Result<Connection> {
        let db = Connection::open(self.home.join("history.sqlite3"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(db)
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
        let db = self.db()?;
        let (old, items): (Option<String>, String) = db.query_row(
            "SELECT p.provider_key,s.items FROM sessions s LEFT JOIN session_providers p ON p.session=s.id WHERE s.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if let Some(old) = old {
            ensure!(
                old == key,
                "This session belongs to a different provider, endpoint, or model. Start a new session; saved history is preserved."
            );
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
            ensure!(
                actual == key,
                "Session provider changed; start a new session"
            );
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
    pub fn begin(&self, sid: &str) -> Result<String> {
        // Caller holds the cross-process session lock. Earlier running turns are
        // interrupted; no tool intent is silently replayed after a crash.
        self.db()?.execute(
            "UPDATE turns SET state='interrupted',finished=?1 WHERE session=?2 AND state='running'",
            params![now(), sid],
        )?;
        let id = id();
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
        Ok(self.db()?.execute(
            "INSERT INTO context_checkpoints VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session,prefix_sha256) DO NOTHING",
            params![sid, data["prefix_sha256"].as_str().context("Missing checkpoint provenance")?, provider, now(), data.to_string()],
        )? == 1)
    }
}
