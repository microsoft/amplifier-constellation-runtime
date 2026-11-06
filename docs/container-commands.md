# Commands in an owner-enabled installation container

Workspace command isolation remains the default. An owner may instead use a
dedicated Linux installation container as the filesystem/network boundary:

```json
{"command_boundary": "container", "command_network": "inherit"}
```

Use the [Instance administrator image and installer](https://github.com/microsoft/amplifier-constellation-instance/tree/main/ops/container).
The runtime requires `/.dockerenv` and a bounded, read-only bind-mounted
`/etc/constellation-container.json` that enables this mode. A persona, environment
variable or missing bubblewrap binary cannot enable it. Invalid configuration
fails before a command runs. This mode is Linux-only and does not loosen the
default Windows or macOS command boundaries.

Commands start in a permitted session workspace but can use `sudo -n` to install
packages or administer the container. The enclosing installation, including its
mounted files and credentials, is accessible. Host files, Docker controls, other
installations and host networking must remain outside that installation. The
runtime trusts its owner-managed outer boundary; it does not turn a general
host process into an isolated container. Containers share the host kernel.

The command environment is cleared. HOME, PATH, locale and a short browser
socket directory are provided; only the two owner-supplied browser locations
`PLAYWRIGHT_BROWSERS_PATH` and `AGENT_BROWSER_EXECUTABLE_PATH` may be inherited.
This prevents accidental environment copying, not access to credentials already
mounted inside the same container. File helper tools retain their workspace
scope; use a command for authorized container administration.

Time limits, bounded output and cancellation remain. On termination the runtime
uses the image's noninteractive sudo to kill the command process group, including
root children. This requires the image's `Defaults:agent !use_pty` setting and
standard `sudo`, `timeout`, and `kill` executables. Daemons intentionally escaping
the command group are container-lifetime processes; stop the container to end
those. Do not interpret successful command cancellation as rollback of completed
package installations or other effects.

`config-schema`, `config-inspect`, `config-validate`, revision-checked `configure`,
and `capabilities` expose the same owner choice. `workspace_run` describes the
active boundary to the model so it can check actual command results instead of
assuming that package installation is unavailable.
