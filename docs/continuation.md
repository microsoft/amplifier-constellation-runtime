# Bounded continuation advice

`agent-runtime continuation input.json` assesses trusted host facts about an
existing responsibility. It does not call a model, execute a command, replay an
action, renew a deadline, or grant permissions. It is an optional local policy
helper, separate from the ACP conversation contract.

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
