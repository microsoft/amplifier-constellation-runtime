# Agent assessment experiment

The agent retains authorship of goals, plans, designs and collaboration. A fast
decision model supplies bounded advice about attention, approach and coordination.
This is metacognition, not a domain action-selection menu or a task-specific script.

`agent-runtime --home HOME assess BACKEND context.json` asks three independent
typed questions. Add `--dry-run` to inspect the exact questions without a paid call.
The configured decision backend handles transport, credentials and normalized
results, as for `decide`. The library entry point is `assessment::request`.

Input fields: `task_id`, `phase` (`running` or `waiting`), `goal`, optional
`working_summary`, `wait_for` (required when waiting), and `observations` containing
unique `id`, `source`, and `content`. Context is bounded to 32 KiB and 24 evidence
records. Source labels distinguish direct observations, tool receipts and reports.

Questions:

- **attention:** ready / wait / uncertain. Does new evidence deserve attention?
- **reflection:** reconsider / continue / uncertain. Is the current approach
  contradicted, repeatedly failing, or making no progress?
- **coordination:** request_help / update / none / uncertain. Is there an unmet
  communication need? Already delivered updates do not require acknowledgments.

All answers are advisory, not new facts, completion checks, instructions or
permission grants. The caller owns scheduling and verifies freshness. The API
does not send messages or operate tools. A normal planner determines the response,
including retaining a creative plan that the assessor cannot fully understand.
This module can be used alongside any ACP agent; it does not extend ACP itself.

## Experiment plan

1. Preserve the completed Minecraft inn as a baseline. Capture observations,
   choices, tool outcomes and collaboration, including failed approaches.
2. Test the same policy with Minecraft, software and team-planning cases. Reverse
   option ordering; record uncertainty and disagreement rather than hiding it.
   `examples/assessment/cases.json` is a small development set, not a quality claim.
3. Run in shadow mode, then supply advice at normal planner turn boundaries.
   Keep existing waiting-task wake authority, Stop, deadlines and budget checks.
4. Give the independent agents a broad creative brief. Let them inspect, propose,
   divide responsibility, create, negotiate and verify. Supply no blueprint,
   construction sequence or fixed design alternatives.
5. Compare failure repetition, useful versus duplicate communication, stale-state
   decisions, missed dependencies, planner calls, latency, tokens and visible
   outcomes. Judge creativity by the artifacts and human experience, not activity
   counts or assessor agreement. A before/after demonstration is not a controlled
   causal comparison; subsequent trials should vary only the assessment condition.
6. Expand only after these observations: relevance-driven memory retrieval,
   participation timing, commitment reconciliation, and model escalation. Then
   test agent-authored watches: an agent identifies what it needs to notice and
   proposes a bounded decision question for its own current responsibility. The
   runtime validates the contract/budget; observations still come from adapters.
   This lets the agent create its own useful fast judgments rather than receiving
   an experience-specific menu designed by the operator. Keep
   identity, access, commitments and attention general; keep geometry, inventory,
   game physics and world interaction in the Minecraft adapter.

Adapter work should improve faithful senses and reliable general game controls
(including container access and partial-block navigation), never solve a specific
building design for the agent. Dependency replacements require a reproduced
failure and a bounded fix, not a wholesale rewrite based on frustration.

## Limits

This first increment assesses at turn boundaries and during explicit waits. It
does not interrupt an active planner, supply continuous motor control, implement
cross-surface memory, or automatically judge artistic quality. A probabilistic
answer is not a calibrated guarantee. Keep source evidence for replay and retain
an explicit off/shadow mode for comparisons and rollback.

## Closing a responsibility

A waiting agent may need to wake to reconcile a completed or cancelled task,
even when no new implementation is needed. Readiness policies should distinguish
that case from continued waiting. The planner still verifies reports; a decision
model does not grant authority or establish completion.

The supplied cases are synthetic. Keep private task transcripts and model
responses outside the repository and evaluate new policies on held-out inputs.
