# A dependable engine for continuing work

Constellation Runtime turns a request into model reasoning, permitted tool use and
a saved account of what happened. People should be able to entrust useful work to
an agent, correct its course and understand its results without supervising every
model call.

It is one engine within the [Constellation ecosystem](https://github.com/microsoft/amplifier-constellation/blob/main/docs/VISION.md).
Compatible clients can use it directly. An optional Instance supplies installed
identity and management; experiences and connectors bring conversations and domain
capabilities. Runtime owns the reasoning and execution inside that composition.

## Carry useful work through change

Imagine an agent preparing a briefing. A correction arrives while it gathers
sources. The engine should incorporate that direction, retain completed work and
continue through the tools and model route its owner selected. If execution stops,
its record should help establish what can continue safely.

Original evidence and a model's working context serve different purposes. Context
handling should make long work economical and coherent without replacing the
record of what actually happened. Provider integration should make the owner's
choices useful without requiring each experience to implement another model loop.

## Improve the whole outcome

Reasoning quality, tools, provider integration, context and recovery all offer room
for independent improvement behind dependable interfaces. Measure the result and
human effort alongside latency, resource use and failure behavior. A quicker
response or smaller prompt is useful only when it supports the work.

The [execution agreement](../contracts/execution.md) states this engine's local
boundary. The umbrella owns [shared promises](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md).
Technical interfaces remain available for contributors deepening a capability;
they do not prescribe the product experience built around the engine.
