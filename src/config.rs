use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub name: String,
    pub instructions: String,
    pub workspace: PathBuf,
    /// Owner-approved directories that clients may request as additional roots.
    /// Listing a directory here does not activate it for every session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_workspaces: Vec<PathBuf>,
    /// Explicit trust in a client's ability to append system instructions.
    /// Ordinary persona/context text never enables this authority.
    #[serde(default)]
    pub allow_client_system_prompt: bool,
    /// Connection-local grants; never persisted in configuration or history.
    #[serde(skip)]
    #[schemars(skip)]
    pub session_roots: Option<Vec<PathBuf>>,
    #[serde(skip)]
    #[schemars(skip)]
    pub client_form_elicitation: bool,
    pub provider: Provider,
    pub limits: Limits,
    pub mode: Mode,
    /// Owner-controlled command networking, independent of filesystem grants.
    #[serde(default)]
    pub command_network: CommandNetwork,
    /// Compact completed turns for model requests; full history stays saved.
    #[serde(default)]
    pub context: crate::context::Policy,
    /// Only these explicitly named mutating MCP tools bypass ask mode.
    #[serde(default)]
    pub allowed_mcp_mutations: Vec<String>,
    /// Exact HTTP endpoints whose read-only tool annotations are trusted.
    #[serde(default)]
    pub trusted_mcp_readonly_urls: Vec<String>,
    /// Local MCP startup grants the ACP client host command authority. Keep
    /// disabled for remotely attached clients unless they are fully trusted.
    #[serde(default)]
    pub allow_client_stdio_mcp: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<crate::decisions::Backend>,
}
#[derive(
    Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CommandNetwork {
    #[default]
    Isolated,
    /// Inherit the enclosing Linux network namespace and its firewall policy.
    /// This does not itself restrict access to public internet addresses.
    Inherit,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    #[serde(default)]
    pub kind: ProviderKind,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub auth: ProviderAuth,
    /// This installation's separately authorized ChatGPT account, never Codex credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chatgpt_credentials_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot: Option<crate::copilot::Bridge>,
    #[serde(default)]
    pub thinking_budget_tokens: Option<usize>,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub tls_ca_file: Option<PathBuf>,
    /// Explicit opt-in for a private LAN model server; never inferred.
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub mock_response: Option<String>,
    #[serde(default, skip_serializing_if = "crate::capabilities::Native::is_empty")]
    pub native: crate::capabilities::Native,
}
#[derive(
    Clone,
    Copy,
    Default,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    clap::ValueEnum,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    Openai,
    AzureOpenai,
    Anthropic,
    Gemini,
    Ollama,
    Vllm,
    ChatCompletions,
    Litellm,
    GithubCopilot,
    Mock,
}
#[derive(
    Clone,
    Copy,
    Default,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    clap::ValueEnum,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuth {
    #[default]
    Auto,
    Bearer,
    ApiKey,
    None,
    Chatgpt,
}
impl ProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::AzureOpenai => "azure-openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::ChatCompletions => "chat-completions",
            Self::Litellm => "litellm",
            Self::GithubCopilot => "github-copilot",
            Self::Mock => "mock",
        }
    }
}
impl Provider {
    pub fn preset(kind: ProviderKind, model: String) -> Self {
        use ProviderKind::*;
        let (endpoint, key) = match kind {
            Openai => ("https://api.openai.com/v1/responses", "OPENAI_API_KEY"),
            AzureOpenai => ("", "AZURE_OPENAI_API_KEY"),
            Anthropic => ("https://api.anthropic.com/v1/messages", "ANTHROPIC_API_KEY"),
            Gemini => (
                "https://generativelanguage.googleapis.com/v1beta",
                "GOOGLE_API_KEY",
            ),
            Ollama => ("http://127.0.0.1:11434/api/chat", "OLLAMA_API_KEY"),
            Vllm => ("http://127.0.0.1:8000/v1/responses", "VLLM_API_KEY"),
            ChatCompletions => ("", "CHAT_COMPLETIONS_API_KEY"),
            Litellm => (
                "http://127.0.0.1:4000/v1/chat/completions",
                "LITELLM_API_KEY",
            ),
            Mock => ("", ""),
            GithubCopilot => ("", "COPILOT_GITHUB_TOKEN"),
        };
        Self {
            kind,
            endpoint: endpoint.into(),
            model,
            api_key_env: key.into(),
            reasoning_effort: (kind == Openai).then(|| "medium".into()),
            auth: ProviderAuth::Auto,
            chatgpt_credentials_file: None,
            copilot: None,
            thinking_budget_tokens: None,
            api_version: None,
            tls_ca_file: None,
            allow_insecure_http: false,
            mock_response: None,
            native: Default::default(),
        }
    }
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        // Credentials can rotate without invalidating a session. Model and
        // destination changes require a new context; signed continuation data
        // must never be silently sent to another provider or deployment.
        let mut identity = serde_json::json!({
            "kind":self.kind,"endpoint":self.endpoint,"model":self.model,"api_version":self.api_version
        });
        // Preserve existing key-based fingerprints; account-specific OAuth
        // continuation belongs to this explicit registration's file.
        if self.auth == ProviderAuth::Chatgpt {
            identity["chatgpt_credentials_file"] = serde_json::json!(self.chatgpt_credentials_file);
        }
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&identity).unwrap())
        )
    }
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_model_calls: usize,
    pub max_tool_calls: usize,
    pub max_output_tokens: usize,
    pub max_context_bytes: usize,
    pub turn_seconds: u64,
    pub command_seconds: u64,
    pub tool_output_bytes: usize,
    pub max_parallel_turns: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ReadOnly,
    Ask,
    Workspace,
}
impl Mode {
    pub fn id(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Ask => "ask",
            Self::Workspace => "workspace",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        serde_json::from_value(serde_json::json!(s)).context("Unknown session mode")
    }
}
impl Config {
    pub fn initial(workspace: PathBuf, name: String, model: String) -> Self {
        Self {
            schema_version: 1,
            name,
            instructions: String::new(),
            workspace,
            additional_workspaces: vec![],
            allow_client_system_prompt: false,
            session_roots: None,
            client_form_elicitation: false,
            provider: Provider::preset(ProviderKind::Openai, model),
            limits: Limits {
                max_model_calls: 16,
                max_tool_calls: 48,
                max_output_tokens: 8192,
                max_context_bytes: 768 * 1024,
                turn_seconds: 600,
                command_seconds: 60,
                tool_output_bytes: 64 * 1024,
                max_parallel_turns: 4,
            },
            mode: Mode::Ask,
            command_network: CommandNetwork::Isolated,
            context: Default::default(),
            allowed_mcp_mutations: vec![],
            trusted_mcp_readonly_urls: vec![],
            allow_client_stdio_mcp: false,
            decisions: vec![],
        }
    }
    pub fn load(home: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&std::fs::read(home.join("runtime.json"))?)?;
        c.validate(home)?;
        Ok(c)
    }
    pub fn validate(&self, home: &Path) -> Result<()> {
        ensure!(
            self.command_network == CommandNetwork::Isolated || cfg!(target_os = "linux"),
            "Inherited command networking is currently supported on Linux only"
        );
        self.context.validate()?;
        crate::decisions::validate_backends(&self.decisions)?;
        self.provider.native.validate(self.provider.kind)?;
        ensure!(
            self.schema_version == 1,
            "Unsupported configuration version"
        );
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 200,
            "Invalid name"
        );
        ensure!(
            self.instructions.len() <= 128 * 1024,
            "Instructions too large"
        );
        ensure!(
            self.workspace.is_absolute() && self.workspace.is_dir(),
            "Workspace must be an existing absolute folder"
        );
        let workspace = std::fs::canonicalize(&self.workspace)?;
        let home = std::fs::canonicalize(home)?;
        ensure!(
            !home.starts_with(&workspace),
            "Workspace must not expose runtime state or credentials"
        );
        ensure!(
            self.additional_workspaces.len() <= 32,
            "Too many additional workspaces"
        );
        for root in &self.additional_workspaces {
            ensure!(
                root.is_absolute() && root.is_dir(),
                "Additional workspaces must be existing absolute directories"
            );
            let root = std::fs::canonicalize(root)?;
            ensure!(
                !home.starts_with(&root) && !root.starts_with(&home),
                "Additional workspaces must not expose runtime state or credentials"
            );
            for protected in self.provider.chatgpt_credentials_file.iter().chain(
                self.provider
                    .copilot
                    .iter()
                    .flat_map(|bridge| [&bridge.python, &bridge.script]),
            ) {
                ensure!(
                    !std::fs::canonicalize(protected)?.starts_with(&root),
                    "Additional workspaces must not expose provider credentials or bridge code"
                );
            }
        }
        ensure!(
            self.provider.model.len() <= 200 && !self.provider.model.is_empty(),
            "Set a model"
        );
        ensure!(
            (self.provider.api_key_env.is_empty()
                && (self.provider.kind == ProviderKind::Mock
                    || matches!(
                        self.provider.auth,
                        ProviderAuth::None | ProviderAuth::Chatgpt
                    )))
                || (!self.provider.api_key_env.is_empty()
                    && self
                        .provider
                        .api_key_env
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_')),
            "Invalid API key environment name"
        );
        if self.provider.auth == ProviderAuth::Chatgpt {
            ensure!(
                self.provider.kind == ProviderKind::Openai
                    && self.provider.endpoint == "https://api.openai.com/v1/responses",
                "ChatGPT plan credentials are only sent to the public OpenAI Responses endpoint"
            );
            let path = self
                .provider
                .chatgpt_credentials_file
                .as_ref()
                .context("Choose this runtime's ChatGPT credential file after signing in")?;
            ensure!(
                path.is_absolute() && path.is_file(),
                "ChatGPT credentials must be an existing absolute private file"
            );
            ensure!(
                !std::fs::canonicalize(path)?.starts_with(&workspace),
                "ChatGPT credentials cannot be in the agent's workspace"
            );
        } else {
            ensure!(
                self.provider.chatgpt_credentials_file.is_none(),
                "ChatGPT credentials require auth=chatgpt"
            );
        }
        if self.provider.kind == ProviderKind::GithubCopilot {
            ensure!(
                cfg!(unix),
                "Copilot SDK bridge currently requires verified Unix process-tree cleanup"
            );
            ensure!(
                self.provider.endpoint.is_empty()
                    && matches!(
                        self.provider.auth,
                        ProviderAuth::Auto | ProviderAuth::Bearer
                    ),
                "Copilot uses its official SDK and an explicit GitHub credential, not a custom HTTP endpoint"
            );
            ensure!(
                self.provider.native.is_empty()
                    && self.provider.thinking_budget_tokens.is_none()
                    && self.provider.api_version.is_none()
                    && self.provider.tls_ca_file.is_none(),
                "Copilot bridge currently supports text and runtime function tools only"
            );
            let bridge = self
                .provider
                .copilot
                .as_ref()
                .context("Configure Copilot's Python and pinned SDK bridge script")?;
            for path in [&bridge.python, &bridge.script] {
                ensure!(
                    path.is_absolute() && path.is_file(),
                    "Copilot bridge paths must be existing absolute files"
                );
                ensure!(
                    !std::fs::canonicalize(path)?.starts_with(&workspace),
                    "Copilot bridge code must remain outside the agent workspace"
                );
            }
        } else {
            ensure!(
                self.provider.copilot.is_none(),
                "Copilot bridge settings require provider github-copilot"
            );
        }
        if !matches!(
            self.provider.kind,
            ProviderKind::Mock | ProviderKind::GithubCopilot
        ) {
            let url = reqwest::Url::parse(&self.provider.endpoint)
                .context("Set this provider's endpoint URL")?;
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "Provider URL cannot contain credentials or query parameters"
            );
            ensure!(
                url.scheme() == "https"
                    || (url.scheme() == "http"
                        && (self.provider.allow_insecure_http
                            || matches!(
                                url.host_str(),
                                Some("127.0.0.1" | "localhost" | "[::1]")
                            ))),
                "Provider must use HTTPS or loopback HTTP; private LAN HTTP requires explicit allow_insecure_http"
            );
        }
        if let Some(ca) = &self.provider.tls_ca_file {
            ensure!(
                ca.is_absolute() && ca.is_file(),
                "Provider CA must be an existing absolute file"
            );
        }
        ensure!(
            self.provider.reasoning_effort.is_none()
                || self.provider.thinking_budget_tokens.is_none(),
            "Choose reasoning_effort or thinking_budget_tokens, not both"
        );
        if let Some(e) = &self.provider.reasoning_effort {
            ensure!(
                ["none", "minimal", "low", "medium", "high", "xhigh", "max"].contains(&e.as_str()),
                "Invalid reasoning effort"
            );
        }
        if let Some(budget) = self.provider.thinking_budget_tokens {
            ensure!(
                matches!(
                    self.provider.kind,
                    ProviderKind::Anthropic | ProviderKind::Gemini
                ) && budget < self.limits.max_output_tokens,
                "Thinking budget is supported only for Anthropic/Gemini and must leave room for output"
            );
            ensure!(
                self.provider.kind != ProviderKind::Anthropic || budget >= 1024,
                "Anthropic thinking budget must be at least 1024 tokens"
            );
        }
        ensure!(
            self.provider.api_version.is_none() || self.provider.kind == ProviderKind::AzureOpenai,
            "api_version applies only to Azure OpenAI"
        );
        ensure!(
            self.provider.mock_response.is_none() || self.provider.kind == ProviderKind::Mock,
            "mock_response applies only to the mock provider"
        );
        ensure!(
            (1..=100).contains(&self.limits.max_model_calls)
                && (1..=300).contains(&self.limits.max_tool_calls),
            "Invalid turn budget"
        );
        ensure!(
            (256..=65536).contains(&self.limits.max_output_tokens),
            "Invalid output token limit"
        );
        ensure!(
            (4096..=8 * 1024 * 1024).contains(&self.limits.max_context_bytes),
            "Invalid context limit"
        );
        ensure!(
            (1..=3600).contains(&self.limits.turn_seconds)
                && (1..=300).contains(&self.limits.command_seconds),
            "Invalid time budget"
        );
        ensure!(
            (1024..=1024 * 1024).contains(&self.limits.tool_output_bytes)
                && (1..=16).contains(&self.limits.max_parallel_turns),
            "Invalid capacity"
        );
        Ok(())
    }
    pub fn cwd(&self, path: &str) -> Result<PathBuf> {
        ensure!(Path::new(path).is_absolute(), "ACP cwd must be absolute");
        let p = std::fs::canonicalize(path).context("Session working folder does not exist")?;
        ensure!(
            p.is_dir() && p.starts_with(std::fs::canonicalize(&self.workspace)?),
            "Session cwd is outside this agent's workspace"
        );
        Ok(p)
    }
    /// Build an effective root grant for this connection. A reconnect must
    /// provide its additional roots again; saved messages cannot restore them.
    pub fn for_session(&self, cwd: PathBuf, additional: &[PathBuf]) -> Result<Self> {
        let cwd = self.cwd(cwd.to_str().context("Session cwd must be UTF-8")?)?;
        ensure!(additional.len() <= 32, "Too many additional directories");
        let mut allowed = vec![std::fs::canonicalize(&self.workspace)?];
        for root in &self.additional_workspaces {
            allowed.push(std::fs::canonicalize(root)?);
        }
        let mut roots = vec![cwd.clone()];
        for root in additional {
            ensure!(
                root.is_absolute(),
                "Additional directories must be absolute"
            );
            let root =
                std::fs::canonicalize(root).context("Additional directory does not exist")?;
            ensure!(
                root.is_dir() && allowed.iter().any(|a| root.starts_with(a)),
                "Additional directory is not in the owner's workspace allowlist"
            );
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let mut config = self.clone();
        config.workspace = cwd;
        config.session_roots = Some(roots);
        Ok(config)
    }
    /// Resolve at use time and reject a root that was redirected since the
    /// client received its grant. File tools also use directory capabilities.
    pub fn effective_roots(&self) -> Result<Vec<PathBuf>> {
        match &self.session_roots {
            Some(roots) => roots
                .iter()
                .map(|root| {
                    let resolved = std::fs::canonicalize(root)?;
                    ensure!(
                        resolved == *root && resolved.is_dir(),
                        "A granted workspace root changed; reconnect to authorize it again"
                    );
                    Ok(resolved)
                })
                .collect(),
            None => Ok(vec![std::fs::canonicalize(&self.workspace)?]),
        }
    }
}
pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
