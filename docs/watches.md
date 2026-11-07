# Agent-authored attention watches

An agent defines what it needs to notice while pursuing an existing responsibility.
The runtime filters selected evidence with a fast decision model or exact predicate, retaining a
notification for the agent to inspect. It never supplies a creative plan, executes
an experience action, marks work complete, or grants new authority.

This is a local reusable capability, not an extension to the ACP specification.
An ACP host can expose it through MCP to any compatible agent implementation.
The first host is Minecraft; the policy also has software and workshop fixtures.

## Explicit task limits (helper v2)

Runtime 0.14 exposes static local discovery through `agent-runtime
helper-capabilities`, without a home, account or model. Require
`schema_version: 1`, `helpers.watch.version: 2`, `command: "watch-v2"`, and
`bound_kinds` containing `finite` and `no_configured_bound` before sending v2.
This does not add ACP fields or imply any runtime family owns the task.

`agent-runtime watch-v2 --store watches.json input.json` keeps the original
operations/evidence format, but each `task.deadline_ms` and
`task.remaining_turns` is explicitly `{"kind":"finite","value":123}` or
`{"kind":"no_configured_bound"}`. A finite deadline is absolute Unix
milliseconds; turns are the remaining allowance. Zero is exhausted, not a
sentinel. Missing, null, unknown and contradictory bounds are rejected. Use the
[v2 fixture](../examples/helpers/watch-v2.json) for a task with no configured
deadline and a finite remaining turn limit.

An authorized task may have either or both limits deliberately unconfigured;
its identity and running/waiting status are still required. Watches always have
their original finite 30–1800 second TTL. A finite task deadline further caps
that TTL. Removing a task bound does not extend any saved watch, change its
identity, rearm expired/cancelled watches, reset decision accounting or erase
notifications/cursors. Old and new helpers share the existing store format.
Only the task's owner may supply these facts; they do not create or amend work.
Provider/tool limits, Stop and action leases remain independent constraints.

The original `watch` command still requires numeric finite limits and retains
policy `responsibility-watch-v7`; v2 changes the input representation, not the
watch evaluator. An older helper cannot represent an unconfigured dimension:
do not substitute a large number, fresh deadline or invented allowance. Report
the missing capability, or use another explicitly supported observation path.

## Useful means

The development goal is satisfied by a practical first increment when:

1. Independent agents author useful watches during genuine creative collaboration.
2. A relevant public development wakes an existing responsibility without another
   person having to relay or explicitly address it, and the agent checks the result.
3. Irrelevant observation changes do not call a planner. Source categories that
   the agent did not select do not even call the decision model.
4. Watches survive restart, have bounded cost and lifetime, retain evidence, and
   cannot revive completed/stopped work or cross conversation scopes.
5. Exact replay inputs, model failures, and artifact inspections are available.

These criteria do not establish improved creativity, calibrated model confidence,
or a controlled cost saving across different tasks.

## Contract and ownership

`agent-runtime watch --store /private/agent/watches.json input.json`

The host supplies `scope`, current `task`, `events`, and an `action`. Each agent
has its own store. Scope and task authorization must come from the trusted host,
not incoming messages or agent tool arguments. The host also enforces audience
access when collecting evidence and rechecks task authority before any wake.

```json
{
  "scope": "public-room:agent-one",
  "task": {
    "id": "current-responsibility",
    "goal": "Design an enjoyable visitor activity",
    "status": "running",
    "deadline_ms": 1794000000000,
    "remaining_turns": 20
  },
  "events": [],
  "action": {
    "operation": "arm",
    "spec": {
      "label": "Peer trial",
      "any_of": [
        "My peer reports a completed trial, including a successful result needing no changes.",
        "My peer explicitly cancels the trial responsibility."
      ],
      "kinds": ["message"],
      "ttl_seconds": 600
    }
  }
}
```

Dates in the example are illustrative; use the real current task deadline.
An event has `id`, `kind` (`message`, `tool`, `environment`), `source`, and `content`.
Source labels distinguish peer reports from direct observations and tool receipts.
`initial_check` chooses whether to baseline existing evidence (`new_events`, the
backward-compatible default) or inspect it on the first poll (`current_state`).
Use current state for “when enough material is available” or “when that review
is complete”: the condition may already be satisfied at registration. This
avoids losing a result that arrived while the watch was being set up. Both modes
subsequently evaluate changes and retain the same task and scope boundaries.

Operations:

- `arm`: validate the agent's condition, select evidence kinds, establish baseline.
- `list`: inspect watches, notifications, expiry, and hourly calls.
- `cancel`: cancel a watch by `id`.
- `prepare`: first evaluate changed exact predicates locally; if selected semantic evidence changed, reserve a bounded evaluation and return
  its `token` and ordinary typed decision `request`; otherwise return a skip reason.
  `observe_only: true` records predicate matches without delivering notifications.
- `resolve`: accept normalized `result` for that token after checking the current
  task, lease, and exact source evidence. `observe_only: true` records a shadow
  outcome without delivering a notification.
- `abandon`: release a failed evaluation; the consumed call still counts.
- `ack`: record delivery after a successful planner turn. Notification ids permit
  idempotent handling; an interrupted delivery can be offered again.
- `recheck`: explicitly replay current evidence for an unexpired, still-armed
  watch after a host policy correction. This does not rearm a fired/cancelled
  watch, extend its expiry, or bypass the hourly budget.

Use one to four separate `any_of` entries. Each is evaluated independently, and
any matching alternative produces one notification identifying the matched
conditions. A legacy `condition` string is accepted for existing integrations;
do not supply both forms. The MCP tool offers the structured form.

### Message filters before semantic evaluation

With `responsibility-watch-v7`, a host can attach factual transport metadata to a
message event: `"message": {"author": "Lena", "recipients": ["Bea"],
"intent": "request"}`. Use authenticated speaker identity and explicit routing or
message-type fields from the experience. A name mentioned in prose is not an
addressee. Metadata narrows evidence; it does not authenticate the prose, prove
completion, or authorize an action.

An `any_of` entry may remain a string or use an exact filter:

```json
[
  {
    "condition": "Lena asks Bea for help on the current project",
    "message": {"authors": ["Lena"], "recipients": ["Bea"], "intents": ["request"]}
  },
  {
    "condition": "Lena reports the completed review, including no changes needed",
    "message": {"authors": ["Lena"], "intents": ["update"]}
  }
]
```

Values match exactly, with OR within a list and AND between specified fields.
At least one nonempty field is required. Missing message metadata cannot satisfy
a filtered branch. String branches retain their previous behavior. Do not require
direct addressing for a completion report that the agent should notice in a
shared conversation; the second example deliberately allows public updates.

Filtering runs before the provider request. If no branch has eligible new
evidence, no decision call or quota is consumed. `filtered_events` counts selected
events rejected by every branch of each watch; it is not a count of saved planner
calls. Eligible evidence still needs semantic evaluation and the usual authority
checks. Filters use separate shadow/live cursors, like other evidence.

Existing definitions, leases, cursors, expirations, and budgets load unchanged.
Adding metadata does not make an already-seen event new. Hosts must keep metadata
stable for an event and explicitly use `recheck` if correcting it. An in-flight
result is discarded if its metadata changed; no old result gains new authority.
Upgrade the helper before a host starts sending the optional metadata. Existing
string watches are not automatically rewritten into filters.

Alternatively, use one exact predicate instead of semantic conditions:

```json
{
  "label": "Required stock is present",
  "initial_check": "current_state",
  "predicate": {"event_id": "inventory", "path": "/wood", "op": "gte", "value": 12},
  "kinds": ["environment"],
  "ttl_seconds": 600
}
```

The JSON Pointer addresses the selected event's `content`. `eq` accepts scalar
strings, numbers and booleans; `gte` and `lte` compare numbers. Missing fields or
wrong types never match. Only host-observed `environment` facts can satisfy these
predicates; an agent's message claiming a number cannot substitute for them.
Exact evaluation makes no provider call and does not consume the semantic
quota. Its receipts identify `evaluator: predicate`. Hosts must still project
fresh facts from the authorized source and inspect a notification before acting.

`prepare` and `resolve` intentionally separate the policy/store from provider
transport. Use the existing `decide` command or `decisions::evaluate`; no additional
provider SDK or credential mechanism is introduced. The pure Rust module has no
Minecraft imports. The Minecraft bridge projects public game facts, exposes tools,
and uses the existing authorized task wake mechanism.

## Switching between observation and delivery

`observe_only` is a host-selected option on each poll, not an immutable property
of a watch or installation. Shadow evaluation keeps its own persistent evidence
history: repeated shadow polls do not produce duplicate receipts or model calls,
and they do not consume the evidence needed for a later live evaluation.
After switching to `observe_only: false`, unchanged evidence still present in the
host's input can trigger an armed watch. Exact predicates check it locally;
semantic watches request a fresh decision, subject to the existing cooldown and
hourly budget. A shadow result is never automatically promoted into a live wake.

For semantic evaluations, a lease prepared in observe-only mode remains
observe-only when resolved, even if the host changes mode while the call is in
flight. A live lease can also be resolved as observe-only. In either case, a
subsequent live poll may evaluate the same current evidence. Initial
`new_events` baselines, expiry, task authority, and the single-notification limit
are unchanged. `recheck` clears current evidence from both histories without
resetting expiry or the model-call budget.

This separation starts with `responsibility-watch-v6`. Existing stores still
load, but older versions recorded shadow and live observations in the same
history, so those earlier observations cannot be distinguished retroactively.
When promoting an older still-armed watch, use `recheck` with its current evidence
once after upgrading. This also discards any evaluation lease from the old policy.

## Lifecycle and limits

At most three outstanding watches, each lasting 30–1800 seconds and no longer than
the task deadline. Each watch produces at most one pending notification. A new
watch is explicit; a successful trigger never rearms itself. Completed, stopped,
changed, or exhausted responsibilities cancel outstanding watches.

The store uses a file lock and atomic replacement. Evaluation leases last 15
seconds; abandoned processes can retry after the lease. Calls have a five-second
minimum interval and a persisted 120/hour limit, including errors. Inputs are
bounded; stale results, malformed distributions, duplicate ids, and scope changes
are rejected. Provider or bridge failure cannot execute work or silently substitute
a broader attention policy.

The host delivers a selected `wake` only when it has at least 0.5 of the supplied
probability mass and agrees with the largest probability. This is
a heuristic for requesting inspection, not calibrated certainty or permission. A
notification carries the exact evidence and asks the planner to inspect it. It
does not interrupt an active model turn. At ordinary turn boundaries it may add
advice; during a valid explicit wait it may wake the existing task.

## Evaluation

The examples exercise completion, negation, wrong-actor reports, cancellation,
misleading quoted text and separate trigger alternatives. They are authored
fixtures; private live replays and agent messages are not distributed.

Measure every alternative independently, retain failures, and keep model-call
cost and actual task progress separate. A notification delivered during an
already scheduled turn is context, not proof of a saved planner call. Use fresh
held-out cases before claiming a quality improvement.
