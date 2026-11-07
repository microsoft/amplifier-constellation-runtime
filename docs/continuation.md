# Bounded continuation advice

`agent-runtime continuation input.json` assesses trusted host facts about an
existing responsibility. It does not call a model, execute a command, replay an
action, renew a deadline, or grant permissions. It is an optional local policy
helper, separate from the ACP conversation contract.

## Explicit limits (helper v2)

Runtime 0.14 adds `agent-runtime helper-capabilities`, a static JSON query that
requires no `--home`, credentials or model. Check `schema_version: 1`, then
`helpers.continuation.version: 2`, `command: "continuation-v2"`, and support for
both `finite` and `no_configured_bound` in `bound_kinds`. This is local helper
discovery, not an ACP/Instance capability or proof of the agent's authority.

`agent-runtime continuation-v2 input.json` uses the same fields below, except
`remaining_ms` and `remaining_turns` must each be one of:

```json
{"kind": "finite", "value": 200000}
```

```json
{"kind": "no_configured_bound"}
```

The value is unsigned remaining milliseconds or turns, respectively. Zero is an
exhausted finite allowance. Missing, null, unknown, bare numeric and contradictory
values are rejected; they never mean unlimited work. Each dimension is explicit
and independent. The host must know that a task limit was deliberately left
unconfigured before using `no_configured_bound`; unknown provider capacity is
not such a fact. No sentinel numbers, replacement deadlines or old-task migration
are needed. See the executable [v2 fixture](../examples/helpers/continuation-v2.json).

V2 returns `bounded-continuation/2`. Its task-status, observation, effect-receipt
and recovery checks are unchanged, including `extend_budget: false` and
`replay_actions: false`. A finite exhausted dimension still produces `checkpoint`.
No configured task bound does not remove Stop, installation/provider/tool limits
or the resource owner's action leases. If discovery is absent, use the original
helper only for genuinely finite tasks; report the unsupported combination rather
than inventing a limit or sending an unsupported input.

## Original numeric input (helper v1)

`agent-runtime continuation input.json` retains its original input and output:

```json
{
  "status": "needs_review",
  "remaining_ms": 200000,
  "remaining_turns": 4,
  "minimum_turn_ms": 120000,
  "idle": true,
  "observation_current": true,
  "receipts_complete": true,
  "actions_succeeded": true,
  "recovery_attempts": 0
}
```

The host must derive these fields from durable action receipts and fresh
observations, not an agent's prose or an incoming peer message. In particular,
missing receipts and partial mutations are not successful outcomes.

The result has policy `bounded-continuation/1`, action and reason. Terminal or
blocked responsibilities produce `hold`. Insufficient remaining time or turns
produce `checkpoint`. A busy host or stale observation produces `hold`. Waiting
tasks continue to wait for evidence. An interrupted task with complete,
successful receipts and fewer than two recoveries produces
`inspect_then_continue`; ambiguous outcomes produce `review`. Running work with
enough budget produces `continue`.

The host enforces task identity, Stop, generation changes, original deadline,
turn limit and recovery count again before applying advice. Continue by observing
the current environment; never replay the interrupted prompt or its actions.
The host retains reconciliation evidence and reports unresolved cases for review.
The minimum duration describes a useful turn in that experience; it is bounded
to one second through ten minutes.

Minecraft uses this policy for an interrupted creative build and projects game
action receipts. Other hosts can project subprocess, document, or service
receipts without importing any game concepts. An ACP-only agent needs no changes
to use tools exposed by such a host.
