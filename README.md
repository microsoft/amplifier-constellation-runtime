# Constellation Runtime

A reusable reasoning and execution engine for persistent agents. This standalone
Rust implementation runs its own model/tool loop, preserves execution sessions,
and connects through Agent Client Protocol (ACP). It calls configured model
providers directly; another agent framework is not required.

Runtime contributes to the [Constellation vision](https://github.com/microsoft/amplifier-constellation/blob/main/docs/VISION.md).
The umbrella owns [shared promises](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/README.md);
this repository owns the native execution boundary and technical interfaces.

## Find the right starting point

| Read | For |
| --- | --- |
| [Runtime vision](docs/VISION.md) | Intended value and the contribution this engine makes. |
| [Contract map](contracts/README.md) | The local interface or behavior your change must preserve. |
| [How the runtime works](docs/runtime-overview.md) | An illustrative walk through the model/tool loop and saved state. |
| [Provider setup](docs/providers.md) and [configuration](docs/configuration.md) | Connect a model, inspect configuration and make a supported change. |
| [Validation](docs/validation.md) | Check the relevant source, protocol, provider and platform behavior. |

## Start one agent

Build with Rust 1.88 or later:

```sh
cargo build --release --locked
./target/release/agent-runtime --home "$PWD/instance" init \
  --workspace "$PWD/workspace" --name "My agent" --model gpt-6.1-sol
```

Supply `OPENAI_API_KEY` privately in the process environment. Configuration stores
its variable name, never the value. The model must be available to that account.
Configure an ACP client to run:

```sh
/absolute/path/to/agent-runtime --home /absolute/path/to/instance acp
```

Use the configured workspace as the client's session working folder. On Windows,
the executable is `agent-runtime.exe`. The private runtime home must not be inside
the exposed workspace. `check`, `sessions` and `events SESSION_ID` support local
inspection; configuration is in `runtime.json` under the home.

`agent-runtime providers` lists provider presets. `init` and
`configure-provider` use the same provider options; see [provider setup](docs/providers.md)
for endpoint, account and local-server behavior. Per-session model and reasoning
choices use the [ACP capability contract](docs/acp-optional-capabilities.md#session-model-and-reasoning-choices).
A supported same-route choice is different from changing provider/account
boundaries; existing private histories are not silently translated.

## Compose the capabilities you need

Clients can use the runtime directly over stdio. The independent
[Instance](https://github.com/microsoft/amplifier-constellation-instance) service
can supply installed identity, authenticated remote access and management.
[Platform setup](docs/platforms.md) covers the optional managed installation.
Experiences and domain connectors stay independently owned.

The [technical interface map](contracts/README.md#technical-interfaces) locates
sessions, providers, tools, context, configuration and optional helpers without
making every capability prerequisite reading. The [execution agreement](contracts/execution.md)
identifies the native engine's boundary.

## Develop and verify

Use [Converge](https://github.com/microsoft/amplifier-converge) and the
[execution agreement](contracts/execution.md) to choose and coordinate the next
useful outcome.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The [validation guide](docs/validation.md) covers additional real ACP subprocess,
local provider/MCP fixture and platform checks. Source fixtures do not establish
live provider quality. A turn ending, a tool result and the user's desired outcome
remain separate observations.

## Contributing

> [!NOTE]
> This project is not currently accepting external contributions, but we're actively working toward opening this up. We value community input and look forward to collaborating in the future. For now, feel free to fork and experiment!

Most contributions require you to agree to a
Contributor License Agreement (CLA) declaring that you have the right to, and actually do, grant us
the rights to use your contribution. For details, visit [Contributor License Agreements](https://cla.opensource.microsoft.com).

When you submit a pull request, a CLA bot will automatically determine whether you need to provide
a CLA and decorate the PR appropriately (e.g., status check, comment). Simply follow the instructions
provided by the bot. You will only need to do this once across all repos using our CLA.

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
For more information see the [Code of Conduct FAQ](https://opensource.microsoft.com/codeofconduct/faq/) or
contact [opencode@microsoft.com](mailto:opencode@microsoft.com) with any additional questions or comments.

## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.
