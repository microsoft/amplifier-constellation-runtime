# The native execution boundary

For ACP clients, hosts, tools and provider integrations using Constellation Runtime.
The promise: a standalone engine carries supported interactions through its own
model/tool loop and retains inspectable execution evidence.

**Example:** A client loads an existing session, adds a correction and receives
ordered response and tool events. A tool outcome retained before a disconnect
remains available on reload; replay displays the record rather than executing it.

## Local commitments

1. **Own the engine loop.** Runtime calls its configured provider directly and
   translates responses into ACP content, tool activity and terminals. Ordinary
   stdio ACP does not require Instance or another agent framework. Optional
   configuration, media, steering and helper behavior are advertised separately
   in the [ACP interface](../docs/acp-optional-capabilities.md).
2. **Keep one writer for a session.** A runtime home stores independent sessions.
   Session execution locks protect history and effects; supported independent
   sessions may run concurrently. Foreground execution belongs to its ACP
   connection. Saved history alone is not a background scheduler; see
   [execution and recovery](../docs/architecture.md#foreground-execution-and-recovery).
3. **Retain provider identity and evidence.** Session history records public
   messages, tool intents/results and private provider continuation separately.
   Model working windows and history queries use the supported
   [context interfaces](../docs/context.md). A model or provider change follows
   [provider identity](../docs/provider-identity.md) and the declared history
   restrictions; opaque state is not silently translated.
4. **Enforce the native tool boundary.** Workspace tools use their configured
   filesystem capability; commands use the selected platform isolation. MCP
   credentials and trust attach to exact configured endpoints. Trusted local
   client mode is a separate capability to start programs, not ordinary workspace
   permission. [Runtime boundaries](../docs/architecture.md) and
   [platforms](../docs/platforms.md) own the precise enforcement and limitations.
5. **Keep helpers independently usable.** Decision, assessment, watch and
   continuation commands expose their own typed inputs and outputs. They can be
   used without replacing an installation's chosen engine. Their advice does not
   create a second owner of domain execution.

The umbrella owns [composition](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/composition.md),
[interaction](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/interaction.md),
[work authority](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-authority.md),
[continuity](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/work-continuity.md),
[installations](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md)
and [engine compatibility](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/engine-compatibility.md).
The native implementation applies those promises; it does not define alternatives.

**Check:** Run an actual ACP client through session load, model/tool execution,
correction and disconnect. Inspect retained history and actual tool effects.
Exercise competing session writers, denied workspace/MCP operations and unsupported
provider/history combinations. Qualify the exact configured route separately from
protocol fixtures using [validation](../docs/validation.md).

**Left open:** Models/providers within declared support, product UX and optional
host services. Runtime neither owns platform conversation membership nor accepts
a domain task merely because a prompt or helper command completed.
