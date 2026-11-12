# Runtime boundaries

```text
Slack / browser / editor / other ACP client
                    |
             standard ACP stdio
                    |
     [optional Agent Instance remote bridge]
                    |
         Native Rust Agent Runtime
        /          |             \
 durable state  model/tool loop   explicit MCP connections
                    |
             selected model provider
```

The optional remote bridge is the host's explicitly named transport, not a claim that WebSocket is standard ACP. Clients that only support ACP stdio can use its attach command. Management, persona import, and scheduler APIs are optional host extensions and are not required by the runtime.

## Identity, conversations, and sessions

A runtime home is one individual's execution state. Multiple clients can create or load independent sessions in that home; session locks prevent two processes from modifying one active history. Separate sessions do not automatically share transcripts. The Agent Instance host can attach authorized conversation-source tools and instruction profiles to each session. The source provider must enforce current agent participation and human/audience access. Sessions are working contexts; the durable agent instance remains the identity above them.

The current engine does not synthesize a global memory or merge conflicting commitments. Agent Instance owns the optional responsibility ledger. Changes to a host's profile apply through its prompt preparation; model-specific continuation state stays in the native engine's database.

## Model and tools

Each provider converts the saved conversation and tool definitions into its own protocol: Responses, Anthropic Messages, Gemini generateContent, Ollama chat, or OpenAI-compatible Chat Completions. Streaming parsers handle fragmented SSE or NDJSON. The model/tool loop, permission checks, cancellation, and ACP events are shared by every provider.

Provider continuation remains private: Responses items are retained intact; other protocols store their native assistant message alongside normalized public text/tool records. This preserves Anthropic thinking signatures, Gemini thought signatures, and supported private reasoning fields through tool calls and restarts. Only outward response text and tool observations go to ACP clients. Raw reasoning is not displayed. A complete, validated response is required before a tool can execute; interrupted streams never start an effect.

Provider, endpoint, account-route and saved-history compatibility follow the
[provider identity contract](provider-identity.md). Explicit per-session model
and reasoning choices follow the [ACP capability contract](acp-optional-capabilities.md#session-model-and-reasoning-choices):
installation-approved same-route model choices can accept empty or ordinary
text-only histories; tool, media, private-continuation and unknown history shapes
refuse a model change. Incompatible private state is not implicitly converted.

Use `configure-provider` for an installation-level provider change: it pins legacy
transcripts to the old configuration before switching. Do not manually rewrite an
unbound legacy configuration before pinning. Existing ACP processes retain their
captured installation configuration; reconnect to use a changed one. This is
separate from durable per-session choices resolved at turn admission.

Local and MCP function calls execute sequentially within one model/tool loop.
Before a tool effect, the function call is persisted. The actual returned result is then persisted. Recovery supplies an explicit unknown-outcome result for unmatched calls; it never executes the old call again. Session mutation and tool execution are protected by an installation-local lock file per session. Multiple independent sessions can run concurrently within configured process capacity.

File operations use a directory capability rooted in the workspace. Parent traversal, symlink escape, writes through hard links, non-regular file access, and oversized reads/writes are rejected. Linux commands run in bubblewrap with a bounded workspace and system binaries, separate process group, owner-controlled networking (disabled by default), and a clean environment. Command cancellation terminates the group. There is no unrestricted fallback.

Linux command environments include read-only synthetic `passwd` and `group`
entries for the current mapped identity, named `agent`. This supports ordinary
user lookups in build and test tools without exposing host accounts, home paths,
password databases, or credentials. It does not grant network or host access.

## Foreground execution and recovery

An ACP connection owns active foreground execution. Disconnecting cancels it.
The separate Instance background-work service can own explicitly authorized
work across a browser disconnection. Saved history alone is not a scheduler.
Conversation replay displays saved results without rerunning tools. Cancellation
of a remote mutation can leave an unknown effect: inspect its outcome before
retrying. Closing the connection cannot roll back an external action.

## MCP trust and discovery

Local MCP children receive a reduced environment and explicit per-server entries,
not the model key. HTTP credentials attach only to the specified server;
redirects are disabled. The runtime does not implement sampling or arbitrary
client callbacks.

Client-supplied local MCP programs are disabled by default. For a trusted local
ACP host, launch `agent-runtime --home /absolute/instance acp --trusted-local-client`.
This enables standard stdio MCP for that process without changing saved settings.
Setting `allow_client_stdio_mcp: true` enables it persistently. Either choice gives
that client authority to start host programs, beyond workspace tools and session
modes. Remote installations normally keep this restricted. The
[ACP validation profile](acp-optional-capabilities.md#validation) distinguishes
trusted-local TCK coverage from the policy-limited default.

The runtime does not infer permission from a persona. MCP annotations alone do
not grant permission: `trusted_mcp_readonly_urls` names exact HTTP endpoints whose
read-only declarations are trusted. Other MCP calls require approval, and
read-only mode denies them. Optional `allowed_mcp_mutations` entries use the exact
HTTP endpoint plus `#tool_name`, never a client-selected server label. Source
access is enforced by each provider's credential and audience policy.

Optional MCP integrations can set
`_meta["io.github.bkrabach.agent-runtime"].optional: true` on their ACP MCP
descriptor. Failed optional discovery is bounded to five seconds and leaves other
connections usable, with a redacted degraded-connection report in the agent's
instructions, `runtime_capabilities`, and turn observations. Server aliases retain
the original descriptor index. Required connections remain required. Reload a
session to retry discovery; uncertain tool actions are never automatically replayed.
This is an optional namespaced extension, not a requirement of standard ACP.

## Credentials and storage

The model key is read from the configured environment variable. The standalone engine does not authenticate the local ACP caller; the invoking operating-system identity controls access. Use an authenticated host/transport to make it reachable remotely. SQLite history contains conversation and tool outputs, so it is private operational data and must be protected accordingly. Unix installations create private directories/files; Windows access also depends on the user's directory ACLs.

## Compatibility

The protocol implementation follows [ACP initialization](https://agentclientprotocol.com/protocol/v1/initialization), [sessions](https://agentclientprotocol.com/protocol/v1/session-setup), [tools](https://agentclientprotocol.com/protocol/v1/tool-calls), and [cancellation](https://agentclientprotocol.com/protocol/v1/cancellation). Provider iteration follows [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling) and [reasoning continuation](https://developers.openai.com/api/docs/guides/reasoning). MCP transports follow the [2025-06-18 transport specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports).

Old Codex engine sessions are not native engine sessions. Migrating an existing agent needs an explicit source-history and state migration plan, with provenance and rollback. Sharing an ACP interface does not make providers' internal history formats interchangeable.

## Command networking

The owner can set `command_network` to `inherit` on Linux, independently of
workspace filesystem grants. The default, `isolated`, preserves the existing
network-disabled behavior. This setting is installation configuration, never a
model tool argument or an ACP client's permission request. It applies when the
runtime reconnects. The CLI configuration API, configuration schema, `check`,
model tool description and `runtime_capabilities` expose the effective policy.

`inherit` shares the enclosing process's network namespace. A container's
firewall still applies; on a host this can include its LAN and local services.
It is not an internet-only firewall. Filesystem/process isolation, cleared
environment, workspace grants, synthetic account, cancellation and deadlines
remain enforced. Only resolver/hosts/NSS files and known CA bundles are mounted
read-only for DNS and HTTPS; host credential directories are not mounted. GitHub
MCP credentials remain separate from the shell. Authenticated pushes can use the
configured GitHub tools; this option does not inject their tokens into commands.

macOS and Windows currently reject `inherit` rather than silently promising
network access. Their existing isolated command behavior is unchanged.

## Refreshed client context

A runtime can advertise `_meta["com.microsoft.amplifier.context-snapshot"]`
with `version: 1`, `contentBlocks: true`, and `originalHistoryPreserved: true`
in ACP initialization. Supporting clients may mark a text block with the same
namespace, `version: 1`, a publisher/conversation/audience-specific `key`, and
`retainedText` containing the original request and essential continuation state.
The key is nonempty and at most 256 bytes; retained text is nonempty and at most
32 KiB. Clients use the envelope only after advertisement, and intermediaries
preserve block metadata when prepending context. Media and independent instructions
remain separate blocks. This is an optional extension, not a standard ACP requirement.

Before ordinary budget compaction, the model view keeps the newest valid block
for each key. Older versions retain their original position and retained text.
Other user content, media, assistant output, tool pairs and provider state stay
in order. Invalid/unmarked content stays unchanged. Original `_acp_prompt` and
saved content are never rewritten; replay and `session_history` retrieve them.
Archive hashes and item indexes refer to the durable original transcript.
Clients must never reuse a key across audiences or put a one-time instruction
only in the replaceable projection. Metadata grants no tool or source access.

Permission cancellation may carry the optional fixed diagnostic reason in
`_meta["com.microsoft.amplifier.permission"].reason`. The runtime distinguishes
`source_changed`, `policy_changed`, and `expired` from rejection. These codes
cannot grant permission, and unknown client text is not repeated as authority.
