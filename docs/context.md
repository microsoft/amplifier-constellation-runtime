# Long conversations

## Native history source

`agent-runtime --home /absolute/engine history-query /private/query.json` provides a read-only projection of this engine's existing store for an authorized context service. It needs no model credential and makes no model request. This is the local `agent-runtime.history/1` management contract, not a standard ACP method. Direct invocation has the OS owner's authority; a network service must enforce its caller's audience before invoking it.

Supported queries are `{"operation":"describe"}`, `{"operation":"list","store_id":"…","session_ids":["…"],"limit":20,"cursor":null}`, and `{"operation":"read","store_id":"…","session_id":"…","limit":20,"before":null}`. Describe returns the stable store ID. List operates only on the caller-supplied authorized IDs, returns up to 40 rows and a `next_cursor`; read returns recent public messages and `next_before` for older pages. The instance service admits only its explicitly registered owner sessions and checks live caller/grant authorization.

No transcript is copied into the instance database. Replacing the native store changes its ID and invalidates old source bindings. User/assistant text is projected separately from reasoning, tool traces, profile injection, and provider-private state. Large text excerpts identify truncation; resource bodies and media stay in the original session and replay through ACP. `agent-history://STORE/SESSION#message-N` URIs identify native source records; they are not public web URLs. Native sessions from external rooms remain discoverable through their authoritative connector instead of bypassing its membership rules via a local copy.

## Working context and compaction

Version 0.4 separates the saved transcript from the model's current working context. Every user message, public assistant output, tool intent/result, and provider continuation record remains in the private session history. Resuming a session still replays its full public history to ACP clients.

When a request would exceed `limits.max_context_bytes`, the engine builds a smaller model view. It replaces an old, completed prefix with an **extractive archive note**, then keeps recent turns and the whole active turn. This is an intentionally incomplete excerpt with item ranges and a SHA-256 digest, not a claim that all old facts have been semantically summarized. An active tool call and its result are never separated. Provider-specific continuation remains intact within the retained turns, including signed/opaque fields where required.

The note tells the model how to request original public messages using `session_history`. That read-only tool supports text search, item offsets, and byte offsets for exact bounded retrieval. It accesses only the current session. It cannot reveal private reasoning or opaque provider records. Cross-conversation retrieval remains a separately authorized MCP capability provided by the host or experience.

Each distinct compacted prefix records a checkpoint and a `context.compacted` event with its provenance. The database records what was omitted from a particular model view without deleting or rewriting the transcript. Repeated model calls do not create duplicate checkpoints for the same prefix.

Defaults are:

```json
{
  "context": {
    "auto_compact": true,
    "retain_recent_turns": 3,
    "archive_note_bytes": 8192
  }
}
```

The preferred recent-turn count may be reduced to fit the budget; the current turn is always retained. If that turn, the instructions, or the tool definitions alone exceed the budget, execution stops with an explicit error. There is no silent active-turn truncation, automatic provider switch, or inference that an archived task is complete. Binary media is measured separately from text context.

This mechanism improves bounded long-session execution. It does not establish cross-agent shared memory, durable commitments, or a semantic memory quality guarantee.
