# Long conversations

## Native history source

`agent-runtime --home /absolute/engine history-query /private/query.json` provides a read-only projection of this engine's existing store for an authorized context service. It needs no model credential and makes no model request. This is the local `agent-runtime.history/1` management contract, not a standard ACP method. Direct invocation has the OS owner's authority; a network service must enforce its caller's audience before invoking it.

Supported queries are `{"operation":"describe"}`, `{"operation":"list","store_id":"…","session_ids":["…"],"limit":20,"cursor":null}`, and `{"operation":"read","store_id":"…","session_id":"…","limit":20,"before":null}`. Describe returns the stable store ID. List operates only on the caller-supplied authorized IDs, returns up to 40 rows and a `next_cursor`; read returns recent public messages and `next_before` for older pages. The instance service admits only its explicitly registered owner sessions and checks live caller/grant authorization.

No transcript is copied into the instance database. Replacing the native store changes its ID and invalidates old source bindings. User/assistant text is projected separately from reasoning, tool traces, profile injection, and provider-private state. Large text excerpts identify truncation; resource bodies and media stay in the original session and replay through ACP. `agent-history://STORE/SESSION#message-N` URIs identify native source records; they are not public web URLs. Native sessions from external rooms remain discoverable through their authoritative connector instead of bypassing its membership rules via a local copy.

## Working context and compaction

The saved transcript stays intact. Model requests use a separate working window.
OpenAI Responses uses server compaction during a live WebSocket chain, and the
standalone compact endpoint before starting a large new chain. The entire
standalone response is the canonical next window; only a server compaction item
may replace an older prefix. Steering successors and pending tool results remain
on their original chain. Provider-signed or opaque records never enter public
history or status labels.

Anthropic uses capability-checked on-demand compaction (`compact-2026-09-04`).
The exact signed block is retained with the unchanged system/tools/settings.
Compaction is applied only after its terminal success, including streamed
responses. Unsupported account/model routes fail explicitly with original
history preserved; disable `provider_compaction` to select the extractive
fallback. This release uses foreground compaction; it does not race background
summaries against an evolving transcript.

Checkpoints are bound to the original prefix, provider and settings hashes.
They survive restart but cannot be applied to a changed prefix. Other providers
use a bounded extractive archive note with item ranges and a digest, retaining
the current tool continuation. The `session_history` tool retrieves original
public messages when more detail is needed. No compaction grants authority or
asserts that work is complete.

```json
{
  "limits": {"turn_seconds": 0, "provider_idle_seconds": 300},
  "context": {
    "auto_compact": true,
    "provider_compaction": true,
    "compact_threshold_tokens": 100000,
    "max_compactions_per_turn": 4,
    "prompt_caching": true,
    "retain_recent_turns": 3,
    "archive_note_bytes": 8192
  }
}
```

A zero whole-turn timeout allows healthy long work. It does not remove model-call,
tool-operation, output or assignment limits. Transport inactivity is a separate
setting; WebSockets use ping/pong and HTTP uses read inactivity. WebSocket
connections rotate before their service lifetime only at a settled response
boundary. Ambiguous calls are never retried automatically.

Stable instructions and deterministically ordered tools preserve cache prefixes.
Anthropic marks the stable system prefix and requests automatic caching; OpenAI
uses its automatic prefix cache. Credential values are never part of diagnostics.

## Execution measurements

`agent-runtime --home /absolute/engine diagnostics` returns content-free counts,
model-step latency, latest working-context size, cache usage and explicit stop
reasons. Original transcript bytes and model-window bytes are separate. Counts
cover the stated stored-event sample; engine model steps, provider-created
responses and usage notifications are different measurements. Missing usage is
not zero cost. Compaction usage is separately recorded in `context.compacted`.
No monetary savings are inferred from cache tokens or reduced input bytes.

The Instance dashboard exposes the same report through
`GET /api/v1/native-observations` for an authorized observer. Model connection
and context settings are available in advanced engine settings and the normal
configuration API/CLI.

Implementation guidance: [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction),
[Responses WebSockets](https://developers.openai.com/api/docs/guides/websocket-mode),
[steering](https://developers.openai.com/api/docs/guides/steering),
[Anthropic on-demand compaction](https://platform.claude.com/docs/en/build-with-claude/compaction-on-demand),
and [prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).
