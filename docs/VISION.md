# A dependable engine for continuing work

Constellation Runtime supplies model reasoning, permitted tool use and retained
execution evidence so people can entrust useful work to agents without managing
every model call. Corrections should reach ongoing work; the record should make
its results and recovery understandable.

Runtime is a reusable implementation behind the umbrella's
[Agent Instance](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/agent-instance.md)
boundary, not a required internal architecture for all agents. Compatible ACP
clients can use it directly. The optional Constellation Instance host supplies
installed identity and management; connections and experiences bring source-owned
conversations and domain capabilities. A saved execution session is not itself
the conversation people share.

## Carry useful work through change

An agent preparing a briefing receives a correction while gathering sources.
The engine incorporates supported input, retains completed work and continues
with the configured tools and model route. If execution stops, retained evidence
helps establish what can continue safely.

Original evidence and a model's working context serve different purposes. Context
handling should keep long work economical and coherent without replacing its
record. Provider integration should make configured choices useful without making
each experience implement another model loop.

## Offer useful parts independently

The engine's decision, assessment, watch and continuation helpers expose their own
[Tool](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/tool.md)
interfaces. Other agents can use them without adopting this runtime. The provider
owns the helper operation; its caller keeps the larger responsibility.

Improve reasoning, tools, context and recovery behind their declared interfaces.
Judge useful outcomes and human effort alongside latency, resource use and failure
behavior. The [execution agreement](../contracts/execution.md) defines this engine's
contribution; [shared promises](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md)
remain in the umbrella. Product experiences and other agent architectures stay open.
