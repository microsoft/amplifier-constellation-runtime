# Agent-authored attention watches

An agent defines what it needs to notice while pursuing an existing responsibility.
The runtime filters selected new evidence with a fast decision model, retaining a
notification for the agent to inspect. It never supplies a creative plan, executes
an experience action, marks work complete, or grants new authority.

This is a local reusable capability, not an extension to the ACP specification.
An ACP host can expose it through MCP to any compatible agent implementation.
The first host is Minecraft; the policy also has software and workshop fixtures.

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
Existing evidence is baselined when arming: a watch concerns new developments.

Operations:

- `arm`: validate the agent's condition, select evidence kinds, establish baseline.
- `list`: inspect watches, notifications, expiry, and hourly calls.
- `cancel`: cancel a watch by `id`.
- `prepare`: if selected evidence changed, reserve a bounded evaluation and return
  its `token` and ordinary typed decision `request`; otherwise return a skip reason.
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

`prepare` and `resolve` intentionally separate the policy/store from provider
transport. Use the existing `decide` command or `decisions::evaluate`; no additional
provider SDK or credential mechanism is introduced. The pure Rust module has no
Minecraft imports. The Minecraft bridge projects public game facts, exposes tools,
and uses the existing authorized task wake mechanism.

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
