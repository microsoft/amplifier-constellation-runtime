//! A versioned support catalog, not an account-entitlement lookup. Choices are
//! concrete; refreshing this catalog never rewrites an installation or session.
use crate::config::{Provider, ProviderKind};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
pub fn catalog() -> Value {
    json!({"asOf":"2026-11-04","policy":"latest_verified_per_class","automaticModelChanges":false,"accountAvailabilityVerified":false,
 "openai":[{"class":"astra","model":"gpt-6-astra"},{"class":"sol","model":"gpt-6.1-sol"},{"class":"terra","model":"gpt-6.1-terra"},{"class":"luna","model":"gpt-6-luna"}],
 "anthropic":[{"class":"fable","model":"claude-fable-5-1"},{"class":"opus","model":"claude-opus-5-5"},{"class":"sonnet","model":"claude-sonnet-5-5"},{"class":"haiku","model":"claude-haiku-5-5"}],
 "gemini":[{"class":"reasoning","model":"gemini-4-argon"},{"class":"flash","model":"gemini-3.8-flash"},{"class":"flash-lite","model":"gemini-3.5-flash-lite"}],
 "unlistedModels":"An OpenAI or Claude model name outside this catalog is refused unless the owner sets provider.allow_unverified_model. Native OpenAI opt-in remains limited to numeric GPT-6-and-later names; retired or unclassified generations stay refused. An accepted name is sent unchanged, reported as unverified, never substituted and never added to this catalog.",
 "specialists":"Image, audio and decision models use independent capability configuration.",
 "otherRoutes":"Azure deployment names, local servers and gateways require route-specific capability validation; third-party ACP engines own their model policies."})
}
/// How a configured model name relates to the dated catalog. A statement about
/// the name only; it never verifies an account, route or model capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStatus {
    /// Named in this version's catalog for its provider.
    Listed,
    /// A vendor model name outside the catalog. Valid only with the owner's
    /// explicit `allow_unverified_model`; this runtime has no record for it.
    Unverified,
    /// No catalog rule applies: deployment names, gateways, local servers and
    /// providers whose routes need their own capability checks.
    NotEnforced,
}
pub fn catalog_status(kind: ProviderKind, model: &str) -> CatalogStatus {
    let (key, vendor_name) = match kind {
        ProviderKind::Openai => (
            "openai",
            ["gpt-", "o1", "o3", "o4"]
                .iter()
                .any(|prefix| model.starts_with(prefix)),
        ),
        ProviderKind::Anthropic => ("anthropic", model.starts_with("claude-")),
        _ => return CatalogStatus::NotEnforced,
    };
    if !vendor_name {
        CatalogStatus::NotEnforced
    } else if catalog()[key]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["model"] == model)
    {
        CatalogStatus::Listed
    } else {
        CatalogStatus::Unverified
    }
}
pub fn validate(p: &Provider) -> Result<()> {
    ensure!(
        !p.allow_unverified_model
            || matches!(p.kind, ProviderKind::Openai | ProviderKind::Anthropic),
        "allow_unverified_model applies only to the openai and anthropic providers, whose model names are checked against the runtime model catalog"
    );
    ensure!(
        p.allow_unverified_model || catalog_status(p.kind, &p.model) != CatalogStatus::Unverified,
        "This model is outside the current supported classes. Choose a current model from the runtime model catalog; saved sessions retain their original binding. For a model newer than this catalog, the owner can accept this exact name as unverified with allow_unverified_model (--allow-unverified-model)."
    );
    if p.kind == ProviderKind::Openai
        && p.allow_unverified_model
        && catalog_status(p.kind, &p.model) == CatalogStatus::Unverified
    {
        // This is the engine's generation retirement rule, not a capability
        // inference. An accepted, unlisted name remains explicitly unverified.
        let generation = p
            .model
            .strip_prefix("gpt-")
            .and_then(|version| version.split(['.', '-']).next())
            .filter(|major| !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|major| major.parse::<u32>().ok());
        ensure!(
            generation.is_some_and(|major| major >= 6),
            "allow_unverified_model does not enable retired or unclassified native OpenAI generations; choose a listed model or an explicit GPT-6-and-later model name"
        );
    }
    if p.kind == ProviderKind::Anthropic {
        ensure!(
            p.thinking_budget_tokens.is_none(),
            "Current Claude classes use adaptive thinking; replace thinking_budget_tokens with a supported reasoning_effort"
        );
    }
    if p.kind == ProviderKind::Openai
        && let Some(e) = &p.reasoning_effort
    {
        ensure!(
            e != "minimal",
            "Current GPT classes use low rather than minimal effort"
        );
        if matches!(p.model.as_str(), "gpt-6-astra" | "gpt-6.1-sol") {
            ensure!(
                e != "none",
                "This model class requires reasoning; select low or higher"
            );
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_classes_and_legacy_migration() {
        for (kind, key) in [
            (ProviderKind::Openai, "openai"),
            (ProviderKind::Anthropic, "anthropic"),
        ] {
            for m in catalog()[key].as_array().unwrap() {
                assert!(
                    validate(&Provider::preset(kind, m["model"].as_str().unwrap().into())).is_ok()
                );
            }
        }
        assert!(
            validate(&Provider::preset(
                ProviderKind::Openai,
                "gpt-5.6-sol".into()
            ))
            .is_err()
        );
        assert!(
            validate(&Provider::preset(
                ProviderKind::Anthropic,
                "claude-opus-4-6".into()
            ))
            .is_err()
        );
        assert!(
            validate(&Provider::preset(
                ProviderKind::AzureOpenai,
                "my-deployment".into()
            ))
            .is_ok()
        );
    }

    fn opted_in(kind: ProviderKind, model: &str) -> Provider {
        let mut provider = Provider::preset(kind, model.into());
        provider.allow_unverified_model = true;
        provider
    }

    #[test]
    fn opt_in_never_restores_retired_native_openai_generations() {
        for model in ["gpt-3.5-turbo", "gpt-4o", "gpt-5.2", "o1", "o3", "o4-mini"] {
            assert!(
                validate(&opted_in(ProviderKind::Openai, model)).is_err(),
                "Retired native OpenAI model was accepted: {model}"
            );
        }
        for model in [
            "gpt-",
            "gpt-unlisted-fixture",
            "gpt-6x",
            "gpt-+6",
            "gpt-99999999999999999999",
        ] {
            assert!(
                validate(&opted_in(ProviderKind::Openai, model)).is_err(),
                "{model}"
            );
        }
    }

    #[test]
    fn unlisted_vendor_model_needs_the_explicit_opt_in_and_stays_unverified() {
        for (kind, model) in [
            (ProviderKind::Anthropic, "claude-unlisted-fixture"),
            (ProviderKind::Openai, "gpt-6.2-unlisted-fixture"),
            (ProviderKind::Openai, "gpt-7-unlisted-fixture"),
        ] {
            let strict = Provider::preset(kind, model.into());
            assert!(!strict.allow_unverified_model, "The default stays strict");
            let refusal = validate(&strict).unwrap_err().to_string();
            assert!(refusal.contains("supported classes"), "{refusal}");
            assert!(refusal.contains("allow_unverified_model"), "{refusal}");
            assert!(refusal.contains("--allow-unverified-model"), "{refusal}");

            let accepted = opted_in(kind, model);
            validate(&accepted).unwrap();
            // The exact name is kept and never presented as a catalog entry.
            assert_eq!(accepted.model, model);
            assert_eq!(catalog_status(kind, model), CatalogStatus::Unverified);
            assert!(!catalog().to_string().contains(model));
        }
    }

    #[test]
    fn opt_in_changes_no_other_rule() {
        // Listed models are unaffected and keep their status.
        for (kind, key) in [
            (ProviderKind::Openai, "openai"),
            (ProviderKind::Anthropic, "anthropic"),
        ] {
            for m in catalog()[key].as_array().unwrap() {
                let model = m["model"].as_str().unwrap();
                validate(&opted_in(kind, model)).unwrap();
                assert_eq!(catalog_status(kind, model), CatalogStatus::Listed);
            }
        }
        // Request-shape rules still apply to an unverified name.
        let mut claude = opted_in(ProviderKind::Anthropic, "claude-unlisted-fixture");
        claude.thinking_budget_tokens = Some(2048);
        assert!(validate(&claude).is_err());
        let mut gpt = opted_in(ProviderKind::Openai, "gpt-7-unlisted-fixture");
        gpt.reasoning_effort = Some("minimal".into());
        assert!(validate(&gpt).is_err());
        // Routes without a catalog rule have nothing to opt in to.
        for (kind, model) in [
            (ProviderKind::AzureOpenai, "my-deployment"),
            (ProviderKind::Gemini, "gemini-3.8-flash"),
            (ProviderKind::Ollama, "claude-like-local-name"),
        ] {
            assert_eq!(catalog_status(kind, model), CatalogStatus::NotEnforced);
            validate(&Provider::preset(kind, model.into())).unwrap();
            let refusal = validate(&opted_in(kind, model)).unwrap_err().to_string();
            assert!(refusal.contains("applies only"), "{refusal}");
        }
        assert_eq!(
            catalog_status(ProviderKind::Anthropic, "gateway-route"),
            CatalogStatus::NotEnforced
        );
    }

    #[test]
    fn opt_in_is_absent_by_default_and_leaves_session_identity_alone() {
        let listed = Provider::preset(ProviderKind::Anthropic, "claude-sonnet-5-5".into());
        let saved = serde_json::to_value(&listed).unwrap();
        assert!(
            saved.get("allow_unverified_model").is_none(),
            "Existing configuration files, revisions and route hashes are unchanged"
        );
        let accepted = opted_in(ProviderKind::Anthropic, "claude-sonnet-5-5");
        let saved = serde_json::to_value(&accepted).unwrap();
        assert_eq!(saved["allow_unverified_model"], true);
        let restored: Provider = serde_json::from_value(saved).unwrap();
        assert!(restored.allow_unverified_model);
        // The binding is the model and destination, not the owner's acceptance.
        assert_eq!(listed.fingerprint(), accepted.fingerprint());
        let temp = tempfile::tempdir().unwrap();
        let mut config =
            crate::config::Config::initial(temp.path().into(), "Fixture".into(), String::new());
        config.provider = listed;
        let route = crate::model_selection::route(&config);
        config.provider = accepted;
        assert_eq!(crate::model_selection::route(&config), route);
    }

    #[test]
    fn session_model_options_follow_the_same_rule_and_report_each_status() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let work = temp.path().join("work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();
        let mut config = crate::config::Config::initial(work, "Fixture".into(), String::new());
        config.provider = Provider::preset(ProviderKind::Anthropic, "claude-sonnet-5-5".into());
        config.model_options = ["claude-sonnet-5-5", "claude-unlisted-fixture"]
            .iter()
            .map(|model| crate::model_selection::ModelOption {
                model: (*model).into(),
                name: (*model).into(),
                reasoning_effort: None,
                reasoning_efforts: vec![],
            })
            .collect();
        let refusal = config.validate(&home).unwrap_err().to_string();
        assert!(refusal.contains("allow_unverified_model"), "{refusal}");
        config.provider.allow_unverified_model = true;
        config.validate(&home).unwrap();

        let report = crate::capabilities::report(&config);
        assert_eq!(report["model"], "claude-sonnet-5-5");
        assert_eq!(report["model_catalog_status"], "listed");
        assert_eq!(
            report["model_options"],
            json!([
                {"model":"claude-sonnet-5-5","model_catalog_status":"listed"},
                {"model":"claude-unlisted-fixture","model_catalog_status":"unverified"}
            ])
        );
        // A session that selects the unlisted option reports it as unverified.
        let selection = crate::model_selection::Selection {
            route: crate::model_selection::route(&config),
            model: "claude-unlisted-fixture".into(),
            reasoning_effort: None,
        };
        let selected = crate::model_selection::apply(&config, &selection).unwrap();
        let report = crate::capabilities::report(&selected);
        assert_eq!(report["model"], "claude-unlisted-fixture");
        assert_eq!(report["model_catalog_status"], "unverified");
        assert_eq!(report["account_and_model_support_verified"], false);
    }
}
