//! Optional local management for this implementation; never an ACP requirement.
use crate::config::{Config, Provider, ProviderKind, private_write};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn schema() -> Value {
    use clap::ValueEnum;
    let mut schema = serde_json::to_value(schemars::schema_for!(Config)).unwrap();
    schema["x-agent-runtime"] = json!({
        "contract_version":1,"engine_version":env!("CARGO_PKG_VERSION"),
        "configuration_file":"runtime.json","applies":"new ACP processes",
        "provider_changes_require_new_session":true,
        "secret_values_in_config":false,
        "sections":[
            {"id":"identity","title":"Agent","fields":["name","instructions"]},
            {"id":"model","title":"Model connection","fields":["provider"]},
            {"id":"decisions","title":"Decision services","fields":["decisions"]},
            {"id":"context","title":"Conversation memory","fields":["context"]},
            {"id":"permissions","title":"Permissions","fields":["mode","allowed_mcp_mutations","trusted_mcp_readonly_urls","allow_client_stdio_mcp"]},
            {"id":"advanced","title":"Advanced","fields":["limits","workspace"],"advanced":true}
        ],
        "provider_presets":ProviderKind::value_variants().iter().map(|k|Provider::preset(*k,String::new())).collect::<Vec<_>>(),
        "decision_presets":crate::decisions::Kind::value_variants().iter().map(|k|crate::decisions::Backend::preset(*k,format!("{k:?}").to_lowercase())).collect::<Vec<_>>()
    });
    schema
}

pub fn inspect(home: &Path) -> Result<Value> {
    let raw = std::fs::read(home.join("runtime.json"))?;
    let config: Config = serde_json::from_slice(&raw)?;
    config.validate(home)?;
    Ok(
        json!({"schema_version":1,"revision":revision(&raw),"config":config,
        "capabilities":crate::capabilities::report(&config),"restart_required":true,
        "changes_apply":"new ACP processes; provider changes require new sessions",
        "credential_values_included":false}),
    )
}

pub fn load_candidate(home: &Path, file: &Path) -> Result<Config> {
    ensure!(
        std::fs::metadata(file)?.len() <= 512 * 1024,
        "Configuration exceeds 512 KiB"
    );
    let c: Config = serde_json::from_slice(&std::fs::read(file)?)?;
    c.validate(home)?;
    Ok(c)
}

pub fn configure(home: &Path, candidate: &Config, expected: &str) -> Result<Value> {
    candidate.validate(home)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let lock = opts.open(home.join("configuration.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Another configuration update is running")?;
    let raw = std::fs::read(home.join("runtime.json"))?;
    ensure!(
        revision(&raw) == expected,
        "Configuration changed. Inspect it again before saving your changes."
    );
    let old: Config = serde_json::from_slice(&raw)?;
    let changed_provider = old.provider.fingerprint() != candidate.provider.fingerprint();
    if changed_provider && old.provider.kind == ProviderKind::Openai {
        crate::store::Store::open(home.to_owned())?
            .pin_legacy_provider(&old.provider.fingerprint())?;
    }
    let bytes = serde_json::to_vec_pretty(candidate)?;
    let backup_dir = home.join("configuration-backups");
    crate::config::private_dir(&backup_dir)?;
    let backup = backup_dir.join(format!("runtime-{}.json", revision(&raw)));
    if !backup.exists() {
        private_write(&backup, &raw)?;
    }
    let temp = home.join(format!("runtime-{}.tmp", uuid::Uuid::new_v4()));
    private_write(&temp, &bytes)?;
    if let Err(error) = std::fs::rename(&temp, home.join("runtime.json")) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(
        json!({"schema_version":1,"revision":revision(&bytes),"restart_required":true,
        "provider_changed":changed_provider,"existing_history_preserved":true,"previous_config_file":backup,
        "changes_apply":"new ACP processes; provider changes require new sessions"}),
    )
}
