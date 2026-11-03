# Native model capabilities

Version 0.3 adds provider-hosted tools, inline media, structured output, and independent decision services. The model/tool loop remains native Rust. ACP carries ordinary image, audio, resource, text, and tool events; Slack, browser, and editor rendering remain client responsibilities.

Capabilities are configured for an installation and its selected model. They are not inferred from a model name. An unsupported combination produces a provider error; it does not silently switch models, accounts, or output formats.

## Configure an installation

```sh
agent-runtime --home /absolute/path/to/agent configure-capabilities examples/native/openai.json
agent-runtime --home /absolute/path/to/agent capabilities
```

The first command replaces `provider.native` in `runtime.json`. Reconnect ACP clients to load the change. Use `{}` to disable optional native capabilities. `runtime_capabilities` exposes the same configuration to the agent. This is configuration inspection, not a model-entitlement probe. Changing the chat provider resets native settings; configure that provider's capabilities afterward.

| Provider protocol | Optional native capabilities |
| --- | --- |
| OpenAI / Azure Responses | Image input, image generation, web search, file search, code interpreter, JSON schema output |
| Anthropic | Image input, web search, web fetch, hosted code execution, JSON schema output, server-tool continuation and container reuse |
| Gemini | Image/audio input, image/audio output, Google Search, URL context, code execution, Maps grounding, file search, JSON schema output |
| Ollama / vLLM / Chat Completions / LiteLLM proxy | Image input and structured output when the served model/protocol supports them; ordinary local and MCP tools remain available |

The tools in `provider.native.tools` are the provider's native tool declarations. Supported tool names are allowlisted, while their provider-specific options are preserved. For example, file search requires the operator's existing vector-store IDs; the runtime does not create or manage an index. Azure support depends on its deployment and region. Versioned Anthropic tools and optional `anthropic_betas` must match the chosen model. The example files are configurations to adapt, not a claim that every tool combination works with every account.

For Gemini image/audio generation, use a model that supports the modality and `disable_function_tools: true`. This explicitly omits local/MCP/decision function tools for that connection; it does not temporarily pretend the image model can run tools. `image_config` and `speech_config` map to the corresponding Gemini generation settings. A different connection can use a general tool-calling model. Other protocols reject Gemini-only settings.

`json_schema` maps to each provider's native structured-output request. The provider must support the schema and model combination. The runtime preserves the returned text; applications that depend on its structure should still parse and validate it.

## Media, files, and continuity

ACP initialization advertises only enabled image/audio input capabilities. Prompts accept inline base64 media, never a model-selected remote URL. Image input supports PNG, JPEG, WebP, and GIF; audio input is currently Gemini-only. Unsupported prompt types fail before making a model request.

Generated media is saved in the private session history and emitted as standard ACP content. Loading the session replays it. Gemini PCM audio fragments become one playable WAV recording while the original provider parts and signatures remain intact. Hosted execution and citations become ordinary tool updates and source links. Private reasoning/signatures remain in private continuation data, not displayed client output.

OpenAI image generation uses `store: false`. Continuing or editing an image therefore sends the retained image bytes as explicitly labeled prior output, rather than referring to an expired provider call ID. `workspace_save_artifact` saves a returned image/audio/file by its artifact ID or `latest`, with the normal workspace write permission check. No model has to copy a large base64 string into tool arguments.

The runtime retrieves up to four generated-file citations from a completed answer automatically, so a file created by a hosted tool becomes an actual ACP attachment even when its identifiers arrive after the final model response. Retrieval/search citations are not automatically downloaded. `provider_file` also retrieves a provider-generated file cited in the current session on demand. It derives the download endpoint from the configured API origin, requires a matching file/container reference, and returns the bytes through ACP. It cannot fetch arbitrary URLs or enumerate other provider files. Generated binary resources are downloads, not executable HTML. The browser chat and Observatory clients render images, play supported audio, and offer downloads; other clients may impose smaller limits or only display an attachment.

Limits are explicit: 16 MiB per base64 media part, four prompt media parts, 20 MiB per prompt, 8 MiB per provider file, 24 MiB per ACP frame, and 48 MiB of stored media/history per session. A host or adapter can impose a lower limit; configure its transport accordingly. Media counts separately from text for context budgeting. Version 0.4 automatically compacts older text turns with [archive provenance and original-history retrieval](context.md). That does not delete saved transcripts or media. The separate retained-media/session storage ceiling still asks for a new focused session when reached.

Provider-hosted tools execute in the provider's environment and may incur separate costs. Enabling them is an installation-level grant; ACP workspace modes continue to govern local file/command/MCP actions, not the provider's hosted interpreter or searches. The runtime does not convert decision scores or personas into authorization.

## Verified scope

Real integration checks completed OpenAI image generation, image editing after reconnect, a permissioned exact-byte image save, web search, and hosted code execution with automatic file download and exact-byte verification; Gemini image generation and replay; and Anthropic web search plus hosted code execution, citations, and replay. The remaining tool combinations use protocol fixtures or require a configured provider resource. Fixtures cover all eight HTTP protocols' vision/schema request formats, Azure media transport, file-reference isolation, private continuation fields, and rejection/cancellation paths. The browser clients separately verify image rendering and persistence with real ACP fixture processes.

Not implemented: native computer control for non-Responses protocols, hosted remote-MCP approval bridging, realtime duplex audio/video, video-generation jobs, embeddings/index administration, non-PDF binary prompt ingestion, automatic model capability discovery, or provider cache management. These have distinct execution or data lifecycles and are not advertised as supported through a generic JSON pass-through.

References: [OpenAI tools](https://developers.openai.com/api/docs/guides/tools), [OpenAI image generation](https://developers.openai.com/api/docs/guides/tools-image-generation), [Anthropic server tools](https://platform.claude.com/docs/en/agents-and-tools/tool-use/server-tools), [Anthropic code execution](https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool), [Gemini image generation](https://ai.google.dev/gemini-api/docs/generate-content/image-generation), and [Gemini generateContent](https://ai.google.dev/api/generate-content).

## PDF input

Enable `provider.native.pdf_input` for a model that accepts PDFs. A client sends
a standard ACP `resource` block with an absolute descriptive URI,
`mimeType: "application/pdf"` and base64 `blob`. The URI is not fetched and never
becomes a host filesystem path. Original bytes are kept for session replay.

The runtime converts the file to Responses `input_file` for OpenAI/Azure,
Anthropic `document`, Gemini `inlineData`, or Chat Completions/LiteLLM `file`.
Unsupported protocols, disabled input, malformed base64 and missing PDF headers
fail before a provider call. Limits remain four media parts, 16 MiB base64 per
part and 20 MiB per complete prompt. Provider page/context limits still apply;
this is a bounded transport, not a PDF sanitizer or a guarantee of model fidelity.

See the provider contracts: [OpenAI file inputs](https://developers.openai.com/api/docs/guides/file-inputs),
[Anthropic PDF support](https://platform.claude.com/docs/en/build-with-claude/pdf-support),
and [Gemini document processing](https://ai.google.dev/gemini-api/docs/document-processing).


## Native computer/browser control

Responses models can use the native `computer` tool when `provider.native.computer`
binds it to a session's explicitly connected MCP server and tool. The
[isolated browser example](../examples/computer/README.md) provides the environment;
the Rust runtime owns action validation, approval, budgets, persistence and the
model loop. It does not control the host desktop or reuse a person's browser.

Every batch, including screenshots, requires a single-use ACP permission. The
permission shows the actions and any provider safety checks. Workspace mode and
MCP read-only hints do not skip it; read-only sessions deny computer calls.
Safety checks are acknowledged to the provider only after approval. The MCP tool
is withheld from ordinary function tools, so the model cannot bypass this path.
A screenshot must be one bounded PNG matching the configured display dimensions.

A failed or interrupted batch has an uncertain outcome and stops the turn. The
runtime preserves the original record, reports the uncertainty, and never
replays it on reconnect. Historical incomplete calls are removed only from the
provider continuation payload, with an explicit interruption note. Original
private history remains intact. Do not treat successful delivery of a batch as
proof of task completion; inspect the screenshot/application result.

This implementation uses the current Responses `computer_call.actions` and
`computer_call_output` contract. OpenAI was checked live against an isolated
local web page: screenshot, approved click, and returned verification code.
Azure uses the same wire format but still needs a compatible deployment. Other
providers' native computer formats are not advertised by this setting.

The live PDF checks separately read a fresh code from an actual PDF and recalled
it after a process restart on OpenAI, Anthropic, and Gemini. The six protocol
fixtures cover wire conversion and original ACP resource replay; those checks do
not establish document interpretation quality for every model or file.

Reference: [OpenAI computer use](https://developers.openai.com/api/docs/guides/tools-computer-use).
