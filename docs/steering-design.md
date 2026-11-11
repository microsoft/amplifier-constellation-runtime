# Steering and repeated input: implementation design

This document defines the native input journal and provider integration. The
[runtime overview](runtime-overview.md) illustrates the components; the shared
[input-continuity agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/input-continuity.v1-candidate.md)
owns cross-component meanings.

## Model support and provider integration

The shared [model-support agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/model-support.v1-candidate.md)
owns support policy. This engine's [provider definitions](providers.md),
[provider identity](provider-identity.md) and [explicit session choices](acp-optional-capabilities.md#session-model-and-reasoning-choices)
define concrete configuration, history compatibility and request behavior.
Provider catalogs and account availability must be inspected when selecting work;
this contract does not maintain a dated model inventory.

### Native OpenAI generation

The native `openai` provider targets GPT-6 and later supported GPT generations,
using Responses WebSockets for generation and native steering. Retire older GPT
model execution from this path and remove its HTTP/SSE generation fallback.
HTTP remains appropriate for account setup, model discovery, file transfer and
other non-generation APIs. Other providers retain their own transports.

GPT-6+ is a product support policy, not an assertion that every future model or
every OpenAI-compatible endpoint supports the same capabilities. Validate the
selected model, account route, endpoint and request settings. Preserve the
selected tier and supported reasoning effort. A model-name prefix alone does
not prove steering support. The default remains an explicit model choice.

This does not remove specialist image/decision models or rewrite independent
third-party ACP engines. Azure, compatible gateways and ChatGPT subscription
authorization require their own tested capability records; do not silently use
different credentials, change billing routes, or substitute another model.

Reject an unsupported older OpenAI model with a clear setup/migration action.
Preserve old transcripts and provider bindings. A model change creates a new
provider-bound execution context; any transfer of public history is explicit.
Do not replay opaque provider state under a different model. Historical fixtures
and migration tests may retain old model names with their historical meaning.

## One logical turn, many provider responses

The session writer owns a durable input journal and a logical turn. Provider
responses are children of that turn. Native steering may create more responses;
none creates a second ACP prompt lifecycle or a fresh execution allowance.

The input journal is independently writable while the engine holds its session
lock. Admission and terminal transition share an active-turn gate; admission
commits its durable receipt while holding that gate. Avoid holding a database transaction across network/tool waits.
A single writer integrates admitted input into session history and records its
model-request association. Callers never concurrently mutate `session.items`.

The shared [input-continuity agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/input-continuity.v1-candidate.md)
defines the meanings consumers rely on; this document owns the native mechanism.

## OpenAI event loop

Use one ordered response chain per active session initially. A session owns its
socket lifecycle; sharing or multiplexing sockets is a later optimization. The
same reader handles streamed output, steering receipts and response transitions
while local tool work is awaited. Blocking socket reads during tools would lose
the benefit and complicate receipt ownership.

Begin a response, wait for its ID, then submit pending eligible input through
`response.steer`. Track the returned steering ID separately from local input
IDs. When a successor is created, new steering targets its ID. Keep reading
through ordinary completion or a `steered` incomplete response when a successor
is expected. Already emitted output and started tools remain real history.
See [OpenAI steering](https://developers.openai.com/api/docs/guides/steering).

Allow one unresolved batch in the chain. Snapshot a bounded ordered prefix from
the local inbox; each member remains a distinct user message. Additional arrivals
remain queued until the batch settles. Do not keep steering an old parent while
its successor is forming, or race an explicit continuation against an expected
automatic one. No arbitrary wait is needed before the first update; later
arrivals are naturally coalesced at these boundaries.

Record submission before transmission. Acceptance means remotely queued;
successor creation establishes commitment. If client results or approvals are
required, fulfill the identified call IDs from saved outcomes in one matching
continuation per parent. Do not duplicate already accepted guidance. Preserve
instructions, tools and request settings on explicit continuations. These
semantics and request-mode restrictions come from the
[WebSocket event reference](https://developers.openai.com/api/reference/resources/responses/websocket-events).

Once a tool-result continuation is necessary, send the required results without
resending remotely accepted guidance. Keep newer locally queued input ordered
until the successor is created, then steer that successor. Each member keeps
its own receipt.
Never hide instructions inside a tool result or treat a lost result as success.

An unsupported mode is a configuration error on this native path, not a silent
return to legacy generation. Keep server-side automatic compaction and server
conversation binding disabled while they conflict with steering. Our own context
checkpointing is a separate operation: perform it only at a settled boundary,
preserving original history and pending input, then start a fresh response chain
from the permitted compacted view. No hidden nested provider multi-agent turn.

## Burst example

```mermaid
sequenceDiagram
    participant E as Experience / Instance
    participant R as Runtime inbox + turn owner
    participant P as OpenAI WebSocket
    participant T as Local tool
    R->>P: Start response R1
    P-->>R: R1 created
    E->>R: Message A
    R->>P: Steer R1 with A
    E->>R: Messages B and C
    Note over R: B and C stay ordered locally
    P-->>R: A accepted; R1 requests tool
    R->>T: Execute tool once
    E->>R: Message D
    T-->>R: Actual saved result
    R->>P: Continue R1 with real result
    Note over P: Accepted A is incorporated by provider
    P-->>R: R2 created; A committed
    R->>P: Steer R2 with B, C, D
    P-->>R: Batch accepted; R3 created
    E->>R: Message E
    R->>P: Steer R3 with E
    P-->>R: E accepted; automatic R4 created
    P-->>R: R4 finishes
    Note over R: Atomically close only if admitted input is settled
    R-->>E: One completed logical turn
```

The example illustrates ordering, not a claim that each message needs a response
of its own. An update may add a requirement, correct a detail or ask a related
question; preserve all of them for the model to reconcile. The runtime does not
silently summarize or discard a burst.

## Limits, failures and recovery

Every automatic successor consumes the logical model-call budget and contributes
to usage. Account for native hosted-tool usage as observable. Provider per-response
limits cannot replace our logical-turn limits. Reserve continuation capacity
before steering, and stop accepting additional work into a turn that cannot
honor it; retain that input for its next authorized lifecycle. Deadline and
explicit-stop handling remain active while streaming or waiting for tools.

On a confirmed steering rejection, return input to local ownership without
replaying accepted batches. On capacity rejection, wait for actual progress;
avoid retry storms. On disconnection, a missing acknowledgment is unknown, not
evidence of rejection. Preserve the ledger, output items and actual tool results.

With `store:false`, a new connection cannot assume access to the previous socket's
cached response. Rebuild only from complete saved context when safe. If the
history needed to establish incorporation or hosted-tool effects is missing,
mark recovery required instead of asserting exactly-once remote execution.
Never rerun a local action to reconstruct a missing acknowledgment. This follows
[OpenAI continuation and recovery](https://developers.openai.com/api/docs/guides/websocket-mode).

The local journal can guarantee single local incorporation. The external API
does not give us a blanket exactly-once guarantee across unknown network outcomes.
Expose that distinction to diagnostic tools without flooding the conversation.

## Other model providers

Use the same journal and turn ownership. Finish the current request and required
tools, then include the admitted input before the next legal model request.
If the call returns a final answer, keep that output as a draft and continue when
input was already admitted. Retain opaque provider reasoning/signature data.

Each encoder enforces its protocol: Anthropic tool results immediately follow
their tool-use message, ahead of new user text; unresolved server-tool work may
require further continuation before text can be added. Preserve parallel results
and thinking blocks. See [Claude tool-call handling](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)
and [tool troubleshooting](https://platform.claude.com/docs/en/agents-and-tools/tool-use/troubleshooting-tool-use).

## ACP and component changes

Expose the compatible optional `_session/steering` capability with an idle
`promptRequired` outcome. Its acceptance binds input to the running lifecycle;
the original `session/prompt` must not finish while that input is still owned by
it. Version any stronger durable receipt metadata separately. Instance must
authorize and decorate newly admitted input as it does initial prompts; do not
create a raw-input bypass around its context and audience policy.

Slack and other clients route new eligible messages through that capability.
Where an opaque third-party agent cannot steer, retain the ordinary queued-prompt
fallback. Stop using unrelated human-message revision changes as revocation of
an active operation or its outgoing results. Keep true stop, source deletion,
authority change and operation-specific invalidation distinct.

Primary implementation locations are `server.rs`/`store.rs` for admission and
state, `engine.rs` for loop boundaries, a dedicated provider WebSocket module for
OpenAI transitions, and provider encoders for legal fallback insertion. Instance
and adapter changes accompany the runtime; transport support alone does not fix
the end-to-end conversation behavior.

## Release evidence required

Test deterministic interleavings with a controllable WebSocket server and tools:
bursts before response creation, after acceptance, during each tool step and
across repeated successors; bounded queue rejection; duplicate events; lost
acknowledgments; expired response IDs; unknown outcomes; input/finalization races;
and exhausted budgets. Assert exact input order, original task identity and
single tool-effect counts. Exercise provider-native media/status behavior too.

Run equivalent model-boundary tests for other providers and ordinary ACP fallback,
then disposable authenticated GPT-6 and third-party ACP checks. Finally exercise
messages-during-work and unchanged-approval cases through a real experience.
Keep implementation, fixture validation, live provider validation and fleet
rollout as separately recorded milestones. Nothing in this design claims those
milestones have already occurred.

## Implemented receipt and recovery interface

`initialize._meta.steering` advertises the optional input methods.
`_session/steering` accepts `sessionId`, ACP `prompt` blocks, and
`_meta.steering` containing a stable `inputId` and `idleBehavior: promptRequired`.
`injected` acknowledges durable admission; `promptRequired` means no active turn
accepted it. Each receipt moves through `received`, `submitted`, `remoteQueued`
(native steering), and `incorporated`, or ends `notAccepted`/`unknown`. Per-input
receipts appear in empty text updates under the runtime metadata namespace and
in the final prompt result. No user-facing empty chat bubble is required.

Receipts and original input remain attached to that session. Loading a session
returns bounded receipt metadata. `_session/input_receipts` requires a loaded
session and returns retained input blocks plus receipt/remote IDs; it is not a
cross-conversation lookup. Uncertain prior submissions, or a successor whose
input was committed but whose complete history was not saved, remain quarantined
without replay. They do not block a genuinely new prompt. The model receives a
bounded metadata notice with receipt IDs and states, telling it to inspect effects
before related actions. Uncertain input text is not inserted into the new request,
and proceeding does not mark the old outcome known or acknowledged.

After inspecting the receipt and real effects, an authorized client can call
`_session/acknowledge_inputs` with that loaded `sessionId`, explicit `inputIds`,
and `disposition: proceed_without_replay`. This records acknowledgment without
changing the original outcome, replaying input, starting work, or claiming
completion. The next prompt is a separately authorized action. An active
session cannot be reconciled concurrently; session deletion removes its receipts.

The runtime bounds one turn to 256 additional messages/4 MiB of retained input,
transmits at most 16 per steering batch, and enforces context capacity before
sending. Inputs waiting before the first request can join that first call.
Created/completed/acceptance events are idempotent by identity; conflicting
duplicates fail visibly. A final receipt can say `notAccepted` when the original
turn's capacity expires. Neither that result nor `unknown` authorizes an automatic
retry.

An active Responses chain and its compacted/refreshed local history view are
different things. If accepted steering is still waiting for a tool result, the
runtime continues the existing chain with the engine's new local observations
only. It defers any changed history projection until that steering is settled,
without repeating earlier input or tool calls. The configured context budget
still applies. `model.failed` records bounded failure stage/category and chain
state without retaining provider error bodies, source text, or credentials.
