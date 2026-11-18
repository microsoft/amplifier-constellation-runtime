//! Local installation-owner operation. No ACP/tool entry point and no execution,
//! recovery, credential access, configuration replacement, or history migration.
use crate::{config::Config, model_selection};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

const SCHEMA: &str = "constellation.native-tools-reconciliation/1";
const EVENT: &str = "session.native_tools_reconciled";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub operation_id: String,
    pub home: PathBuf,
    pub session_id: String,
    pub previous_config: PathBuf,
    pub previous_config_sha256: String,
    pub current_config_sha256: String,
    pub history_identity: String,
    pub session_snapshot_sha256: String,
    pub saved_choice_sha256: String,
    pub provider_binding_sha256: String,
    pub previous_route: String,
    pub current_route: String,
    pub previous_tools_sha256: String,
    pub current_tools_sha256: String,
    pub previous_tool_count: usize,
    pub current_tool_count: usize,
    pub previous_tools: Vec<Value>,
    pub current_tools: Vec<Value>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Use a canonical UUID"
    );
    Ok(())
}

fn home(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute() && path.is_dir(),
        "Use an existing absolute runtime home"
    );
    Ok(path.canonicalize()?)
}

fn file(path: &Path, write: bool) -> Result<File> {
    ensure!(
        path.is_absolute() && std::fs::symlink_metadata(path)?.is_file(),
        "Expected an existing regular file"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = file(path, false)?;
    ensure!(
        f.metadata()?.len() <= limit,
        "Evidence file exceeds size limit"
    );
    let mut bytes = Vec::new();
    f.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Evidence file exceeds size limit"
    );
    Ok(bytes)
}

/// Existing locks only: even preparing a plan must not initialize an installation.
fn locks(home: &Path, session: &str) -> Result<(File, File)> {
    let config = file(&home.join("configuration.lock"), true)
        .context("An existing configuration.lock is required")?;
    fs2::FileExt::try_lock_exclusive(&config).context("Configuration update is running")?;
    let session = file(&home.join("locks").join(session), true)
        .context("An existing session lock is required")?;
    fs2::FileExt::try_lock_exclusive(&session).context("Session is running or being changed")?;
    Ok((config, session))
}

fn database(home: &Path, write: bool) -> Result<Connection> {
    let path = home.join("history.sqlite3");
    let _existing = file(&path, write)?;
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    // No CREATE, Store::open, WAL mode change, schema migration or recovery.
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.busy_timeout(Duration::ZERO)?;
    Ok(db)
}

fn rows(db: &Connection, sql: &str, session: &str) -> Result<Value> {
    use rusqlite::types::ValueRef;
    let mut query = db.prepare(sql)?;
    let columns: Vec<_> = query
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut result = Vec::new();
    let mut rows = query.query([session])?;
    while let Some(row) = rows.next()? {
        let mut values = Vec::new();
        for i in 0..columns.len() {
            values.push(match row.get_ref(i)? {
                ValueRef::Null => json!(["null"]),
                ValueRef::Integer(v) => json!(["integer", v]),
                ValueRef::Real(v) => json!(["real_bits", v.to_bits()]),
                ValueRef::Text(v) => json!(["text", String::from_utf8(v.to_vec())?]),
                ValueRef::Blob(v) => json!(["blob", v]),
            });
        }
        result.push(serde_json::to_string(&values)?);
    }
    result.sort();
    Ok(json!({"columns":columns,"rows":result}))
}

fn snapshot(db: &Connection, session: &str) -> Result<String> {
    let mut all = Vec::new();
    for (table, key) in [
        ("sessions", "id"),
        ("session_model_choices", "session"),
        ("session_providers", "session"),
        ("turns", "session"),
        ("turn_inputs", "session"),
        ("events", "session"),
        ("context_checkpoints", "session"),
        ("history_imports", "session"),
    ] {
        all.push(json!([
            table,
            rows(
                db,
                &format!("SELECT * FROM {table} WHERE {key}=?1"),
                session
            )?
        ]));
    }
    Ok(digest(&serde_json::to_vec(&all)?))
}

struct Evidence {
    plan: Plan,
    choice_bytes: String,
    next_choice_bytes: String,
}

fn evidence(
    db: &Connection,
    home: &Path,
    session: &str,
    previous: &Path,
    operation: &str,
) -> Result<Evidence> {
    let old_bytes = read(previous, 512 * 1024)?;
    let new_bytes = read(&home.join("runtime.json"), 512 * 1024)?;
    let old: Config = serde_json::from_slice(&old_bytes)?;
    let current: Config = serde_json::from_slice(&new_bytes)?;
    ensure!(
        old.schema_version == 1 && current.schema_version == 1,
        "Unsupported configuration schema"
    );
    old.provider.native.validate(old.provider.kind)?;
    current.provider.native.validate(current.provider.kind)?;
    let old_tools = serde_json::to_vec(&old.provider.native.tools)?;
    let new_tools = serde_json::to_vec(&current.provider.native.tools)?;
    ensure!(
        old.provider.native.tools != current.provider.native.tools,
        "No native tool change to reconcile"
    );
    let mut neutral_old = old.provider.clone();
    let mut neutral_new = current.provider.clone();
    neutral_old.native.tools.clear();
    neutral_new.native.tools.clear();
    ensure!(
        serde_json::to_value(neutral_old)? == serde_json::to_value(neutral_new)?,
        "Only provider.native.tools may differ; provider, account, model and other grants must stay unchanged"
    );
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
        [session],
        |r| r.get(0),
    )?;
    ensure!(exists, "Existing session required");
    let running: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session=?1 AND state='running')",
        [session],
        |r| r.get(0),
    )?;
    ensure!(
        !running,
        "Session has a running turn; inspect its existing outcome first"
    );
    let choice_bytes: String = db
        .query_row(
            "SELECT data FROM session_model_choices WHERE session=?1",
            [session],
            |r| r.get(0),
        )
        .context("Existing saved model choice required")?;
    let choice: model_selection::Selection = serde_json::from_str(&choice_bytes)?;
    let before = model_selection::apply(&old, &choice)
        .context("Previous configuration must prove the exact saved route and choice")?;
    let next = model_selection::Selection {
        route: model_selection::route(&current),
        model: choice.model.clone(),
        reasoning_effort: choice.reasoning_effort.clone(),
    };
    ensure!(
        next.route != choice.route,
        "Saved choice already uses the current route"
    );
    let after = model_selection::apply(&current, &next)
        .context("Saved model and reasoning must still be offered")?;
    crate::model_policy::validate(&before.provider)?;
    crate::model_policy::validate(&after.provider)?;
    ensure!(
        before.provider.fingerprint() == after.provider.fingerprint(),
        "Provider identity must stay unchanged"
    );
    let binding: String = db
        .query_row(
            "SELECT provider_key FROM session_providers WHERE session=?1",
            [session],
            |r| r.get(0),
        )
        .context("Existing provider binding required")?;
    ensure!(
        binding == before.provider.fingerprint()
            || binding == before.provider.insertion_order_fingerprint(),
        "Saved provider identity does not match the selected model"
    );
    let identity: String =
        db.query_row("SELECT value FROM history_identity WHERE id=1", [], |r| {
            r.get(0)
        })?;
    // Change only the route value, retaining the original serialized choice in
    // the receipt rather than silently losing its original encoding.
    let next_choice_bytes = replace_route(&choice_bytes, &choice.route, &next.route)?;
    let plan = Plan {
        schema: SCHEMA.into(),
        operation_id: operation.into(),
        home: home.into(),
        session_id: session.into(),
        previous_config: previous.into(),
        previous_config_sha256: digest(&old_bytes),
        current_config_sha256: digest(&new_bytes),
        history_identity: identity,
        session_snapshot_sha256: snapshot(db, session)?,
        saved_choice_sha256: digest(choice_bytes.as_bytes()),
        provider_binding_sha256: digest(binding.as_bytes()),
        previous_route: choice.route,
        current_route: next.route,
        previous_tools_sha256: digest(&old_tools),
        current_tools_sha256: digest(&new_tools),
        previous_tool_count: old.provider.native.tools.len(),
        current_tool_count: current.provider.native.tools.len(),
        previous_tools: old.provider.native.tools.clone(),
        current_tools: current.provider.native.tools.clone(),
    };
    Ok(Evidence {
        plan,
        choice_bytes,
        next_choice_bytes,
    })
}

fn replace_route(raw: &str, before: &str, after: &str) -> Result<String> {
    // Stored Selection is exactly {route,model,reasoning_effort}. A route is a
    // SHA-256 hex string; serde parsing above rejects duplicate/unknown fields.
    ensure!(
        before.len() == 64 && before.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid saved route digest"
    );
    let mut value: Value = serde_json::from_str(raw)?;
    value["route"] = json!(after);
    // Only the route field changes semantically; the original bytes are retained
    // in the owner event so serialization changes are explicit and reversible.
    Ok(serde_json::to_string(&value)?)
}

fn receipt(db: &Connection, operation: &str) -> Result<Option<Value>> {
    let mut query = db.prepare(
        "SELECT data FROM events WHERE kind=?1 AND json_extract(data,'$.operation_id')=?2",
    )?;
    let results = query
        .query_map(params![EVENT, operation], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        results.len() <= 1,
        "Ambiguous operation receipts; no reconciliation attempted"
    );
    results
        .first()
        .map(|r| Ok(serde_json::from_str(r)?))
        .transpose()
}

pub fn prepare(home_path: &Path, session: &str, previous_config: &Path) -> Result<Plan> {
    uuid(session)?;
    let home = home(home_path)?;
    let _locks = locks(&home, session)?;
    ensure!(
        previous_config.is_absolute(),
        "Use an absolute previous configuration path"
    );
    let previous = previous_config.canonicalize()?;
    let mut db = database(&home, false)?;
    let tx = db.transaction()?;
    Ok(evidence(
        &tx,
        &home,
        session,
        &previous,
        &uuid::Uuid::new_v4().to_string(),
    )?
    .plan)
}

pub fn inspect(home_path: &Path, operation: &str) -> Result<Value> {
    uuid(operation)?;
    let home = home(home_path)?;
    let db = database(&home, false)?;
    Ok(receipt(&db, operation)?.unwrap_or_else(
        || json!({"schema":SCHEMA,"operation_id":operation,"status":"not_committed"}),
    ))
}

pub fn apply(home_path: &Path, plan_path: &Path) -> Result<Value> {
    let plan: Plan = serde_json::from_slice(&read(plan_path, 256 * 1024)?)?;
    ensure!(plan.schema == SCHEMA, "Unsupported reconciliation plan");
    uuid(&plan.operation_id)?;
    uuid(&plan.session_id)?;
    let home = home(home_path)?;
    ensure!(
        plan.home == home,
        "Plan belongs to a different runtime home"
    );
    let _locks = locks(&home, &plan.session_id)?;
    let plan_hash = digest(&serde_json::to_vec(&plan)?);
    let mut db = database(&home, true)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(receipt) = receipt(&tx, &plan.operation_id)? {
        ensure!(
            receipt["plan_sha256"] == plan_hash && receipt["session_id"] == plan.session_id,
            "Operation ID already belongs to a different plan"
        );
        return Ok(receipt);
    }
    let observed = evidence(
        &tx,
        &home,
        &plan.session_id,
        &plan.previous_config,
        &plan.operation_id,
    )?;
    ensure!(
        observed.plan == plan,
        "Reconciliation evidence or current state changed; prepare and review a new plan"
    );
    // Detect configuration/evidence edits even by a writer not obeying our lock.
    ensure!(
        digest(&read(&home.join("runtime.json"), 512 * 1024)?) == plan.current_config_sha256
            && digest(&read(&plan.previous_config, 512 * 1024)?) == plan.previous_config_sha256,
        "Configuration evidence changed during reconciliation"
    );
    let count = tx.execute(
        "UPDATE session_model_choices SET data=?1 WHERE session=?2 AND data=?3",
        params![
            observed.next_choice_bytes,
            plan.session_id,
            observed.choice_bytes
        ],
    )?;
    ensure!(count == 1, "Saved choice changed during reconciliation");
    let result = json!({
        "schema":SCHEMA, "status":"committed", "operation_id":plan.operation_id, "session_id":plan.session_id,
        "plan_sha256":plan_hash, "plan":plan, "authority":"local_installation_owner",
        "previous_choice_json":observed.choice_bytes, "current_choice_json":observed.next_choice_bytes,
        "history_preserved":true, "inputs_preserved":true, "work_started":false,
        "scope":"provider.native.tools_only", "committed_at":crate::store::now(),
    });
    tx.execute(
        "INSERT INTO events(session,kind,at,data) VALUES(?1,?2,?3,?4)",
        params![
            plan.session_id,
            EVENT,
            result["committed_at"].as_str().unwrap(),
            serde_json::to_string(&result)?
        ],
    )?;
    tx.commit()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Mode, Provider, ProviderKind},
        store::Store,
    };

    struct Fixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        previous: PathBuf,
        current: Config,
        game: String,
        slack: String,
        store: Store,
    }

    impl Fixture {
        fn new(remove: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("engine");
            std::fs::create_dir(&home).unwrap();
            File::create(home.join("configuration.lock")).unwrap();
            let mut old = Config::initial(
                temp.path().join("work"),
                "Fixture".into(),
                "gpt-6.1-sol".into(),
            );
            old.provider = Provider::preset(ProviderKind::Openai, "gpt-6.1-sol".into());
            old.provider.reasoning_effort = Some("medium".into());
            let tools = vec![json!({"type":"web_search"})];
            if remove {
                old.provider.native.tools = tools.clone();
            }
            let previous = temp.path().join("previous.json");
            std::fs::write(&previous, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
            let store = Store::open(home.clone()).unwrap();
            let mut session = store.create(old.workspace.clone(), Mode::Ask).unwrap();
            let game = session.id.clone();
            {
                let _lock = store.lock(&game).unwrap();
                store
                    .select_model_config(&game, &old, "model", "gpt-6.1-sol")
                    .unwrap();
            }
            session.items = vec![
                json!({"type":"reasoning","encrypted_content":"opaque-fixture"}),
                json!({"type":"function_call","call_id":"past-call","name":"effect","arguments":"{}"}),
                json!({"type":"function_call_output","call_id":"past-call","output":"completed"}),
            ];
            store.save(&session).unwrap();
            {
                let db = store.db().unwrap();
                db.execute(
                    "INSERT INTO turns VALUES('past-turn',?1,'completed','before','after')",
                    [&game],
                )
                .unwrap();
                db.execute("INSERT INTO turn_inputs(session,turn,input_id,item,state,recorded,reason) VALUES(?1,'past-turn','uncertain','{}','unknown',0,'retained')", [&game]).unwrap();
                db.execute("INSERT INTO context_checkpoints VALUES(?1,'prefix','provider','before','opaque-checkpoint')", [&game]).unwrap();
            }
            let mut current = old;
            current.provider.native.tools = if remove { vec![] } else { tools };
            std::fs::write(
                home.join("runtime.json"),
                serde_json::to_vec_pretty(&current).unwrap(),
            )
            .unwrap();
            let slack = store
                .create(current.workspace.clone(), Mode::Ask)
                .unwrap()
                .id;
            {
                let _lock = store.lock(&slack).unwrap();
                store
                    .select_model_config(&slack, &current, "model", "gpt-6.1-sol")
                    .unwrap();
            }
            Self {
                _temp: temp,
                home,
                previous,
                current,
                game,
                slack,
                store,
            }
        }
        fn plan(&self) -> Plan {
            prepare(&self.home, &self.game, &self.previous).unwrap()
        }
        fn save_plan(&self, plan: &Plan) -> PathBuf {
            let path = self._temp.path().join("plan.json");
            std::fs::write(&path, serde_json::to_vec_pretty(plan).unwrap()).unwrap();
            path
        }
        fn change_current(&mut self, change: impl FnOnce(&mut Config)) {
            change(&mut self.current);
            std::fs::write(
                self.home.join("runtime.json"),
                serde_json::to_vec(&self.current).unwrap(),
            )
            .unwrap();
        }
        fn unchanged(&self, before: &str) {
            let db = self.store.db().unwrap();
            assert_eq!(snapshot(&db, &self.game).unwrap(), before);
        }
        fn snap(&self) -> String {
            snapshot(&self.store.db().unwrap(), &self.game).unwrap()
        }
    }

    #[test]
    fn addition_and_removal_preserve_original_and_other_session_with_unknown_inputs() {
        for remove in [false, true] {
            let f = Fixture::new(remove);
            let before = f.snap();
            let config = std::fs::read(f.home.join("runtime.json")).unwrap();
            let slack_before = snapshot(&f.store.db().unwrap(), &f.slack).unwrap();
            let preserved = |db: &Connection| {
                [
                    "sessions",
                    "session_providers",
                    "turns",
                    "turn_inputs",
                    "context_checkpoints",
                    "history_imports",
                ]
                .map(|table| {
                    rows(
                        db,
                        &format!(
                            "SELECT * FROM {table} WHERE {}=?1",
                            if table == "sessions" { "id" } else { "session" }
                        ),
                        &f.game,
                    )
                    .unwrap()
                })
            };
            let kept = preserved(&f.store.db().unwrap());
            let plan = f.plan();
            assert_eq!(plan.previous_tool_count, usize::from(remove));
            assert_eq!(plan.current_tool_count, usize::from(!remove));
            f.unchanged(&before);
            assert!(f.store.session_config(&f.game, &f.current).is_err());
            let result = apply(&f.home, &f.save_plan(&plan)).unwrap();
            assert_eq!(result["status"], "committed");
            assert_eq!(result["work_started"], false);
            let selected = f.store.session_config(&f.game, &f.current).unwrap();
            assert_eq!(selected.provider.model, "gpt-6.1-sol");
            assert_eq!(
                selected.provider.reasoning_effort.as_deref(),
                Some("medium")
            );
            let old: Value =
                serde_json::from_str(result["previous_choice_json"].as_str().unwrap()).unwrap();
            let mut new: Value =
                serde_json::from_str(result["current_choice_json"].as_str().unwrap()).unwrap();
            new["route"] = old["route"].clone();
            assert_eq!(old, new);
            assert_eq!(preserved(&f.store.db().unwrap()), kept);
            assert_eq!(
                snapshot(&f.store.db().unwrap(), &f.slack).unwrap(),
                slack_before
            );
            assert_eq!(std::fs::read(f.home.join("runtime.json")).unwrap(), config);
            assert_eq!(inspect(&f.home, &plan.operation_id).unwrap(), result);
            let after = f.snap();
            assert_eq!(apply(&f.home, &f.save_plan(&plan)).unwrap(), result);
            f.unchanged(&after);
            // Lost reply: a later legitimate event does not cause a second apply.
            f.store
                .event(&f.game, None, "later-observation", json!({}))
                .unwrap();
            let later = f.snap();
            assert_eq!(apply(&f.home, &f.save_plan(&plan)).unwrap(), result);
            f.unchanged(&later);
            let mut conflicting = plan;
            conflicting.current_tool_count += 1;
            assert!(apply(&f.home, &f.save_plan(&conflicting)).is_err());
            f.unchanged(&later);
        }
    }

    #[test]
    fn only_native_tools_may_change_not_provider_account_model_or_other_grants() {
        let changes: Vec<fn(&mut Config)> = vec![
            |c| c.provider.endpoint.push_str("/different"),
            |c| c.provider.api_key_env = "DIFFERENT_ACCOUNT".into(),
            |c| c.provider.model = "gpt-6.2-sol".into(),
            |c| c.provider.reasoning_effort = Some("high".into()),
            |c| c.provider.native.image_input = true,
            |c| c.provider.native.disable_function_tools = true,
            |c| c.provider.allow_unverified_model = true,
        ];
        for change in changes {
            let mut f = Fixture::new(false);
            let before = f.snap();
            f.change_current(change);
            assert!(prepare(&f.home, &f.game, &f.previous).is_err());
            f.unchanged(&before);
        }
    }

    #[test]
    fn missing_spoofed_or_invalid_previous_evidence_refuses_without_repair() {
        for case in 0..4 {
            let f = Fixture::new(false);
            let before = f.snap();
            match case {
                0 => std::fs::remove_file(&f.previous).unwrap(),
                1 => std::fs::write(
                    &f.previous,
                    std::fs::read(f.home.join("runtime.json")).unwrap(),
                )
                .unwrap(),
                2 => std::fs::write(&f.previous, b"{\"provider\":\"spoof\"}").unwrap(),
                _ => {
                    let mut old: Value =
                        serde_json::from_slice(&std::fs::read(&f.previous).unwrap()).unwrap();
                    old["provider"]["endpoint"] = json!("https://other.invalid");
                    std::fs::write(&f.previous, serde_json::to_vec(&old).unwrap()).unwrap();
                }
            }
            assert!(prepare(&f.home, &f.game, &f.previous).is_err());
            f.unchanged(&before);
        }
    }

    #[test]
    fn apply_rechecks_current_evidence_history_input_choice_identity_and_turns() {
        for case in 0..9 {
            let f = Fixture::new(false);
            let plan = f.plan();
            let path = f.save_plan(&plan);
            match case {
                0 => {
                    let mut b = std::fs::read(f.home.join("runtime.json")).unwrap();
                    b.push(b' ');
                    std::fs::write(f.home.join("runtime.json"), b).unwrap();
                }
                1 => {
                    let mut b = std::fs::read(&f.previous).unwrap();
                    b.push(b' ');
                    std::fs::write(&f.previous, b).unwrap();
                }
                2 => {
                    f.store
                        .db()
                        .unwrap()
                        .execute("UPDATE sessions SET title='changed' WHERE id=?1", [&f.game])
                        .unwrap();
                }
                3 => {
                    f.store
                        .db()
                        .unwrap()
                        .execute(
                            "UPDATE turn_inputs SET reason='changed' WHERE session=?1",
                            [&f.game],
                        )
                        .unwrap();
                }
                4 => {
                    f.store
                        .db()
                        .unwrap()
                        .execute("UPDATE history_identity SET value='different'", [])
                        .unwrap();
                }
                5 => {
                    f.store
                        .db()
                        .unwrap()
                        .execute(
                            "UPDATE session_model_choices SET data='{}' WHERE session=?1",
                            [&f.game],
                        )
                        .unwrap();
                }
                6 => {
                    f.store.db().unwrap().execute("UPDATE session_providers SET provider_key='different' WHERE session=?1", [&f.game]).unwrap();
                }
                7 => {
                    f.store
                        .db()
                        .unwrap()
                        .execute(
                            "UPDATE turns SET state='running' WHERE session=?1",
                            [&f.game],
                        )
                        .unwrap();
                }
                _ => {
                    f.store
                        .db()
                        .unwrap()
                        .execute(
                            "UPDATE context_checkpoints SET data='different' WHERE session=?1",
                            [&f.game],
                        )
                        .unwrap();
                }
            }
            let before = f.snap();
            assert!(apply(&f.home, &path).is_err(), "case {case}");
            f.unchanged(&before);
            assert_eq!(
                inspect(&f.home, &plan.operation_id).unwrap()["status"],
                "not_committed"
            );
        }
    }

    #[test]
    fn config_and_session_lock_contention_or_absence_refuse_without_creating_files() {
        for config in [false, true] {
            let f = Fixture::new(false);
            let plan = f.plan();
            let path = f.save_plan(&plan);
            let before = f.snap();
            let lock_path = if config {
                f.home.join("configuration.lock")
            } else {
                f.home.join("locks").join(&f.game)
            };
            let lock = file(&lock_path, true).unwrap();
            fs2::FileExt::try_lock_exclusive(&lock).unwrap();
            assert!(prepare(&f.home, &f.game, &f.previous).is_err());
            assert!(apply(&f.home, &path).is_err());
            drop(lock);
            std::fs::remove_file(&lock_path).unwrap();
            assert!(prepare(&f.home, &f.game, &f.previous).is_err());
            assert!(apply(&f.home, &path).is_err());
            assert!(!lock_path.exists());
            f.unchanged(&before);
        }
    }

    #[test]
    fn missing_store_or_selection_and_no_tool_delta_are_not_initialized() {
        let f = Fixture::new(false);
        let absent = f._temp.path().join("missing-home");
        assert!(prepare(&absent, &f.game, &f.previous).is_err());
        assert!(!absent.exists());
        assert!(prepare(&f.home, &f.slack, &f.previous).is_err());
        f.store
            .db()
            .unwrap()
            .execute(
                "DELETE FROM session_model_choices WHERE session=?1",
                [&f.game],
            )
            .unwrap();
        let before = f.snap();
        assert!(prepare(&f.home, &f.game, &f.previous).is_err());
        f.unchanged(&before);
        let empty = f._temp.path().join("empty-home");
        std::fs::create_dir(&empty).unwrap();
        assert!(inspect(&empty, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(!empty.join("history.sqlite3").exists());
    }

    #[test]
    fn removed_saved_model_or_reasoning_and_unsupported_tools_refuse() {
        for case in 0..3 {
            let mut f = Fixture::new(false);
            let before = f.snap();
            f.change_current(|c| match case {
                0 => {
                    c.model_options = vec![model_selection::ModelOption {
                        model: "gpt-6.2-sol".into(),
                        name: "Other".into(),
                        reasoning_effort: Some("medium".into()),
                        reasoning_efforts: vec!["medium".into()],
                    }]
                }
                1 => {
                    c.model_options = vec![model_selection::ModelOption {
                        model: "gpt-6.1-sol".into(),
                        name: "Same".into(),
                        reasoning_effort: Some("high".into()),
                        reasoning_efforts: vec!["high".into()],
                    }]
                }
                _ => c.provider.native.tools = vec![json!({"type":"not_a_supported_tool"})],
            });
            assert!(prepare(&f.home, &f.game, &f.previous).is_err());
            f.unchanged(&before);
        }
    }

    #[test]
    fn prepared_large_tool_declarations_fit_the_bounded_apply_plan() {
        let mut f = Fixture::new(false);
        let mut old: Config = serde_json::from_slice(&std::fs::read(&f.previous).unwrap()).unwrap();
        old.provider.native.tools =
            vec![json!({"type":"web_search","fixture_extension":"x".repeat(48 * 1024)})];
        old.provider.native.validate(old.provider.kind).unwrap();
        std::fs::write(&f.previous, serde_json::to_vec(&old).unwrap()).unwrap();
        // Reproduce a prior explicit choice made under the old declaration.
        let selection = model_selection::Selection {
            route: model_selection::route(&old),
            model: old.provider.model.clone(),
            reasoning_effort: old.provider.reasoning_effort.clone(),
        };
        f.store
            .db()
            .unwrap()
            .execute(
                "UPDATE session_model_choices SET data=?1 WHERE session=?2",
                params![serde_json::to_string(&selection).unwrap(), f.game],
            )
            .unwrap();
        f.change_current(|c| {
            c.provider.native.tools =
                vec![json!({"type":"web_search","fixture_extension":"y".repeat(48 * 1024)})]
        });
        let path = f.save_plan(&f.plan());
        assert!(std::fs::metadata(&path).unwrap().len() > 64 * 1024);
        assert_eq!(apply(&f.home, &path).unwrap()["status"], "committed");
    }

    #[test]
    fn other_session_writer_refuses_before_update_then_same_plan_applies_after_rollback() {
        let f = Fixture::new(false);
        let plan = f.plan();
        let path = f.save_plan(&plan);
        let before = f.snap();
        let other_before = snapshot(&f.store.db().unwrap(), &f.slack).unwrap();
        // A different session can have its own writer. The target's two file
        // locks are available; SQLite must still refuse concurrent mutation.
        let mut other = database(&f.home, true).unwrap();
        let transaction = other
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "UPDATE sessions SET title='other writer pending' WHERE id=?1",
                [&f.slack],
            )
            .unwrap();
        let error = apply(&f.home, &path).unwrap_err();
        assert!(error.to_string().contains("locked"), "{error:#}");
        f.unchanged(&before);
        assert_eq!(
            inspect(&f.home, &plan.operation_id).unwrap()["status"],
            "not_committed"
        );
        transaction.rollback().unwrap();
        assert_eq!(
            snapshot(&f.store.db().unwrap(), &f.slack).unwrap(),
            other_before
        );
        assert_eq!(apply(&f.home, &path).unwrap()["status"], "committed");
        assert_eq!(
            snapshot(&f.store.db().unwrap(), &f.slack).unwrap(),
            other_before
        );
        let count: i64 = f
            .store
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM events WHERE kind=?1", [EVENT], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}
