# Model providers

Provider selection belongs inside the native Rust runtime. ACP clients, Slack adapters, browser conversations, the Agent Instance host, and the fleet manager do not need provider-specific code. This implements the common text, streaming, and function-tool loop, not every optional feature of the Amplifier Python modules.

The accepted next design supports the latest verified model in each useful
provider class, with explicit model upgrades and preserved session bindings.
See the [model support policy](steering-design.md#model-support-policy-across-providers).
That policy and native OpenAI WebSocket steering are not implemented yet; the
reference below describes the released runtime.

## Available providers

| Preset | Protocol | Default credential variable | Endpoint |
| --- | --- | --- | --- |
| `openai` | OpenAI Responses | `OPENAI_API_KEY` | `https://api.openai.com/v1/responses` |
| `azure-openai` | Azure OpenAI Responses | `AZURE_OPENAI_API_KEY` | Supply the full resource's v1 Responses URL |
| `anthropic` | Claude Messages | `ANTHROPIC_API_KEY` | `https://api.anthropic.com/v1/messages` |
| `gemini` | Google GenAI streaming generateContent | `GOOGLE_API_KEY` | `https://generativelanguage.googleapis.com/v1beta` |
| `ollama` | Native Ollama streaming chat | `OLLAMA_API_KEY`, optional | `http://127.0.0.1:11434/api/chat` |
| `vllm` | vLLM Responses | `VLLM_API_KEY`, optional | `http://127.0.0.1:8000/v1/responses` |
| `chat-completions` | OpenAI-compatible streaming chat | `CHAT_COMPLETIONS_API_KEY`, optional | Supply the full `/v1/chat/completions` URL |
| `litellm` | Separately operated LiteLLM proxy's chat API | `LITELLM_API_KEY`, optional | `http://127.0.0.1:4000/v1/chat/completions` |
| `github-copilot` | Pinned official SDK bridge | `COPILOT_GITHUB_TOKEN` | SDK-managed; explicit token, Linux/macOS |
| `mock` | Deterministic local response | None | No network connection |

Choose a model that your account or server actually provides, with tool support when work requires tools. Azure's model is the **deployment name**. Except for OpenAI's existing default and mock, model selection is required. A compatible endpoint does not establish a particular model's reasoning or tool quality.

`agent-runtime providers` prints machine-readable presets without contacting providers. `init` and `configure-provider` share these options:

```text
--provider NAME --model MODEL
--endpoint URL --key-env ENVIRONMENT_VARIABLE
--auth auto|bearer|api-key|none|chatgpt
--chatgpt-credentials-file /absolute/private/credentials.json
--copilot-python /absolute/private/venv/bin/python
--copilot-bridge /absolute/private/provider.py
--reasoning-effort LEVEL | --thinking-budget-tokens NUMBER
--api-version VERSION                  (Azure only)
--tls-ca-file /absolute/path/to/ca.pem
--allow-insecure-http                  (explicit private LAN opt-in)
```

The JSON configuration uses `provider.kind`, `api_key_env`, `reasoning_effort`, `thinking_budget_tokens`, `api_version`, `tls_ca_file`, `allow_insecure_http`, and `auth` (`api_key` in JSON). The environment variable's name is saved; its value is not. `check` validates local configuration, not account access or model availability.

## Examples

Supply credentials privately in the process environment before connecting. Reconnect after changing configuration and start a new session for a different provider, model, or endpoint.

```sh
# Claude adaptive thinking; requires a model supporting this mode.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider anthropic --model claude-sonnet-5-5 --reasoning-effort medium

# Gemini; use --key-env GEMINI_API_KEY if that is your chosen variable.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider gemini --model gemini-3.8-flash

# Azure API key, deployment name, and full endpoint.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider azure-openai --model my-deployment \
  --endpoint https://my-resource.openai.azure.com/openai/v1/responses

# Azure with an externally supplied Entra access token instead of an API key.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider azure-openai --model my-deployment \
  --endpoint https://my-resource.openai.azure.com/openai/v1/responses \
  --auth bearer --key-env AZURE_OPENAI_AD_TOKEN

# Existing local installations; this does not download models or start servers.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider ollama --model your-installed-model --auth none
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider vllm --model your-served-model --auth none
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider chat-completions --model your-served-model \
  --endpoint http://127.0.0.1:1234/v1/chat/completions --auth none

# A proxy route already configured by the LiteLLM operator.
agent-runtime --home /absolute/path/to/agent configure-provider \
  --provider litellm --model my-proxy-route
```

OpenAI defaults to medium reasoning. Other providers send no reasoning override by default. Anthropic `--reasoning-effort` requests adaptive thinking. The current Claude classes reject the older explicit thinking-budget setting during local configuration validation. Gemini receives `thinkingLevel` or `thinkingBudget`; Ollama receives `think`; compatible chat APIs receive `reasoning_effort`. An unsupported model option fails visibly; the runtime does not silently change the model or account.

`--auth auto` uses the native key header for Anthropic, Gemini, and Azure, and bearer authentication for the other HTTP providers when a key is configured. `--auth none` never reads or sends an inherited key. Azure bearer tokens must be obtained and refreshed externally; this implementation does not run Azure CLI, managed identity, or the Azure SDK credential chain. Likewise, it does not perform Google OAuth token refresh.

HTTPS verifies certificates. A custom PEM CA can be added without disabling verification. Loopback HTTP works for local servers; other HTTP destinations require explicit opt-in. Redirects are disabled. There is no automatic provider/account fallback or blind request retry.

## Managed installations

The native-platform `scripts/install-instance.py` accepts the same provider, endpoint, reasoning, and authentication options. It validates settings before creating a host, pins a copy of the runtime under a version/hash path, and gives that instance only its selected model credential. Credentials are stored in the host's private secret store and passed to the engine as `AGENT_MODEL_API_KEY`. Keyless providers do not create a model secret.

For an existing managed installation, preserve `--key-env AGENT_MODEL_API_KEY` when running `configure-provider`. Update the host's `native-model-key` secret through its existing credentials interface if needed, and ensure the backend references that secret. Disconnect old ACP clients so newly launched runtime processes pick up the configuration. The optional [configuration contract](configuration.md) lets the host expose the same provider controls.

The agent keeps its instance identity, workspace, and saved history. An execution context retains its original provider/endpoint/model binding: loading history for inspection is allowed, but prompting it against a different binding is rejected before sending content. `configure-provider` also pins legacy 0.1 OpenAI transcripts before changing settings. Do not manually change a legacy destination before this migration. Switching back to the original configuration allows that context to continue.

## Validation and scope

Live checks used OpenAI `gpt-6.1-sol`, Anthropic `claude-sonnet-5-5` with adaptive thinking, and Gemini `gemini-3.8-flash`. Each wrote and read a file with native tools, passed an independent exact-byte check, restarted, and recalled a unique value from its saved session. These are bounded integration checks, not broad model-quality comparisons.

All eight HTTP presets have local wire-contract checks through real ACP subprocesses: fragmented streams, tool arguments/results, approval, cancellation, incomplete responses, native continuation, and restart. Mock and the installer have separate local checks. Azure, Ollama, vLLM, generic chat, and LiteLLM have **not** been checked against real deployments in this pass. Their tests do not establish availability on a user's server.

Version 0.5 adds [GitHub Copilot](copilot.md), including a live approved write/read and saved-history resume check with `gpt-6-luna`. Its SDK is pinned, runs in a temporary private directory, and never executes model-requested tools.

Version 0.3 additionally supports opt-in [native media, hosted tools, and structured output](native-capabilities.md), plus independent [decision services](decisions.md). Version 0.4 adds extractive [context compaction](context.md), configuration discovery, and a separately authorized [ChatGPT model catalog](chatgpt.md). Embeddings, discovery for other providers, and provider-specific cache controls remain outside this implementation. Continuation signatures and private reasoning fields stay in the private history; they are not displayed to ACP clients.

## Amplifier inventory

All 13 repositories matching `microsoft/amplifier-module-provider-*` were inspected at the revisions below on November 1, 2026. Revisions identify the source reviewed, not a runtime dependency. The implementation is native Rust and does not install these Python modules.

| Module | Reviewed source | Native runtime status |
| --- | --- | --- |
| `anthropic` | [af32c269886d](https://github.com/microsoft/amplifier-module-provider-anthropic/tree/af32c269886df2cf3f23f838d3f81ae2638cd6e9) | Implemented: text, streaming, tools |
| `azure-openai` | [e509318ae78d](https://github.com/microsoft/amplifier-module-provider-azure-openai/tree/e509318ae78d2d0561e5b5453cbd085c284c944d) | Implemented: text, streaming, tools |
| `chat-completions` | [486ea577c7ef](https://github.com/microsoft/amplifier-module-provider-chat-completions/tree/486ea577c7efd8596bd4ba9d42220ff6c48b5df9) | Implemented: text, streaming, tools |
| `gemini` | [39546071dca4](https://github.com/microsoft/amplifier-module-provider-gemini/tree/39546071dca40cc4c0114b3185f95d4b60a6c88f) | Implemented: text, streaming, tools |
| `github-copilot` | [ed942e2c8a0c](https://github.com/microsoft/amplifier-module-provider-github-copilot/tree/ed942e2c8a0c6d4ff6d878bc4ba480592f6c676a) | Implemented in 0.5: bounded official SDK bridge; Rust retains tool execution |
| `jev` | [7ccade1eed84](https://github.com/microsoft/amplifier-module-provider-jev/tree/7ccade1eed84b17f215bc2e80253a1fb4c3a2130) | Implemented as a separate decision service |
| `litellm` | [1908d0eb5f45](https://github.com/microsoft/amplifier-module-provider-litellm/tree/1908d0eb5f4560ca187c360c8618f75cbd55abb1) | Proxy mode only |
| `local-decisions` | [75d743fb2f7e](https://github.com/microsoft/amplifier-module-provider-local-decisions/tree/75d743fb2f7ea599dd92709625a8e0661dcf7ae8) | Implemented as a separate decision service |
| `mock` | [a90821f6b376](https://github.com/microsoft/amplifier-module-provider-mock/tree/a90821f6b3767f8e52df5e754a96c20352684b43) | Deterministic local response |
| `ollama` | [f86ae6c7bdf5](https://github.com/microsoft/amplifier-module-provider-ollama/tree/f86ae6c7bdf55745294cc1719b5df5dda4f84c2b) | Implemented: text, streaming, tools |
| `openai` | [38924e3b3db2](https://github.com/microsoft/amplifier-module-provider-openai/tree/38924e3b3db2ee0299e39a98de4ff832999b88a3) | Implemented: text, streaming, tools |
| `openai-chatgpt` | [b9394c218feb](https://github.com/microsoft/amplifier-module-provider-openai-chatgpt/tree/b9394c218feb18aac4bda8b9d356b646aa042d86) | Implemented separate public subscription OAuth; actual user consent and live entitlement pending |
| `vllm` | [985dcf464a2e](https://github.com/microsoft/amplifier-module-provider-vllm/tree/985dcf464a2ef917a3e56bc5ac0d82d39a19668d) | Implemented: text, streaming, tools |

The remaining modules have different boundaries:

- **GitHub Copilot** uses its SDK, authentication, sessions, and tool interception. Supporting it correctly needs a dedicated integration; an arbitrary bearer request to a guessed endpoint is not equivalent.
- **OpenAI ChatGPT** now uses the public OSS Sign in with ChatGPT flow, a separate registration and credential store, and the public Responses endpoint. Subscription login is not API-key access. No existing Codex credentials are reused. See [account setup](chatgpt.md).
- **JEV** and **local-decisions** provide bounded predicate/choice/score decisions, not conversational model/tool loops. Version 0.3 exposes them through a separate named decision capability, alongside OpenAI Decisions, Microsoft Decision One, and H2O Lightning; they do not appear in the chat model selector.
- **LiteLLM** in Amplifier invokes the Python SDK directly. This runtime supports its OpenAI-compatible proxy, which can route to the operator's configured providers. It does not embed the full Python SDK or claim all of its direct integrations.

Protocol references: [Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming), [Gemini function calling](https://ai.google.dev/gemini-api/docs/function-calling), [Ollama chat](https://docs.ollama.com/api/chat), [Azure Responses](https://learn.microsoft.com/en-us/azure/foundry/openai/how-to/responses), and the pinned module sources above.

## Current model selection

`management schema` exposes a dated model catalog with a concrete current model
for each useful class. OpenAI generation uses GPT-6 family models through Responses
WebSockets; it does not retry over HTTP or silently select an older model. Claude
uses Fable 5.1 and Opus, Sonnet, and Haiku 5.5 with adaptive thinking. The catalog
also lists Gemini 4 Argon, 3.8 Flash, and 3.5 Flash-Lite. Those Gemini entries are
documentation-verified choices, not claims of live account access. Specialist
image, audio, and decision configurations remain independent. Azure deployment
names, local servers, and gateways retain their own routing/capability checks.

Changing a catalog does not change a saved conversation's model binding. Select
a supported model explicitly when migrating an installation; provider changes
start a new context under the existing migration rules. The ChatGPT subscription
route omits unsupported output-token limits, retains the logical turn's time and
model-call limits, and uses only connection-local response chaining.

Sources: [OpenAI steering](https://developers.openai.com/api/docs/guides/steering),
[ChatGPT route limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations),
[Claude model overview](https://platform.claude.com/docs/en/models/overview),
[Gemini models](https://aistudio.google.com/docs/models).
