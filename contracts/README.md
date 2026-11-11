# Runtime contract map

The [Runtime vision](../docs/VISION.md) describes intended value. The umbrella's
[family agreement](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/family.v1-candidate.md)
owns shared responsibilities, information ownership and interface meanings.
Detailed native commitments stay in their existing documents below; this map
does not create another specification.

| If your change affects… | Read the local contract |
| --- | --- |
| ACP sessions, model/reasoning choices, roots, content or optional capabilities | [ACP capabilities](../docs/acp-optional-capabilities.md) |
| Provider request semantics, account/endpoint binding or model compatibility | [Providers](../docs/providers.md) and [provider identity](../docs/provider-identity.md) |
| Installation configuration, discovery or atomic editing | [Configuration](../docs/configuration.md) |
| Original history, compaction, model views or history queries | [Context](../docs/context.md) |
| Input arriving during work, durable receipts or uncertain input recovery | [Steering](../docs/steering-design.md) |
| Workspace/MCP tools, credentials, tool intent or execution recovery | [Runtime boundaries](../docs/architecture.md) and [platforms](../docs/platforms.md) |
| Provider media, hosted tools or structured output | [Native capabilities](../docs/native-capabilities.md) |
| Typed decision or assessment helpers | [Decisions](../docs/decisions.md) and [assessment](../docs/assessment.md) |
| Task-condition watches or receipt-based continuation advice | [Watches](../docs/watches.md) and [continuation](../docs/continuation.md) |
| Owner-enabled container commands | [Container commands](../docs/container-commands.md) |

ACP and MCP retain their upstream meanings. Family-wide requirements for SDKs,
input continuity, public delivery, task authority and lifecycle belong in the
[umbrella contract map](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md).
Reference them when changing an integration; do not copy them into a parallel
native definition.

[How the runtime works](../docs/runtime-overview.md) explains the design. It adds
no commitments beyond these contracts. When a promise changes, review the linked
examples in the same change. Check the actual implementation and applicable
[validation paths](../docs/validation.md); this map is not a release or gap view.
