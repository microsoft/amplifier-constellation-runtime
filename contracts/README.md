# Runtime agreements and interfaces

Start with the [vision](../docs/VISION.md) and local
[execution agreement](execution.md). The umbrella owns
[shared promises](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md),
including SDK use, model choice, continuity and work authority. This repository
owns the native implementation and its precise interfaces.

## Technical interfaces

| Boundary | Definition |
| --- | --- |
| ACP sessions, content, model/reasoning choices and optional capabilities | [ACP interface](../docs/acp-optional-capabilities.md) |
| Provider requests, accounts/routes and compatible history | [Providers](../docs/providers.md), [provider identity](../docs/provider-identity.md) |
| Configuration discovery and revision-checked changes | [Configuration](../docs/configuration.md) |
| Original history, model context and compaction | [Context](../docs/context.md) |
| In-turn input, receipts and recovery | [Steering](../docs/steering-design.md) |
| Workspace/MCP tools, execution and platform isolation | [Runtime boundaries](../docs/architecture.md), [platforms](../docs/platforms.md) |
| Provider media, hosted tools and structured output | [Native capabilities](../docs/native-capabilities.md) |
| Independently usable local helpers | [Decisions](../docs/decisions.md), [assessment](../docs/assessment.md), [watches](../docs/watches.md), [continuation](../docs/continuation.md) |
| Owner-enabled container commands | [Container commands](../docs/container-commands.md) |

[How the runtime works](../docs/runtime-overview.md) is an implementation walkthrough.
It adds no second agreement. Read technical detail for the boundary you will use
or change, then select its [validation](../docs/validation.md) and the affected
consumer check. Negotiated support, protocol fixtures and useful provider behavior
are separate evidence.
