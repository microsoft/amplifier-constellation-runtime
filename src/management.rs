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
    let catalog = crate::model_policy::catalog();
    let presets = ProviderKind::value_variants()
        .iter()
        .map(|kind| {
            let class = match kind {
                ProviderKind::Openai => Some(("openai", "sol")),
                ProviderKind::Anthropic => Some(("anthropic", "sonnet")),
                ProviderKind::Gemini => Some(("gemini", "flash")),
                _ => None,
            };
            let model = class
                .and_then(|(provider, class)| {
                    catalog[provider]
                        .as_array()
                        .and_then(|rows| rows.iter().find(|row| row["class"] == class))
                        .and_then(|row| row["model"].as_str())
                })
                .unwrap_or_default();
            Provider::preset(*kind, model.to_owned())
        })
        .collect::<Vec<_>>();
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
        "model_catalog":catalog,
        "provider_presets":presets,
        "decision_presets":crate::decisions::Kind::value_variants().iter().map(|k|crate::decisions::Backend::preset(*k,format!("{k:?}").to_lowercase())).collect::<Vec<_>>()
    });
    schema
}

pub fn inspect(home: &Path) -> Result<Value> {
    let raw = std::fs::read(home.join("runtime.json"))?;
    let config: Config = serde_json::from_slice(&raw)?;
    // Inspection must remain available when a runtime upgrade retires an
    // execution setting. Configure and execution still enforce validation.
    let error = config.validate(home).err().map(|error| error.to_string());
    Ok(
        json!({"schema_version":1,"revision":revision(&raw),"config":config,
        "validation":{"valid":error.is_none(),"error":error},
        "capabilities":if error.is_none(){crate::capabilities::report(&config)}else{Value::Null},"restart_required":true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_are_resolved_from_current_catalog_without_gateway_guesses() {
        let schema = schema();
        let metadata = &schema["x-agent-runtime"];
        for (kind, class) in [
            ("openai", "sol"),
            ("anthropic", "sonnet"),
            ("gemini", "flash"),
        ] {
            let preset = metadata["provider_presets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["kind"] == kind)
                .unwrap();
            let model = metadata["model_catalog"][kind]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["class"] == class)
                .unwrap();
            assert_eq!(preset["model"], model["model"]);
        }
        for p in metadata["provider_presets"].as_array().unwrap() {
            if !["openai", "anthropic", "gemini"].contains(&p["kind"].as_str().unwrap()) {
                assert_eq!(
                    p["model"], "",
                    "Deployment and gateway names remain owner selected"
                );
            }
        }
    }

    #[test]
    fn retired_config_can_be_inspected_and_explicitly_migrated_without_rewriting_history() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let work = temp.path().join("work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        let config = Config::initial(work.clone(), "Fixture".into(), "gpt-5.6-sol".into());
        let raw = serde_json::to_vec(&config).unwrap();
        std::fs::write(home.join("runtime.json"), &raw).unwrap();
        let store = crate::store::Store::open(home.clone()).unwrap();
        let mut session = store.create(work, crate::config::Mode::Ask).unwrap();
        session.items = vec![json!({"role":"user","content":"original history"})];
        store.save(&session).unwrap();
        let old = inspect(&home).unwrap();
        assert_eq!(old["validation"]["valid"], false);
        assert!(old["capabilities"].is_null());
        assert!(
            old["validation"]["error"]
                .as_str()
                .unwrap()
                .contains("supported classes")
        );
        assert_eq!(std::fs::read(home.join("runtime.json")).unwrap(), raw);
        assert!(load_candidate(&home, &home.join("runtime.json")).is_err());
        assert!(configure(&home, &config, old["revision"].as_str().unwrap()).is_err());
        let mut replacement = config;
        replacement.provider.model = "gpt-6.1-sol".into();
        configure(&home, &replacement, old["revision"].as_str().unwrap()).unwrap();
        assert_eq!(inspect(&home).unwrap()["validation"]["valid"], true);
        assert_eq!(store.load(&session.id).unwrap().items, session.items);
        assert!(
            store
                .bind_provider(&session.id, &replacement.provider.fingerprint(), false)
                .is_err()
        );
    }
}
