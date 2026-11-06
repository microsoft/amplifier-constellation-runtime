use agent_runtime::{
    config::{Config, Provider, ProviderAuth, ProviderKind, private_dir, private_write},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Independent Rust agent execution over standard ACP")]
struct Args {
    #[arg(long)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Classify continuation from trusted host observations; never executes work.
    Continuation {
        file: PathBuf,
    },
    Init {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long, default_value = "Native agent")]
        name: String,
        #[command(flatten)]
        provider: ProviderOptions,
    },
    /// List supported provider presets; does not connect to any accounts.
    Providers,
    /// JSON Schema and UI metadata for this exact engine version.
    ConfigSchema,
    /// Read settings and their revision without credential values.
    ConfigInspect,
    /// Validate a complete runtime.json without writing it or contacting models.
    ConfigValidate {
        file: PathBuf,
    },
    /// Atomically replace settings, rejecting concurrent edits.
    Configure {
        file: PathBuf,
        #[arg(long)]
        expected_revision: String,
    },
    /// Authorize this installation with the official ChatGPT plan usage flow.
    ChatgptSignIn {
        #[arg(long, default_value_t = 1455)]
        port: u16,
        #[arg(long)]
        no_browser: bool,
    },
    ChatgptStatus,
    ChatgptModels,
    /// Discover models using this runtime's explicit Copilot credential.
    CopilotModels,
    ChatgptSignOut,
    /// Import this tool's protected registration onto a remote host over SSH.
    ChatgptImport {
        file: PathBuf,
    },
    /// List independent decision services (no connection or download).
    DecisionProviders,
    /// Install or replace a named decision backend from JSON.
    ConfigureDecision {
        file: PathBuf,
    },
    /// Evaluate a typed request from JSON, independently of chat.
    Decide {
        backend: String,
        file: PathBuf,
    },
    /// Assess attention, approach and coordination from source-labelled evidence.
    Assess {
        backend: String,
        file: PathBuf,
        /// Print the typed questions without calling a provider.
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage bounded agent-authored attention watches; provider calls are separate.
    Watch {
        #[arg(long)]
        store: PathBuf,
        file: PathBuf,
    },
    /// Inspect configured native tools, modalities, and decision services.
    Capabilities,
    /// Configure provider-native capabilities from JSON.
    ConfigureCapabilities {
        file: PathBuf,
    },
    /// Change the provider for new sessions. Existing histories stay intact.
    ConfigureProvider {
        #[command(flatten)]
        provider: ProviderOptions,
    },
    Acp {
        /// Trust this local ACP host to launch its supplied MCP programs as you.
        /// Do not use for an untrusted network adapter.
        #[arg(long)]
        trusted_local_client: bool,
    },
    Check,
    Sessions,
    /// Content-free model/context/cache/latency observations from the local history.
    Diagnostics,
    /// Read bounded public history; the caller enforces conversation audience grants.
    HistoryQuery {
        file: PathBuf,
    },
    /// Explicit OS-owner import of public text; never credentials, tools, or authority.
    HistoryImport {
        file: PathBuf,
    },
    Events {
        session: String,
    },
}
#[derive(clap::Args)]
struct ProviderOptions {
    #[arg(long, value_enum, default_value = "openai")]
    provider: ProviderKind,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    key_env: Option<String>,
    #[arg(long, value_enum, default_value = "auto")]
    auth: ProviderAuth,
    #[arg(long)]
    chatgpt_credentials_file: Option<PathBuf>,
    #[arg(long, requires = "copilot_bridge")]
    copilot_python: Option<PathBuf>,
    #[arg(long, requires = "copilot_python")]
    copilot_bridge: Option<PathBuf>,
    #[arg(long)]
    reasoning_effort: Option<String>,
    #[arg(long, conflicts_with = "reasoning_effort")]
    thinking_budget_tokens: Option<usize>,
    #[arg(long)]
    api_version: Option<String>,
    #[arg(long)]
    tls_ca_file: Option<PathBuf>,
    #[arg(long)]
    allow_insecure_http: bool,
}
impl ProviderOptions {
    fn build(self) -> Result<Provider> {
        let model = self
            .model
            .or_else(|| match self.provider {
                ProviderKind::Openai => Some("gpt-6.1-sol".into()),
                ProviderKind::Mock => Some("mock".into()),
                _ => None,
            })
            .context(
                "Choose --model from your provider's available models (Azure: deployment name)",
            )?;
        let mut p = Provider::preset(self.provider, model);
        if let Some(v) = self.endpoint {
            p.endpoint = v;
        }
        if let Some(v) = self.key_env {
            p.api_key_env = v;
        }
        if self.reasoning_effort.is_some() {
            p.reasoning_effort = self.reasoning_effort;
        }
        if self.thinking_budget_tokens.is_some() {
            p.reasoning_effort = None;
        }
        p.thinking_budget_tokens = self.thinking_budget_tokens;
        p.auth = self.auth;
        p.chatgpt_credentials_file = self.chatgpt_credentials_file;
        p.copilot = self
            .copilot_python
            .zip(self.copilot_bridge)
            .map(|(python, script)| agent_runtime::copilot::Bridge { python, script });
        p.api_version = self.api_version;
        p.tls_ca_file = self.tls_ca_file;
        p.allow_insecure_http = self.allow_insecure_http;
        Ok(p)
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Command::Continuation { file } = &args.command {
        ensure!(
            std::fs::metadata(file)?.len() <= 16384,
            "Continuation input too large"
        );
        let input = serde_json::from_slice(&std::fs::read(file)?)?;
        println!("{}", agent_runtime::continuation::assess(input)?);
        return Ok(());
    }
    if let Command::Watch { store, file } = &args.command {
        println!("{}", agent_runtime::watches::from_file(store, file)?);
        return Ok(());
    }
    if matches!(args.command, Command::ConfigSchema) {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_runtime::management::schema())?
        );
        return Ok(());
    }
    if matches!(args.command, Command::Providers) {
        use clap::ValueEnum;
        let rows:Vec<_>=ProviderKind::value_variants().iter().map(|kind|{let p=Provider::preset(*kind,String::new());serde_json::json!({"provider":kind.id(),"endpoint":p.endpoint,"key_env":p.api_key_env,"model_required":!matches!(kind,ProviderKind::Openai|ProviderKind::Mock),"notes":match kind {ProviderKind::Litellm=>"Requires a separately operated LiteLLM OpenAI-compatible proxy",ProviderKind::AzureOpenai=>"Use the v1 Responses URL and deployment name; API key or explicit Entra bearer token",ProviderKind::Mock=>"Deterministic local fixture, not model inference",ProviderKind::GithubCopilot=>"Official SDK bridge on Unix; explicit supported GitHub token and pinned Python SDK required",_=>"Text and tool calls; provider-specific reasoning is optional"}})}).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if matches!(args.command, Command::DecisionProviders) {
        use clap::ValueEnum;
        let rows: Vec<_> = agent_runtime::decisions::Kind::value_variants()
            .iter()
            .map(|k| agent_runtime::decisions::Backend::preset(*k, format!("{k:?}").to_lowercase()))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let home = args
        .home
        .context("Supply --home for this agent installation")?;
    if let Command::Init {
        workspace,
        name,
        provider,
    } = args.command
    {
        ensure!(
            home.is_absolute() && workspace.is_absolute(),
            "Use absolute home and workspace folders"
        );
        private_dir(&home)?;
        private_dir(&workspace)?;
        let mut config = Config::initial(workspace, name, String::new());
        config.provider = provider.build()?;
        config.validate(&home)?;
        private_write(
            &home.join("runtime.json"),
            &serde_json::to_vec_pretty(&config)?,
        )?;
        Store::open(home)?;
        println!(
            "Runtime initialized for {}. Credential environment: {}. New ACP connections use this configuration.",
            config.provider.kind.id(),
            if config.provider.api_key_env.is_empty() {
                "none"
            } else {
                &config.provider.api_key_env
            }
        );
        return Ok(());
    }
    match &args.command {
        Command::ChatgptSignIn { port, no_browser } => {
            let result = tokio::select! { r=agent_runtime::chatgpt::sign_in(&home,*port,*no_browser)=>r?, _=tokio::signal::ctrl_c()=>anyhow::bail!("Sign-in cancelled") };
            println!("{}", result);
            return Ok(());
        }
        Command::ChatgptStatus => {
            println!("{}", agent_runtime::chatgpt::status(&home)?);
            return Ok(());
        }
        Command::ChatgptModels => {
            println!("{}", agent_runtime::chatgpt::models(&home).await?);
            return Ok(());
        }
        Command::ChatgptSignOut => {
            println!("{}", agent_runtime::chatgpt::sign_out(&home).await?);
            return Ok(());
        }
        Command::ChatgptImport { file } => {
            println!("{}", agent_runtime::chatgpt::import(&home, file).await?);
            return Ok(());
        }
        Command::ConfigInspect => {
            println!(
                "{}",
                serde_json::to_string_pretty(&agent_runtime::management::inspect(&home)?)?
            );
            return Ok(());
        }
        Command::ConfigValidate { file } => {
            agent_runtime::management::load_candidate(&home, file)?;
            println!(
                "{}",
                serde_json::json!({"valid":true,"authentication_verified":false})
            );
            return Ok(());
        }
        Command::Configure {
            file,
            expected_revision,
        } => {
            let c = agent_runtime::management::load_candidate(&home, file)?;
            println!(
                "{}",
                agent_runtime::management::configure(&home, &c, expected_revision)?
            );
            return Ok(());
        }
        _ => {}
    }
    let inspected = agent_runtime::management::inspect(&home)?;
    let mut config: Config = serde_json::from_value(inspected["config"].clone())?;
    if let Command::ConfigureProvider { provider } = args.command {
        config.provider = provider.build()?;
        println!(
            "{}",
            agent_runtime::management::configure(
                &home,
                &config,
                inspected["revision"].as_str().unwrap()
            )?
        );
        return Ok(());
    }
    match &args.command {
        Command::ConfigureDecision { file } => {
            let b: agent_runtime::decisions::Backend =
                serde_json::from_slice(&std::fs::read(file)?)?;
            config.decisions.retain(|old| old.name != b.name);
            config.decisions.push(b);
        }
        Command::ConfigureCapabilities { file } => {
            config.provider.native = serde_json::from_slice(&std::fs::read(file)?)?;
        }
        _ => {}
    }
    if matches!(
        args.command,
        Command::ConfigureDecision { .. } | Command::ConfigureCapabilities { .. }
    ) {
        println!(
            "{}",
            agent_runtime::management::configure(
                &home,
                &config,
                inspected["revision"].as_str().unwrap()
            )?
        );
        return Ok(());
    }
    // Inspection stays available to repair obsolete settings; execution and
    // readiness still require a valid configuration after any edit path.
    config.validate(&home)?;
    if let Command::Decide { backend, file } | Command::Assess { backend, file, .. } = &args.command
    {
        let b = config
            .decisions
            .iter()
            .find(|b| b.name == *backend)
            .context("Decision backend is not configured")?;
        ensure!(
            std::fs::metadata(file)?.len() <= 256 * 1024,
            "Decision input too large"
        );
        let bytes = std::fs::read(file)?;
        let req = if matches!(args.command, Command::Assess { .. }) {
            agent_runtime::assessment::request(serde_json::from_slice(&bytes)?)?
        } else {
            serde_json::from_slice(&bytes)?
        };
        if matches!(args.command, Command::Assess { dry_run: true, .. }) {
            println!("{}", serde_json::to_string_pretty(&req)?);
            return Ok(());
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = tokio::select! {r=agent_runtime::decisions::evaluate(b,&req,&cancel)=>r?,_=tokio::signal::ctrl_c()=>{cancel.cancel();anyhow::bail!("Cancelled");}};
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if matches!(args.command, Command::Capabilities) {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_runtime::capabilities::report(&config))?
        );
        return Ok(());
    }
    if matches!(args.command, Command::CopilotModels) {
        ensure!(
            config.provider.kind == ProviderKind::GithubCopilot,
            "Configure github-copilot first"
        );
        let key = std::env::var(&config.provider.api_key_env)
            .context("Set the configured Copilot credential privately")?;
        let cancel = tokio_util::sync::CancellationToken::new();
        println!(
            "{}",
            agent_runtime::copilot::invoke(
                &config,
                &key,
                serde_json::json!({"operation":"models"}),
                &cancel
            )
            .await?
        );
        return Ok(());
    }
    let store = Store::open(home)?;
    match args.command {
        Command::Acp {
            trusted_local_client,
        } => {
            if trusted_local_client {
                config.allow_client_stdio_mcp = true;
            }
            agent_runtime::server::serve(config, store).await?
        }
        Command::Check => println!(
            "{}",
            serde_json::json!({"valid":true,"engine":"native-rust","provider":config.provider.kind,"model":config.provider.model,"authentication_method":config.provider.auth,"api_key_present":config.provider.auth!=ProviderAuth::Chatgpt && std::env::var(&config.provider.api_key_env).is_ok_and(|v|!v.trim().is_empty()),"chatgpt_account_configured":config.provider.chatgpt_credentials_file.as_ref().is_some_and(|p|p.is_file()),"authentication_verified":false,"local_commands_available":agent_runtime::tools::command_isolation().is_some(),"local_command_isolation":agent_runtime::tools::command_isolation(),"local_command_network":config.command_network,"mode":config.mode,"version":env!("CARGO_PKG_VERSION")})
        ),
        Command::Diagnostics => println!("{}", agent_runtime::observability::inspect(&store)?),
        Command::Sessions => println!("{}", store.list(None, None)?),
        Command::HistoryQuery { file } => {
            println!("{}", agent_runtime::history::from_file(&store, &file)?)
        }
        Command::HistoryImport { file } => {
            println!(
                "{}",
                agent_runtime::migrate::from_file(&store, &config, &file)?
            )
        }
        Command::Events { session } => println!("{}", store.events(&session)?),
        Command::Init { .. }
        | Command::ConfigSchema
        | Command::ConfigInspect
        | Command::ConfigValidate { .. }
        | Command::Configure { .. }
        | Command::ChatgptSignIn { .. }
        | Command::ChatgptStatus
        | Command::ChatgptModels
        | Command::CopilotModels
        | Command::ChatgptSignOut
        | Command::ChatgptImport { .. }
        | Command::ConfigureProvider { .. }
        | Command::Providers
        | Command::DecisionProviders
        | Command::ConfigureDecision { .. }
        | Command::Decide { .. }
        | Command::Assess { .. }
        | Command::Watch { .. }
        | Command::Continuation { .. }
        | Command::Capabilities
        | Command::ConfigureCapabilities { .. } => {
            unreachable!()
        }
    }
    Ok(())
}
