# Native ACP capabilities and authority

The production wire profile remains ACP v1. It supports load with replay, resume without replay, close, delete, list, permission modes/config options, embedded resource context and resource links. Prompt images/audio depend on the configured provider. Local stdio MCP requires explicit host-command trust; HTTP MCP is supported in the default restricted profile.

Resource links and embedded resources retain their original prompt blocks for
replay. References do not automatically fetch URLs or open files; supplied binary
resources are retained without claiming that a text model decoded them. The
runtime does not expose client filesystem or terminal callbacks; its file tools
operate inside the agent's granted workspace.

## Session model and reasoning choices

The standard `session/set_config_option` method accepts `model` (category
`model`) and, when configured, `reasoning_effort` (category `thought_level`).
New, load and resume return the current values in `configOptions`. A successful
change returns the full option list and emits `config_option_update`.
The existing `mode` option continues to govern tool permissions independently.

By default the only model choice is the installation's configured model. An
owner can add `model_options` to `runtime.json`, alongside `provider`:

```json
{
  "model_options": [
    {
      "model": "gpt-6.1-sol",
      "name": "Sol",
      "reasoning_effort": "medium",
      "reasoning_efforts": ["low", "medium", "high"]
    },
    {
      "model": "gpt-6-luna",
      "name": "Luna",
      "reasoning_effort": "low",
      "reasoning_efforts": ["low", "medium"]
    }
  ]
}
```

The default model entry and effort must match `provider.model` and
`provider.reasoning_effort`. These are explicitly configured choices, not model
discovery or a claim of account entitlement. All choices use the same configured
endpoint, account selector, provider capabilities and permission policy. No
client can provide a new endpoint or credential. Include `default` in a model's
`reasoning_efforts` only to allow omitting the provider reasoning parameter; this
differs from the explicit effort `none`. An empty list exposes no reasoning
control. Invalid, duplicate or unsupported configuration is rejected at startup.

An explicit choice is saved per session and survives close, process restart and
load/resume. A new session still uses installation defaults. Other connections
resolve the latest durable choice when admitting their next turn. Changes during
an active turn are rejected, including across processes. Unknown values leave
the choice and permissions unchanged. Model changes select the new model's
configured default effort; reasoning changes affect only the current model.

Model changes accept empty or ordinary text-only histories. Private reasoning,
provider continuation, media, tool/effect history or unknown item shapes refuse
a model change with a remedy to keep the model or explicitly start a new session.
The runtime does not strip private state, rewrite old history, retry tools, or
create a replacement session to make a switch work. Saved input uncertainty and
original observations are retained. Reasoning changes keep the same model binding.
If an owner removes a saved option or changes its provider route/account
configuration, that session fails visibly rather than silently falling back.
Restore the option/configuration to continue it. Older runtimes do not understand
the saved choice and their provider binding check refuses a different model;
downgrading is not a model migration.

Tests use real ACP subprocesses with a deterministic local provider. They cover
durability, current-call exclusion, connection isolation, unchanged configuration,
text retention and refusal of opaque history. Real provider acceptance remains
specific to the configured model/account.

## Workspace roots

`sessionCapabilities.additionalDirectories: {}` is advertised. A session's effective filesystem roots are its `cwd` plus the explicitly requested `additionalDirectories`. Relative paths always start at `cwd`.

The installation owner can configure `additional_workspaces` as an allowlist of existing absolute directories. A client can request additional roots inside the primary configured workspace or this allowlist. The runtime rejects roots outside that allowlist, runtime/credential exposure, relative paths and redirected grants. Merely adding a directory to the configuration does not give every session access to it.

New, load and resume construct fresh, connection-local grants. Omitting the field or sending `[]` activates no additional directories. Stored messages, persona text and previous session grants cannot re-enable access. Separate clients connected to the same saved conversation may have different current root grants.

File operations use directory capabilities and preserve symlink/hard-link checks. Linux commands bind only granted roots into bubblewrap; macOS Seatbelt rules use that root list; native Windows grants a fresh AppContainer SID to those roots and removes its ACL entries during cleanup. System executable/read paths remain available to the command executor, without host home, credentials or networking. The primary configured workspace is an installation limit, not an implicit grant to its sibling projects when the session `cwd` selects a subfolder.

## Structured input

The runtime reads `clientCapabilities.elicitation.form` during initialization. Only an explicit non-null capability object enables its `user_input` model tool. That tool supports small forms containing text, choices, numbers and booleans, delivered with standard `elicitation/create`, a session ID and a tool call ID.

Replies are validated against the generated schema. Decline/cancel values are discarded; an accepted answer with missing required fields, wrong types or unoffered choices is rejected. Requests belong to the receiving ACP connection and use its request cancellation mechanism. Answers provide task information, never permission to perform an operation. Clients without form support continue through ordinary conversation. URL-based authentication elicitation is not implemented; forms must not request credentials.

## Optional client system instructions

`allow_client_system_prompt` defaults to `false`. The installation owner must enable it explicitly for a client allowed to append system-level instructions. When enabled, the runtime advertises the preview `sessionCapabilities.systemPrompt` marker and accepts only `{"mode":"append","content":[{"type":"text","text":"..."}]}`. Override mode and non-text blocks are rejected before creating or loading a session. The runtime's own instructions remain present.

The appended instructions are connection-local. Reconnects must send them again; omission uses the installation's default instructions. They are not stored as user conversation messages. This option is separate from persona files, ordinary context blocks, local MCP process authority and filesystem root grants. A remotely reachable ACP service should leave it disabled unless every client using that runtime connection has the required authority.

## Completed public commentary

Clients may opt in to `com.microsoft.amplifier.message.completion` version 1 through ACP capability metadata. For OpenAI Responses streams, a completed assistant output item emits a bounded text-completion receipt. This lets a conversation adapter post a whole public contribution while later work continues. Private reasoning is never included. The receipt is emitted only when the provider’s completed snapshot matches the streamed text; it does not claim task completion.

Ordinary ACP clients continue to receive the same text chunks and final prompt result. Other providers retain their existing output behavior; adapters must not infer completion from pauses or partial text. See the [conversation delivery contract](https://github.com/microsoft/amplifier-constellation/blob/main/crates/acp-connection/docs/public-speech.md).

## Protocol version and transport

The production handler implements [ACP v1](https://agentclientprotocol.com/protocol/v1/session-setup). A v2 initialization request receives the supported v1 response; a client can continue if it supports v1, or reconnect with a v1 initialization to negotiate v1-specific capabilities. No v2 support is advertised.

Adding v2 requires a separate negotiated handler, not a changed version number. The documented differences include acceptance-only prompt replies, foreground state notifications and failures after acceptance, IDs/upsert semantics for messages/tool calls, a changed permission envelope, new initialization capability shape, resume-with-replay replacing load, config options replacing modes, and removal of client filesystem/terminal callbacks. The existing engine's completed-turn result cannot be relabeled as a v2 prompt-acceptance result. A v2 implementation would need its own handler and conformance checks.

The native executable uses standard ACP stdio. Agent Instance's authenticated WebSocket tunnel remains the explicitly named `agent-instance.wss/1` transport with a standard stdio attach bridge. The upstream [Streamable HTTP/WebSocket transport RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport) is separate from this runtime's stdio transport contract. The RFD's HTTP/2, connection/session header routing, connection/session SSE streams and termination behavior are not implemented by this runtime. The existing tunnel must not be described as that RFD's complete implementation.

## Validation

The local protocol suite covers actual granted-root reads, another session's denial, revoke-on-resume, rejected roots, optional system instruction delivery/removal, explicit form capability negotiation and answer validation. Rust boundary and command tests exercise additional roots and denial outside the current root set. Windows has a native child-process test for access, revocation, and exact ACL restoration across multiple roots.

The `--trusted-local-client` profile permits the client-provided stdio MCP
programs required by the upstream v1 TCK. The restricted default intentionally
disallows those programs, so it is a policy-limited profile rather than a claim
of full v1 conformance. Protocol fixture results do not establish actual provider
quality. Use [validation](validation.md) and the source's applicable CI checks
for reproducible verification; old release test counts are not a current verdict.
