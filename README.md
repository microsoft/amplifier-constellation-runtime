# Agent Runtime

Part of [Amplifier Constellation](https://github.com/microsoft/amplifier-constellation).

A standalone Rust agent with its own model/tool loop, durable sessions, and an ACP interface. It calls OpenAI, Azure OpenAI, Anthropic, Gemini, Ollama, vLLM, and compatible model servers directly. It does not launch Codex or share another agent's history.

Each installation has a private home, explicit workspace, model credential reference, and execution limits. Connect an ACP client directly over stdio, or use the independent [Agent Instance](https://github.com/microsoft/amplifier-constellation-instance) service for remote access, management, profiles, conversation sources, and durable background responsibilities. Experiences and adapters remain separate projects.

Start with [How the runtime works](docs/runtime-overview.md) for a visual overview
of the current design and the responsibilities of Runtime, Instance and adapters.

## Start one agent

Build with Rust 1.88 or later:

```sh
cargo build --release --locked
./target/release/agent-runtime --home "$PWD/instance" init \
  --workspace "$PWD/workspace" --name "My agent" --model gpt-6.1-sol
```

Supply `OPENAI_API_KEY` privately in the process environment. The configuration stores the variable's name, never its value. The model must be available to that account. Then configure an ACP client to run:

```sh
/absolute/path/to/agent-runtime --home /absolute/path/to/instance acp
```

Use the configured workspace as the client's session working folder. On Windows, the binary is `agent-runtime.exe`. Configuration is in `instance/runtime.json`; `check`, `sessions`, and `events SESSION_ID` are available alongside `acp`. The home must not be inside the exposed workspace.

### Choose a provider

`agent-runtime providers` lists the ten supported presets. `init` and `configure-provider` accept the same provider options:

```sh
agent-runtime --home /absolute/path/to/instance configure-provider \
  --provider anthropic --model claude-sonnet-5-5 --reasoning-effort medium
```

Supply `ANTHROPIC_API_KEY` privately, then reconnect and create a new session. Existing histories remain available for inspection; continuing them requires their original provider, endpoint, and model. Changing an environment variable's credential does not invalidate a session. [Provider setup and compatibility](docs/providers.md) covers all presets, endpoints, authentication, local servers, and the Amplifier module inventory.

### Optional managed installation on Linux, macOS, or Windows

With both repositories checked out, an Agent Instance binary built, and the model key in the environment:

```sh
python3 scripts/install-instance.py \
  --host-repo /absolute/path/to/agent-instance \
  --host-binary /absolute/path/to/agent-instance/target/release/agent-instance \
  --runtime-binary /absolute/path/to/agent-runtime/target/release/agent-runtime \
  --home "$HOME/.local/share/agent-instance/my-native-agent" \
  --name "My native agent"
```

On Windows use `python` and `.exe` binary paths. The installer uses systemd, LaunchAgent, or a user scheduled task according to the platform; [platform setup and limits](docs/platforms.md) describes the prerequisites.

This creates a new installation, copies the engine, stores a private credential reference, and starts an independent user service. It refuses an existing installation. Existing agents and Codex histories are not converted. The host's normal certificate, authentication, and remote-access setup applies. The installer also accepts `--provider`, `--model`, `--endpoint`, `--key-env`, and the runtime's reasoning/authentication options. Keyless local models and mock installations need no credential. Inspect its `--help` for URL and mode settings.

### Configure through a UI or automation

`config-schema`, `config-inspect`, `config-validate`, and revision-checked `configure` expose the same complete configuration to the CLI, Agent Instance, and an external management UI. [Configuration contract](docs/configuration.md) covers updates, backups, and restart behavior.

Owners using a dedicated installation container can enable [container-local administrator commands](docs/container-commands.md) for package installation while keeping host access outside that container.

[GitHub Copilot](docs/copilot.md) uses the pinned official SDK for model inference while this Rust runtime retains tool execution, permissions, and saved history.

[Continue with ChatGPT](docs/chatgpt.md) supports a separate subscription authorization and account model catalog, with explicit user consent. It never imports Codex credentials.

## Optional native capabilities

Enable [image generation, media input/output, hosted tools, and structured output](docs/native-capabilities.md) with `configure-capabilities`. Configure [JEV, Microsoft Decision One, OpenAI Decisions, H2O Lightning, and local decision services](docs/decisions.md) independently with `configure-decision`. Both remain opt-in and expose their configuration through `capabilities`.

## What works

- ACP v1 initialization, new/load/list/resume/close/delete sessions, prompt streaming, cancellation, mode/config-option changes, tool events, and permission requests.
- Resource links and embedded resources in prompts, with original blocks preserved for replay. References do not automatically fetch URLs or open files; supplied binary resources are retained but not claimed to be decoded by a text model.
- SQLite history with process-safe session ownership; reconnecting clients replay conversation and tool history.
- Native model/tool iteration with bounded time, calls, output, context, and concurrency.
- Provider-native [context compaction](docs/context.md), stable cache prefixes, provenance checkpoints, and read-only original-message retrieval; full history remains saved. Other providers retain an extractive fallback.
- Workspace file tools on Linux, macOS, and Windows. Linux offers bounded commands through bubblewrap; macOS uses a default-deny Seatbelt profile; Windows uses an AppContainer without network capabilities and a Job Object for its process tree. None pass provider credentials or the host home to commands.
- MCP tools over Streamable HTTP, with optional trusted-client stdio support. Per-connection credentials are reattached on resume and are never saved in session history.
- Three session modes: read-only, ask before changes, and workspace access. External mutations still require approval unless explicitly listed in configuration.
- Durable tool intent before execution. Interrupted effects are reported as uncertain and are never automatically retried.

## Deliberate limits

Version 0.4 adds configuration discovery/editing, archive-based context compaction, explicit ChatGPT subscription sign-in, native-platform installation, and confined macOS commands. API/provider capabilities remain opt-in and account/model dependent. ChatGPT's actual consent and entitlement must be verified with a newly authorized account. Version 0.5 adds the official Copilot SDK bridge on Linux and macOS. It currently buffers each model step and has no provider-native media/tool support. Embeddings remain unfinished. OpenAI caching is automatic; Anthropic caching and capability-checked native compaction are configurable. There is no built-in multi-agent delegation or mobile UI; connect additional capabilities through explicit MCP servers and separate experiences. The engine does not expose client filesystem/terminal callbacks; file tools belong to this agent's workspace.

Version 0.6 adds native Windows commands with verified file, network, environment, cancellation, and permission-cleanup boundaries. Windows commands require a local workspace without reparse points or multiply linked files; executables must be in that workspace or the Windows system directory. See [platform scope](docs/platforms.md) for limits. No platform falls back to unrestricted execution. macOS depends on the deprecated but currently available `sandbox-exec` helper and intentionally denies additional host tools that need broader access.

An ACP connection owns active foreground execution. Disconnecting cancels it. The separate Agent Instance scheduler can own explicitly authorized background work across browser disconnection. Scheduler reports still require independent verification; producing a report is not proof that a responsibility is complete.

Client-supplied local MCP programs are disabled by default. For a trusted local ACP host, launch `agent-runtime --home /absolute/instance acp --trusted-local-client`. This enables standard stdio MCP for that process without changing saved settings. Setting `allow_client_stdio_mcp: true` enables it persistently. Either choice gives that ACP client authority to start host programs, beyond workspace tools and session modes. Normal remote installations keep this restricted. Because v1 mandates stdio MCP, the restricted default is a policy-limited profile, not a claim of full v1 conformance. The trusted-local profile is covered by the upstream TCK; see [validation](docs/validation.md).

The optional [native history query contract](docs/context.md#native-history-source) lets an authorized instance context layer retrieve this engine's own public conversations without exporting and reimporting transcripts. It does not expose a new public network endpoint or change ordinary ACP interoperability.

The runtime does not infer permission from a persona. MCP annotations alone do not grant permission: `trusted_mcp_readonly_urls` names exact HTTP endpoints whose read-only declarations are trusted. Other MCP calls require approval, and read-only mode denies them. Optional `allowed_mcp_mutations` entries use the exact HTTP endpoint plus `#tool_name`, never a client-selected server label. Source access is enforced by each provider's credential and audience policy. A cancelled remote mutation may still finish remotely; inspect its outcome before retrying.

## Check it

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/protocol.py
python3 tests/providers.py
python3 tests/capabilities.py
python3 tests/install.py
python3 tests/management.py
python3 tests/copilot_protocol.py
```

The Python tests use local model/MCP fixtures and real ACP subprocesses; no paid account is needed. `tests/official-sdk.mjs` checks an existing, exercised session with the official ACP client SDK. See [architecture](docs/architecture.md) and [validation](docs/validation.md) for scope and results.

## Contributing

> [!NOTE]
> This project is not currently accepting external contributions, but we're actively working toward opening this up. We value community input and look forward to collaborating in the future. For now, feel free to fork and experiment!

Most contributions require you to agree to a
Contributor License Agreement (CLA) declaring that you have the right to, and actually do, grant us
the rights to use your contribution. For details, visit [Contributor License Agreements](https://cla.opensource.microsoft.com).

When you submit a pull request, a CLA bot will automatically determine whether you need to provide
a CLA and decorate the PR appropriately (e.g., status check, comment). Simply follow the instructions
provided by the bot. You will only need to do this once across all repos using our CLA.

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
For more information see the [Code of Conduct FAQ](https://opensource.microsoft.com/codeofconduct/faq/) or
contact [opencode@microsoft.com](mailto:opencode@microsoft.com) with any additional questions or comments.

## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.

See [bounded continuation advice](docs/continuation.md) for receipt-based recovery and remaining-budget checks, and [attention watches](docs/watches.md) for current-state and exact-predicate contracts. Both are optional host helpers; standard ACP agents remain independent.

### Inspecting and sharing local images

`workspace_view_image` reads a PNG/JPEG/GIF/WebP inside the session's granted
workspace and gives the model visual evidence. `workspace_attach_image` explicitly
attaches that saved image to the current ACP conversation (maximum 2 MiB). It
follows the session's write/approval policy; viewing an image does not publish it.
The ACP client decides how attachments are displayed or delivered, such as a
Slack file upload. The runtime does not choose another destination or fetch a
remote URL when attaching.

Standard MCP image results are retained as attributed visual tool evidence and
replayed in tool cards. Images are passed through the existing provider image
codecs, without embedding base64 in truncated text. Tool observations never
become outgoing attachments automatically; tool-provided private runtime fields
cannot request publication. Reloading a session shows saved tool images without
rerunning a capture or uploading an attachment again.

Optional MCP integrations can set
`_meta["io.github.bkrabach.agent-runtime"].optional: true` on their ACP MCP
descriptor. Failed optional discovery is bounded to five seconds and leaves other
connections usable, with a redacted degraded-connection report in the agent's
instructions, `runtime_capabilities`, and turn observations. Server aliases retain
the original descriptor index. Required connections remain required. Reload a
session to retry discovery; uncertain tool actions are never automatically replayed.
This is an optional namespaced extension, not a requirement of standard ACP.

### Completed public commentary

Clients may opt in to `com.microsoft.amplifier.message.completion` version 1 through ACP capability metadata. For OpenAI Responses streams, a completed assistant output item now emits a bounded text-completion receipt. This lets a conversation adapter post a whole public contribution while later work continues. Private reasoning is never included. The receipt is emitted only when the provider’s completed snapshot matches the streamed text; it does not claim task completion.

Ordinary ACP clients continue to receive the same text chunks and final prompt result. Other providers retain their existing output behavior; adapters must not infer completion from pauses or partial text. See the [conversation delivery contract](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/conversation-delivery.v1-candidate.md).
