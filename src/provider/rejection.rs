//! Why a provider refused a request, reduced to identifiers this engine knows.
//! A rejection body can echo prompts, credentials and deployment names, so the
//! provider's own message, code and parameter text never leave this module.
use crate::config::{Provider, ProviderKind};
use futures_util::StreamExt;
use serde_json::Value;

/// A provider's answer to a refused request. Callers see only what is derived.
pub(super) struct Refusal(Value);

impl Refusal {
    /// At most 8 KiB of the body, awaited briefly. A larger, slower or non-JSON
    /// body carries no structured reason; it is not an error of its own.
    pub(super) async fn read(response: reqwest::Response) -> Self {
        let read = async {
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(Ok(part)) = stream.next().await {
                if bytes.len() + part.len() > 8192 {
                    break;
                }
                bytes.extend_from_slice(&part);
            }
            bytes
        };
        let body = tokio::time::timeout(std::time::Duration::from_secs(5), read)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self(body.unwrap_or(Value::Null))
    }

    /// The provider's `error.code`, otherwise its `error.type`, when allowlisted.
    pub(super) fn reason(&self) -> Option<&'static str> {
        // Never copy arbitrary provider messages, parameters or codes into logs.
        let known = |value: &Value| {
            Some(match value.as_str()? {
                "context_length_exceeded" => "context_length_exceeded",
                "rate_limit_exceeded" | "rate_limit_error" => "rate_limited",
                "invalid_api_key" | "authentication_error" => "authentication_failed",
                "insufficient_quota" => "insufficient_quota",
                "model_not_found" => "model_not_found",
                "unsupported_parameter" => "unsupported_parameter",
                "unknown_parameter" => "unknown_parameter",
                "unsupported_value" => "unsupported_value",
                "invalid_request_error" => "invalid_request",
                _ => return None,
            })
        };
        known(&self.0["error"]["code"]).or_else(|| known(&self.0["error"]["type"]))
    }

    /// The refused request field, only when the provider names a top-level
    /// field this engine sent or can send for this protocol. A server may
    /// refuse a default it applied to a field left out. The result is the
    /// engine's own spelling, never the provider's text.
    fn field<'a>(&self, kind: ProviderKind, sent: &'a Value) -> Option<&'a str> {
        let named = self.0["error"]["param"].as_str()?;
        let top = named.split(['.', '[']).next()?;
        if let Some((name, _)) = sent.as_object().and_then(|o| o.get_key_value(top)) {
            return Some(name);
        }
        super::Provider::request_fields(kind)
            .iter()
            .copied()
            .find(|name| *name == top)
    }

    /// The failure shown for a refused model request.
    pub(super) fn describe(&self, p: &Provider, status: u16, sent: &Value) -> String {
        let kind = p.kind.id();
        let (reason, field) = (self.reason(), self.field(p.kind, sent));
        if reason.is_none() && field.is_none() {
            return format!(
                "{kind} request failed (HTTP {status}); check this provider's credential, endpoint, model and quota"
            );
        }
        let reason = reason.unwrap_or("provider_rejected");
        let Some(field) = field else {
            return format!("{kind} request failed (HTTP {status}, {reason})");
        };
        let mut text = format!("{kind} request failed (HTTP {status}, {reason}: {field})");
        // The owner corrects the setting; a refused request is never resent.
        if !matches!(
            p.kind,
            ProviderKind::ChatCompletions | ProviderKind::Litellm
        ) {
            return text;
        }
        if field == p.token_limit_parameter.id()
            && matches!(reason, "unsupported_parameter" | "unknown_parameter")
        {
            text.push_str(&format!(
                "; set provider.token_limit_parameter to {} for this server (--token-limit-parameter)",
                p.token_limit_parameter.other().id()
            ));
        } else if field == "reasoning_effort" {
            text.push_str(if sent.get(field).is_some() {
                "; set provider.reasoning_effort to a value this server accepts (--reasoning-effort)"
            } else {
                "; no reasoning effort is configured, so this server used its default: set provider.reasoning_effort explicitly, for example none (--reasoning-effort)"
            });
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TokenLimitParameter;
    use serde_json::json;

    const CANARY: &str = "PRIVATE_CANARY";

    #[test]
    fn only_known_codes_and_types_become_reasons() {
        let reason = |error: Value| Refusal(json!({"error":error})).reason();
        assert_eq!(
            reason(json!({"code":"unsupported_parameter","type":"invalid_request_error"})),
            Some("unsupported_parameter")
        );
        assert_eq!(
            reason(json!({"code":null,"type":"authentication_error"})),
            Some("authentication_failed")
        );
        // A gateway's own code does not hide a type this engine knows.
        assert_eq!(
            reason(json!({"code":"400","type":"invalid_request_error"})),
            Some("invalid_request")
        );
        assert_eq!(reason(json!({"code":400,"type":CANARY})), None);
        assert_eq!(reason(json!({"code":CANARY})), None);
        assert_eq!(reason(json!(CANARY)), None);
        assert_eq!(Refusal(Value::Null).reason(), None);
    }

    #[test]
    fn a_refused_field_is_named_only_in_the_engines_own_words() {
        let sent = json!({"model":"m","messages":[],"max_tokens":1,"stream_options":{}});
        let chat = ProviderKind::ChatCompletions;
        let field =
            |kind, param: Value| Refusal(json!({"error":{"param":param}})).field(kind, &sent);
        assert_eq!(field(chat, json!("max_tokens")), Some("max_tokens"));
        assert_eq!(field(chat, json!("messages[3].content")), Some("messages"));
        assert_eq!(
            field(chat, json!("stream_options.include_usage")),
            Some("stream_options")
        );
        // A field this protocol can carry is named even when it was left out:
        // the server may be refusing the default it applied instead.
        assert_eq!(
            field(chat, json!("reasoning_effort")),
            Some("reasoning_effort")
        );
        assert_eq!(
            field(ProviderKind::Anthropic, json!("reasoning_effort")),
            None
        );
        assert_eq!(
            field(ProviderKind::AzureOpenai, json!("reasoning.effort")),
            Some("reasoning")
        );
        for foreign in [
            json!(CANARY),
            json!("temperature"),
            json!("Reasoning_Effort"),
            json!(""),
            json!(null),
            json!(7),
        ] {
            assert_eq!(field(chat, foreign), None);
        }
    }

    #[test]
    fn failure_text_carries_the_reason_and_never_provider_text() {
        let mut p = Provider::preset(ProviderKind::ChatCompletions, "m".into());
        let sent = |name: &str| json!({"model":"m","messages":[],name:1});
        let refused = |code: &str, param: &str| {
            Refusal(
                json!({"error":{"message":CANARY,"type":"invalid_request_error","param":param,"code":code}}),
            )
        };
        assert_eq!(
            refused("unsupported_parameter", "max_tokens").describe(&p, 400, &sent("max_tokens")),
            "chat-completions request failed (HTTP 400, unsupported_parameter: max_tokens); set provider.token_limit_parameter to max_completion_tokens for this server (--token-limit-parameter)"
        );
        // The same advice in the other direction, for a server that knows only the older name.
        p.token_limit_parameter = TokenLimitParameter::MaxCompletionTokens;
        let newer = sent("max_completion_tokens");
        assert_eq!(
            refused("unknown_parameter", "max_completion_tokens").describe(&p, 400, &newer),
            "chat-completions request failed (HTTP 400, unknown_parameter: max_completion_tokens); set provider.token_limit_parameter to max_tokens for this server (--token-limit-parameter)"
        );
        // A refused value is not a refused name, and other protocols have no such setting.
        assert_eq!(
            refused(CANARY, "max_completion_tokens").describe(&p, 400, &newer),
            "chat-completions request failed (HTTP 400, invalid_request: max_completion_tokens)"
        );
        let anthropic = Provider::preset(ProviderKind::Anthropic, "m".into());
        assert_eq!(
            refused("unsupported_parameter", "max_tokens").describe(
                &anthropic,
                400,
                &sent("max_tokens")
            ),
            "anthropic request failed (HTTP 400, unsupported_parameter: max_tokens)"
        );
        // OpenAI's Chat Completions answer when tools meet its default reasoning.
        let default_effort = Refusal(
            json!({"error":{"message":CANARY,"type":"invalid_request_error","param":"reasoning_effort","code":null}}),
        );
        assert_eq!(
            default_effort.describe(&p, 400, &newer),
            "chat-completions request failed (HTTP 400, invalid_request: reasoning_effort); no reasoning effort is configured, so this server used its default: set provider.reasoning_effort explicitly, for example none (--reasoning-effort)"
        );
        let mut configured = newer.clone();
        configured["reasoning_effort"] = json!("high");
        assert_eq!(
            refused("unsupported_value", "reasoning_effort").describe(&p, 400, &configured),
            "chat-completions request failed (HTTP 400, unsupported_value: reasoning_effort); set provider.reasoning_effort to a value this server accepts (--reasoning-effort)"
        );
        assert_eq!(
            default_effort.describe(&anthropic, 400, &sent("max_tokens")),
            "anthropic request failed (HTTP 400, invalid_request)"
        );
        assert_eq!(
            Refusal(json!({"error":{"message":CANARY,"code":"invalid_api_key"}}))
                .describe(&p, 401, &newer),
            "chat-completions request failed (HTTP 401, authentication_failed)"
        );
        assert_eq!(
            Refusal(json!({"error":{"message":CANARY,"code":CANARY,"param":"model"}}))
                .describe(&p, 400, &newer),
            "chat-completions request failed (HTTP 400, provider_rejected: model)"
        );
        // Nothing recognizable keeps the existing advice, exactly.
        for opaque in [
            json!({"error":{"message":CANARY,"type":CANARY,"param":CANARY,"code":CANARY}}),
            json!({"error":CANARY}),
            Value::Null,
        ] {
            assert_eq!(
                Refusal(opaque).describe(&p, 429, &newer),
                "chat-completions request failed (HTTP 429); check this provider's credential, endpoint, model and quota"
            );
        }
    }
}
