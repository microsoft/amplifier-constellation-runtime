# Native ACP capabilities and authority

The production wire profile remains ACP v1. It supports load with replay, resume without replay, close, delete, list, permission modes/config options, embedded resource context and resource links. Prompt images/audio depend on the configured provider. Local stdio MCP requires explicit host-command trust; HTTP MCP is supported in the default restricted profile.

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

## Version and transport audit — November 2, 2026

The upstream documentation labels [v1 as Latest](https://agentclientprotocol.com/protocol/v1/session-setup) and [v2 as Draft](https://agentclientprotocol.com/protocol/v2/migration). A v2 initialization request receives the supported v1 response; a client can continue if it supports v1, or reconnect with a v1 initialization to negotiate v1-specific capabilities. No v2 support is advertised.

Adding v2 requires a separate negotiated handler, not a changed version number. The documented differences include acceptance-only prompt replies, foreground state notifications and failures after acceptance, IDs/upsert semantics for messages/tool calls, a changed permission envelope, new initialization capability shape, resume-with-replay replacing load, config options replacing modes, and removal of client filesystem/terminal callbacks. The existing engine's completed-turn result cannot be relabeled as a v2 prompt-acceptance result. This remains a separate experimental implementation and TCK track; the production rollout does not claim it is complete.

The native executable uses standard ACP stdio. Agent Instance's authenticated WebSocket tunnel remains the explicitly named `agent-instance.wss/1` transport with a standard stdio attach bridge. The upstream [Streamable HTTP/WebSocket transport RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport) is active, not the stable stdio transport contract. Its HTTP/2, connection/session header routing, connection/session SSE streams and termination behavior are not implemented by this runtime. The existing tunnel must not be described as that RFD's complete implementation.

## Validation

The local protocol suite covers actual granted-root reads, another session's denial, revoke-on-resume, rejected roots, optional system instruction delivery/removal, explicit form capability negotiation and answer validation. Rust boundary and command tests exercise additional roots and denial outside the current root set. Windows has a native child-process test for access, revocation, and exact ACL restoration across multiple roots.

The 0.7.0 Windows build was compiled and tested natively on Windows using Rust 1.99.1: 17 library, eight boundary, nine native command and two migration tests passed, along with 20 protocol cases, the real command-approval check and strict all-target Clippy. The native command cases include multi-root access, revoke-on-reconnect and exact ACL restoration. These are fixture-based checks, not a claim that every provider has been exercised on Windows.

The pinned upstream ACP v1 TCK (`b7f40fb3c003c45926547be5a1f7e2b374035815`) passed 67 tests with seven optional/inapplicable skips under `--trusted-local-client`, including all 31 mandatory checks and additional-directories lifecycle acceptance. The model is a local deterministic fixture. The default profile intentionally disallows arbitrary client-provided local MCP programs, so that restricted policy does not satisfy the TCK's mandatory stdio-MCP acceptance requirement.
