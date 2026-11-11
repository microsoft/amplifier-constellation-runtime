# A dependable engine for persistent work

Constellation Runtime should let people and experiences entrust useful work to an
agent without rebuilding reasoning, tool execution and recovery for each new
connection. Its contribution is an engine that can think with the models its owner
chooses, use authorized tools, retain what happened and continue coherently as
input and circumstances change.

The engine is one part of the [Constellation ambition](https://github.com/microsoft/amplifier-constellation/blob/main/docs/VISION.md).
The [family agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/family.v1-candidate.md)
defines how it fits with installed identity, conversation services, experiences
and domains. This vision focuses on the value of the native implementation.

## Useful work that remains understandable

An agent should be able to carry a substantial assignment through model calls,
tools, new information and interruptions while preserving a faithful record.
People should spend their attention on intent and consequential choices, with
enough evidence to understand results and intervene when needed. Improving the
engine means improving that whole outcome, not just producing more tokens or
making a single response faster.

Original evidence and a model's working context serve different purposes. The
engine should make context useful and economical without treating a summary as
a replacement for what actually happened. A corrected request should inform the
remaining work. Recovery should help establish what can safely continue rather
than make uncertain actions look successful.

## Room for deep independent improvement

Provider integration, model working context, reasoning and tool execution,
session continuity and execution quality are substantial areas of investment.
The early implementation does not bound their future depth. Owners should be
able to improve them while clients and domains rely on declared interfaces.
The [local contract map](../contracts/README.md) locates those commitments.

Standard ACP makes this engine useful to independent clients. Optional native
capabilities can expose richer behavior without making every consumer adopt the
same host, model provider or product experience. Future interfaces may present
the same work very differently; the engine supplies dependable execution and
observations rather than prescribing their UX.

## How to judge an improvement

Exercise a meaningful piece of work through its actual client and tools. Observe
the result, correction behavior, permissions, context growth, latency and
recovery together. Distinguish tool observations from independently checked
effects and requester acceptance. A local fixture can expose a defect; it cannot
establish every model's quality or every installation's behavior.

Choose the next useful work by comparing this intent and the relevant contracts
with current evidence. Use the shared [Converge practice](https://github.com/microsoft/amplifier-constellation/blob/main/docs/working-together.md)
to coordinate changed promises and carry decisions into active work. Keep the
implementation and its explanations simpler as understanding improves.
