# GitHub Copilot model provider

Version 0.5 uses the official `github-copilot-sdk==1.0.15` for model inference on Linux and macOS. The Rust runtime still owns the agent loop, tools, approvals, saved conversation, context compaction, and cancellation. No Codex process or Codex account credentials are used.

The bridge starts a private, temporary SDK session for each model step. It supplies the saved normalized conversation and advertises only the Rust runtime's offered tools. SDK permissions and all SDK tool execution are denied. The first complete requested tool batch is captured, the SDK session is stopped, and Rust applies its normal mode and approval checks before executing anything. SDK-owned plugins, MCP servers, skills, config discovery, memory, and persistent session storage are disabled.

## Set up an installation

Keep the Python environment and bridge with this agent's own deployment files, outside its workspace. Python 3.11 or newer is required by the SDK. For example, from this repository:

```sh
python3 -m venv /absolute/private/my-agent-provider/venv
/absolute/private/my-agent-provider/venv/bin/python -m pip install \
  -r bridges/copilot/requirements.txt
cp bridges/copilot/provider.py /absolute/private/my-agent-provider/provider.py

agent-runtime --home /absolute/private/my-agent init \
  --workspace /absolute/private/my-agent-workspace --name "My agent" \
  --provider github-copilot --model gpt-6-luna \
  --copilot-python /absolute/private/my-agent-provider/venv/bin/python \
  --copilot-bridge /absolute/private/my-agent-provider/provider.py
```

Use `configure-provider` instead of `init` to update an existing instance, then reconnect and start a new session. The helper's exact SDK version is checked at runtime. The managed installer also accepts these two absolute paths; it does not implicitly install Python dependencies or reuse another agent's environment.

Provide `COPILOT_GITHUB_TOKEN` privately to the runtime process, or choose a different explicit environment reference with `--key-env`. The bridge receives only that selected credential. It does not fall back to a global GitHub CLI login. The official SDK supports an OAuth user token or a fine-grained token with the required Copilot permission; classic personal access tokens are unsupported. Account and organization Copilot policies still apply. See [GitHub's SDK authentication documentation](https://docs.github.com/en/copilot/how-tos/copilot-sdk/auth/authenticate).

`agent-runtime --home /absolute/private/my-agent copilot-models` returns the account's available model IDs and names. Catalog access and successful inference are separate checks. Choose a returned model suitable for the work; the name in the example is not a promise of availability to every account.

For a managed Agent Instance, the installer saves the selected credential in that instance's private secret store and configures the engine to read `AGENT_MODEL_API_KEY`. Keep that environment reference when editing its provider later.

## Boundaries and current limits

- The helper protocol is one bounded JSON request and response, limited to 8 MiB. The subprocess receives a clean environment and private temporary home. A deadline, cancellation, or normal completion ends its process group, including descendants.
- SDK model requests have network access to Copilot. SDK tool callbacks cannot grant that network access to commands: actual local command execution belongs to Rust's separately confined command tool.
- The bridge uses SDK streaming internally to capture tool requests, but buffers each model step before emitting ACP text. It does not yet expose incremental text to the user.
- No opaque SDK continuation is saved. Each model step receives normalized conversation history, with the runtime's archive-based compaction when needed. SDK token usage is best effort; an immediately captured tool request may not include final usage counters.
- Provider-native images, hosted web/code tools, structured-output options, and Windows bridge cleanup are not enabled. Unsupported configuration fails visibly. The independent typed decision services remain available.
- OAuth refresh/login UI is not implemented for this provider. The caller supplies a supported current credential through the explicit reference. This does not enroll a new application or silently consume another application's login.

Fixture tests exercise captured tool batches, denied SDK callbacks, unadvertised calls, real Rust approvals, saved-history resume, and descendant cancellation. A live check on Linux with the official SDK and `gpt-6-luna` wrote/read an approved workspace file, independently verified its exact bytes, restarted the Rust process, and recalled the saved unique value. This is a bounded integration check, not a general capability or model-quality claim.
