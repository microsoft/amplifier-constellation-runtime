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
 "specialists":"Image, audio and decision models use independent capability configuration.",
 "otherRoutes":"Azure deployment names, local servers and gateways require route-specific capability validation; third-party ACP engines own their model policies."})
}
pub fn validate(p: &Provider) -> Result<()> {
    let catalog = catalog();
    let key = match p.kind {
        ProviderKind::Openai => "openai",
        ProviderKind::Anthropic => "anthropic",
        _ => return Ok(()),
    };
    let vendor_name = match p.kind {
        ProviderKind::Openai => {
            p.model.starts_with("gpt-")
                || p.model.starts_with("o1")
                || p.model.starts_with("o3")
                || p.model.starts_with("o4")
        }
        _ => p.model.starts_with("claude-"),
    };
    if vendor_name {
        ensure!(
            catalog[key]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["model"] == p.model),
            "This model is outside the current supported classes. Choose a current model from the runtime model catalog; saved sessions retain their original binding."
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
}
