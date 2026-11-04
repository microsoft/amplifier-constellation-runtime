//! Explicit capabilities of this installation, not guesses from model names.
use crate::config::{Config, ProviderKind};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Native {
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub audio_input: bool,
    /// Inline ACP PDF resources, converted to the selected provider's file input.
    #[serde(default)]
    pub pdf_input: bool,
    #[serde(default)]
    pub computer: Option<crate::computer::Computer>,
    /// Provider-hosted tools only. Local execution still uses permissioned tools.
    #[serde(default)]
    pub tools: Vec<Value>,
    /// Gemini image/audio models often do not support function tools.
    #[serde(default)]
    pub disable_function_tools: bool,
    #[serde(default)]
    pub response_modalities: Vec<String>,
    #[serde(default)]
    pub image_config: Option<Value>,
    #[serde(default)]
    pub speech_config: Option<Value>,
    #[serde(default)]
    pub json_schema: Option<Value>,
    #[serde(default)]
    pub anthropic_betas: Vec<String>,
}
impl Native {
    pub fn is_empty(&self) -> bool {
        !self.image_input
            && !self.audio_input
            && !self.pdf_input
            && self.computer.is_none()
            && self.tools.is_empty()
            && !self.disable_function_tools
            && self.response_modalities.is_empty()
            && self.image_config.is_none()
            && self.speech_config.is_none()
            && self.json_schema.is_none()
            && self.anthropic_betas.is_empty()
    }
    pub fn validate(&self, k: ProviderKind) -> Result<()> {
        ensure!(
            serde_json::to_vec(self)?.len() <= 64 * 1024,
            "Native capability configuration too large"
        );
        ensure!(self.tools.len() <= 8, "Too many native tools");
        if let Some(computer) = &self.computer {
            ensure!(
                matches!(k, ProviderKind::Openai | ProviderKind::AzureOpenai),
                "Native computer calls currently require the OpenAI/Azure Responses protocol"
            );
            computer.validate()?;
        }
        ensure!(
            !self.image_input || k != ProviderKind::Mock,
            "Mock has no vision model"
        );
        ensure!(
            !self.audio_input || k == ProviderKind::Gemini,
            "Audio input currently requires Gemini"
        );
        ensure!(
            !self.pdf_input
                || matches!(
                    k,
                    ProviderKind::Openai
                        | ProviderKind::AzureOpenai
                        | ProviderKind::Anthropic
                        | ProviderKind::Gemini
                        | ProviderKind::ChatCompletions
                        | ProviderKind::Litellm
                ),
            "PDF input requires a supported document-capable provider protocol"
        );
        ensure!(
            self.response_modalities.is_empty() || k == ProviderKind::Gemini,
            "Response modalities apply to Gemini"
        );
        ensure!(
            self.response_modalities
                .iter()
                .all(|s| matches!(s.as_str(), "TEXT" | "IMAGE" | "AUDIO")),
            "Invalid response modality"
        );
        ensure!(
            self.response_modalities.iter().all(|s| s == "TEXT") || self.disable_function_tools,
            "Image/audio generation requires disable_function_tools for this Gemini connection"
        );
        ensure!(
            (self.image_config.is_none() && self.speech_config.is_none())
                || k == ProviderKind::Gemini,
            "Image/speech configuration applies to Gemini"
        );
        ensure!(
            self.json_schema.is_none() || k != ProviderKind::Mock,
            "Mock does not produce structured model output"
        );
        ensure!(
            self.json_schema.as_ref().is_none_or(Value::is_object),
            "JSON schema must be an object"
        );
        ensure!(
            self.anthropic_betas.is_empty() || k == ProviderKind::Anthropic,
            "Beta headers apply only to Anthropic"
        );
        ensure!(self.anthropic_betas.iter().all(|s|!s.is_empty()&&s.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-')),"Invalid beta header");
        let mut seen = std::collections::HashSet::new();
        for t in &self.tools {
            ensure!(t.is_object(), "Native tool must be an object");
            let name = match k {
                ProviderKind::Openai | ProviderKind::AzureOpenai => {
                    let ty = t["type"].as_str().unwrap_or("");
                    ensure!(
                        [
                            "web_search",
                            "web_search_preview",
                            "file_search",
                            "code_interpreter",
                            "image_generation"
                        ]
                        .contains(&ty),
                        "Unsupported hosted Responses tool; client/computer/shell/MCP tools use the runtime's separate execution boundary"
                    );
                    if ty == "code_interpreter" {
                        ensure!(
                            t["container"].is_string() || t["container"].is_object(),
                            "Code interpreter needs a container setting"
                        );
                    }
                    if ty == "file_search" {
                        ensure!(
                            t["vector_store_ids"]
                                .as_array()
                                .is_some_and(|v| !v.is_empty()),
                            "File search needs vector_store_ids"
                        );
                    }
                    ty.to_string()
                }
                ProviderKind::Anthropic => {
                    let ty = t["type"].as_str().unwrap_or("");
                    let name = t["name"].as_str().unwrap_or("");
                    ensure!(
                        ["web_search", "web_fetch", "code_execution"].contains(&name)
                            && ty.strip_prefix(&format!("{name}_")).is_some_and(
                                |v| v.len() == 8 && v.bytes().all(|c| c.is_ascii_digit())
                            ),
                        "Unsupported Anthropic server tool"
                    );
                    name.to_string()
                }
                ProviderKind::Gemini => {
                    let o = t.as_object().unwrap();
                    ensure!(o.len() == 1, "One Gemini native tool per entry");
                    let name = o.keys().next().unwrap();
                    ensure!(
                        [
                            "googleSearch",
                            "urlContext",
                            "codeExecution",
                            "googleMaps",
                            "fileSearch"
                        ]
                        .contains(&name.as_str()),
                        "Unsupported Gemini hosted tool"
                    );
                    name.clone()
                }
                _ => anyhow::bail!(
                    "This protocol has no supported provider-hosted tools; use local or MCP tools"
                ),
            };
            ensure!(seen.insert(name), "Duplicate hosted tool");
        }
        Ok(())
    }
}
pub fn report(c: &Config) -> Value {
    json!({"local_commands":{"available":crate::tools::command_isolation().is_some(),"network":c.command_network,"filesystem":"session_workspace_roots","credentials_inherited":false},"provider":c.provider.kind,"model":c.provider.model,"configured":c.provider.native,"decisions":c.decisions.iter().map(|b|json!({"name":b.name,"provider":b.provider,"model":b.model,"expose_to_agent":b.expose_to_agent,"credential_present":std::env::var(&b.api_key_env).is_ok_and(|s|!s.trim().is_empty())})).collect::<Vec<_>>(),"account_and_model_support_verified":false,"supported":{"openai_azure":["image/PDF input","native computer calls through an explicit MCP environment","image generation","web search","file search","code interpreter","structured output"],"anthropic":["image/PDF input","web search","web fetch","code execution","structured output","pause/resume server loops"],"gemini":["image/audio/PDF input","image/audio output","Google search","URL context","code execution","Maps grounding","file search","structured output"],"compatible":["image input (model dependent)","structured output (server dependent)"]},"not_implemented":["hosted remote MCP approval bridging","realtime duplex audio/video","video generation jobs","embedding/index management","automatic model capability discovery"],"notes":"Capabilities are opt-in. Provider/model combinations and account entitlements must be checked on the actual endpoint; unsupported requests fail without a silent downgrade."})
}
