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

Each exercised session is pinned to its provider, endpoint, model/deployment, and optional Azure API version. Credential rotation is allowed; changing those context boundaries requires a new session. The additive database migration preserves pre-0.2 OpenAI transcripts. Use `configure-provider` to change an installation: it pins legacy transcripts to the old configuration before switching. Do not manually rewrite a legacy configuration before its first upgrade or pinning. Existing ACP processes keep the configuration they started with; reconnect to use a new one.

Before a tool effect, the function call is persisted. The actual returned result is then persisted. Recovery supplies an explicit unknown-outcome result for unmatched calls; it never executes the old call again. Session mutation and tool execution are protected by an installation-local lock file per session. Multiple independent sessions can run concurrently within configured process capacity.

File operations use a directory capability rooted in the workspace. Parent traversal, symlink escape, writes through hard links, non-regular file access, and oversized reads/writes are rejected. Linux commands run in bubblewrap with a bounded workspace and system binaries, separate process group, no network, and a clean environment. Command cancellation terminates the group. There is no unrestricted fallback.

Linux command environments include read-only synthetic `passwd` and `group`
entries for the current mapped identity, named `agent`. This supports ordinary
user lookups in build and test tools without exposing host accounts, home paths,
password databases, or credentials. It does not grant network or host access.

MCP stdio startup is disabled by default because it grants host-command authority to the connected client. An owner may enable it explicitly for fully trusted local clients. Those children receive a reduced environment and explicit per-server environment entries, not the model key. HTTP credentials are attached only to the specified server; redirects are disabled. The runtime does not implement sampling or arbitrary client callbacks. Remote side effects cannot be rolled back by closing the connection.

## Credentials and storage

The model key is read from the configured environment variable. The standalone engine does not authenticate the local ACP caller; the invoking operating-system identity controls access. Use an authenticated host/transport to make it reachable remotely. SQLite history contains conversation and tool outputs, so it is private operational data and must be protected accordingly. Unix installations create private directories/files; Windows access also depends on the user's directory ACLs.

## Compatibility

The protocol implementation follows [ACP initialization](https://agentclientprotocol.com/protocol/v1/initialization), [sessions](https://agentclientprotocol.com/protocol/v1/session-setup), [tools](https://agentclientprotocol.com/protocol/v1/tool-calls), and [cancellation](https://agentclientprotocol.com/protocol/v1/cancellation). Provider iteration follows [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling) and [reasoning continuation](https://developers.openai.com/api/docs/guides/reasoning). MCP transports follow the [2025-06-18 transport specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports).

Old Codex engine sessions are not native engine sessions. Migrating an existing agent needs an explicit source-history and state migration plan, with provenance and rollback. Sharing an ACP interface does not make providers' internal history formats interchangeable.
