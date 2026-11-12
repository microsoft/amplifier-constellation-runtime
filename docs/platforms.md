# Native installation and execution

## Native executable packages

CI builds and runs the native checks on Linux x64/ARM64, Intel/Apple-silicon Mac,
Windows x64 and Windows 11 ARM64. Windows x64 CI uses Windows Server 2025;
that is not a Windows 11 desktop installation test. Published release assets,
their checksums and `PACKAGE.json` identify the actual available binary, source,
architecture and version. Windows packages include the C runtime; Mac binaries
record their deployment target. Linux packages currently target the Ubuntu 24.04
build environment. Packages are not signed or notarized, and CI does not establish
consumer first-launch behavior on every OS version.

Use a verified executable directly with an ACP client or through the independently
packaged Instance. Runtime does not need a compiler or another language runtime
for its own model/tool loop. Optional provider bridges and tool programs retain
their documented dependencies; installation owners must provide them deliberately.
The umbrella owns the [native setup commitment](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/installations.md#native-single-computer-setup).

The optional `scripts/install-instance.py` remains a developer assembly tool,
requiring Python and the Instance source setup machinery. It is not the consumer
installer. It creates a new, independent installation and registers the host's
user systemd service, LaunchAgent or Windows scheduled task only after engine
configuration is valid; `--no-start` leaves it stopped. It does not migrate or
replace an existing agent.

## Tool isolation

Workspace file tools and explicitly connected MCP tools are available on all three platforms. Local command execution is a distinct capability:

| Platform | Command boundary | Scope |
| --- | --- | --- |
| Linux | `/usr/bin/bwrap` | Private mount/process namespaces, network disabled by default or explicitly inherited, workspace writes, system binaries read-only, private temporary folder, clean environment |
| macOS | `/usr/bin/sandbox-exec` / Seatbelt | Default-deny profile, workspace/private scratch writes, system binaries read-only, no network or Launch Services, clean environment |
| Windows | AppContainer + Job Object | Unique per-command identity, no network capabilities, checked workspace/private scratch permissions, explicit handle/environment allowlist, owned process tree |

The command tool is advertised only when its platform implementation is enabled and any required helper exists. Failure to apply confinement fails the command. `check` reports the boundary. Commands have an output limit and deadline; cancellation ends their process group or Windows Job Object and descendants. Provider credentials, SSH agents, host home variables, and arbitrary inherited environment values are not passed to commands.

On macOS, the profile permits directory metadata needed to traverse paths and the root-directory read needed by dyld, but not arbitrary file contents. Some tools installed through Homebrew, Xcode, GUI applications, or user profiles require additional host paths/services and will fail. The runtime does not weaken the profile automatically. Apple's `sandbox-exec` manual marks the helper deprecated; this preview depends on its continued availability and is not an App Sandbox/notarization claim. Native execution was tested on macOS 27.2, including outside/symlink access, a real listening network socket, and descendant cancellation.

## Windows command scope

Version 0.6 enables the native executor by default. A source build with `--no-default-features` omits this capability; file tools and explicit MCP remain available. Native denial tests ran on Windows 11 x86_64, including an actual ACP allow/deny callback and read-only session mode. This is an unsigned preview, not a Windows security certification.

Each command gets a fresh AppContainer identity with no network capabilities. Windows' own shared OS permissions remain in effect; the runtime additionally grants that identity access only to the verified workspace and a private scratch folder. A suspended process joins a Job Object before it starts. The job prevents breakaway, limits descendants to 64 processes, and terminates them on completion, timeout, cancellation, or runtime handle closure. Only the three standard I/O handles are inherited. Preparation shares the command deadline and cannot start a process after cancellation has been acknowledged.

Workspaces must use ordinary local Unicode drive paths shorter than 240 characters. Preparation refuses reparse points, symlinks, junctions, multiply linked files, and trees over 50,000 entries. Executables must be in the workspace or Windows system directory; user-profile tool installations are not implicitly exposed. Invoke batch commands explicitly as `cmd.exe` with `/c` followed by one command string. AutoRun is disabled. These restrictions may require copying a self-contained tool into the workspace; the runtime does not broaden permissions automatically.

Temporary ACL entries use a unique per-command SID. Cleanup preserves unrelated entries and inheritance, visits verified new descendants, and never follows reparse points. Concurrent host edits can introduce entries that cannot be safely cleaned. A successful result includes `cleanup_incomplete` if any entries could not be verified; an interrupted/failed run reports the same issue to the runtime log. Safe siblings are still cleaned and the temporary AppContainer profile is removed. Original access, new-file cleanup, startup failure, and a concurrent host-created junction are covered by native checks.

Mobile devices remain clients of an installed agent; this package does not imply an unrestricted background server on iOS or Android.

On macOS, command arguments should use canonical workspace paths. An OS alias
such as `/var` for `/private/var` can be outside the command isolation grant even
when it identifies the same directory; use the session's canonical root.
