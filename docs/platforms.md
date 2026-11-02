# Native installation and execution

The Rust engine builds natively for Linux, macOS, and Windows. The optional `scripts/install-instance.py` installer uses the Agent Instance host's platform installer: a user systemd service on Linux, a launch agent on macOS, and a user scheduled task on Windows. The host is configured before its service starts. `--no-start` leaves the installation stopped for inspection. Each installation pins its own version-and-hash engine binary and keeps its own workspace, configuration, credentials, and history.

Build both repositories on the target OS, then invoke the same Python installer shown in the README. Use `python` on Windows and supply `.exe` paths for both binaries. Rust/Cargo, a C compiler/toolchain for the chosen target, Python 3, and the host repository's documented prerequisites must already be installed. This developer installer does not install global build tools or claim to be a signed end-user package. Windows user-task registration and macOS LaunchAgent registration are provided by the host's native installer; their creation is deferred until the engine configuration is valid.

Workspace file tools and explicitly connected MCP tools are available on all three platforms. Local command execution is a distinct capability:

| Platform | Command boundary | Scope |
| --- | --- | --- |
| Linux | `/usr/bin/bwrap` | Private mount/process/network namespaces, workspace writes, system binaries read-only, private temporary folder, clean environment |
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
