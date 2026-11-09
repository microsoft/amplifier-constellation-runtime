use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use cap_std::fs::{Dir, OpenOptions};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
#[cfg(not(windows))]
use std::{process::Stdio, time::Duration};
#[cfg(not(windows))]
use tokio::io::AsyncReadExt;

pub fn definition(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({"type":"function","name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"strict":false})
}
pub fn definitions() -> Vec<Value> {
    definitions_for_network(crate::config::CommandNetwork::Isolated)
}
pub fn definitions_for_network(network: crate::config::CommandNetwork) -> Vec<Value> {
    definitions_for_boundary(network, crate::config::CommandBoundary::Workspace)
}
pub fn definitions_for_config(config: &Config) -> Vec<Value> {
    definitions_for_boundary(config.command_network, config.command_boundary)
}
fn definitions_for_boundary(
    network: crate::config::CommandNetwork,
    boundary: crate::config::CommandBoundary,
) -> Vec<Value> {
    let mut result = vec![
        definition("clock", "Read the current UTC time.", json!({}), vec![]),
        definition(
            "workspace_list",
            "List files in the agent's workspace. Paths are relative to the current working folder. Cannot access runtime state, secrets, or other installations.",
            json!({"path":{"type":"string"}}),
            vec!["path"],
        ),
        definition(
            "workspace_read",
            "Read a UTF-8 file in the agent's workspace.",
            json!({"path":{"type":"string"}}),
            vec!["path"],
        ),
        definition(
            "workspace_view_image",
            "Inspect a saved PNG, JPEG, GIF or WebP image inside the granted workspace. Returns visual evidence to you; does not attach or publish it to the conversation.",
            json!({"path":{"type":"string"}}),
            vec!["path"],
        ),
        definition(
            "workspace_attach_image",
            "Attach a saved PNG, JPEG, GIF or WebP image from the granted workspace to your current conversation as ACP response output. Use when the user requests sharing/showing that image. Available in every session mode; it reads the file without changing it. This sends the bytes to this client; it does not select or post to another channel. Maximum 2 MiB.",
            json!({"path":{"type":"string"}}),
            vec!["path"],
        ),
        definition(
            "workspace_write",
            "Write a UTF-8 file in the workspace. Requires the session's write permission. Parent folders are created as needed.",
            json!({"path":{"type":"string"},"content":{"type":"string"}}),
            vec!["path", "content"],
        ),
    ];
    let container =
        boundary == crate::config::CommandBoundary::Container && container_boundary_available();
    if container || command_isolation().is_some() {
        let description = if container {
            "Run a bounded command inside this agent's installation container. You can install packages and administer the container with sudo -n; the container filesystem, its mounted installation files, and its network policy are the boundary. This does not grant host Docker, host files, or other agents' installations. Use actual tool results to check capabilities. Provider environment variables are not inherited. Returns actual exit code and bounded stdout/stderr. Requires write permission."
        } else {
            match network {
                crate::config::CommandNetwork::Isolated => {
                    "Run a bounded local command in the agent's workspace. Workspace and system binaries only; networking disabled. No host home or provider credentials. Returns actual exit code and bounded stdout/stderr. Requires write permission."
                }
                crate::config::CommandNetwork::Inherit => {
                    "Run a bounded local command in the agent's workspace. Networking uses the enclosing installation's network and firewall, so repository checkout and dependency downloads can be attempted. Workspace filesystem boundaries remain enforced; no host home or provider credentials. Returns actual exit code and bounded stdout/stderr. Requires write permission. Diagnose a failed connection before declaring all networking unavailable."
                }
            }
        };
        result.push(definition(
            "workspace_run",
            description,
            json!({"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),
            vec!["command", "args"],
        ));
    }
    result
}
pub fn container_boundary_available() -> bool {
    if !cfg!(target_os = "linux") || !Path::new("/.dockerenv").is_file() {
        return false;
    }
    let marker = "/etc/constellation-container.json";
    let Ok(metadata) = std::fs::metadata(marker) else {
        return false;
    };
    if metadata.len() > 16 * 1024 {
        return false;
    }
    let Ok(data) = std::fs::read(marker) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&data) else {
        return false;
    };
    let mounted_read_only = std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mounts| {
        mounts.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() > 6 && fields[4] == marker && fields[5].split(',').any(|v| v == "ro")
        })
    });
    mounted_read_only
        && value["container_admin"] == true
        && value["command_boundary"] == "container"
}
pub fn command_isolation_for(config: &Config) -> Option<&'static str> {
    if config.command_boundary == crate::config::CommandBoundary::Container {
        return container_boundary_available().then_some("installation-container");
    }
    command_isolation()
}
pub fn command_isolation() -> Option<&'static str> {
    if cfg!(all(windows, feature = "windows-appcontainer")) {
        return Some("windows-appcontainer");
    }
    if cfg!(target_os = "linux") && Path::new("/usr/bin/bwrap").is_file() {
        Some("bubblewrap")
    } else if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file() {
        Some("macos-seatbelt")
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_profile(roots: &[PathBuf], scratch: &Path) -> Result<String> {
    let literal = |path: &Path| -> Result<String> {
        let value = path.to_str().context("Command paths must be UTF-8")?;
        ensure!(!value.chars().any(char::is_control), "Invalid command path");
        Ok(format!(
            "\"{}\"",
            value.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    };
    // No broad Mach/Launch Services access: those can start an unconfined
    // application. Reading the filesystem root itself is needed by dyld on
    // macOS 27; it does not grant reads of its descendants.
    let mut grants = roots
        .iter()
        .map(|root| Ok(format!("(subpath {})", literal(root)?)))
        .collect::<Result<Vec<_>>>()?;
    grants.push(format!("(subpath {})", literal(scratch)?));
    Ok(format!(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow process-info* (target same-sandbox))
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow file-read-metadata (vnode-type DIRECTORY))
(allow file-read* (literal "/") (subpath "/System") (subpath "/usr") (subpath "/bin") (subpath "/sbin") (subpath "/Library/Apple") (literal "/private/var/select/sh"))
(allow file-read* file-write* {})
(allow file-read* file-write* (literal "/dev/null"))
(allow file-read* (literal "/dev/urandom") (literal "/dev/random"))
"#,
        grants.join(" ")
    ))
}
pub fn mutation(name: &str) -> bool {
    // Explicit attachments are response output from already granted roots.
    // Viewing an image or receiving one from MCP never publishes it implicitly.
    matches!(
        name,
        "workspace_write" | "workspace_run" | "workspace_save_artifact"
    )
}
fn relative(c: &Config, cwd: &Path, input: &str) -> Result<(PathBuf, PathBuf)> {
    let roots = c.effective_roots()?;
    let path = Path::new(input);
    ensure!(
        !path.components().any(|v| matches!(v, Component::ParentDir)),
        "Parent traversal is not allowed"
    );
    // The configured spelling can differ from the canonical root (macOS
    // /var -> /private/var, Windows verbatim paths). Resolve the existing cwd,
    // but leave the requested leaf to cap-std so a new file can be created and
    // symlinks remain confined by the open directory capability.
    let mut p = if path.is_absolute() {
        path.to_owned()
    } else {
        std::fs::canonicalize(cwd)?.join(path)
    };
    if path.is_absolute() && !roots.iter().any(|root| p.starts_with(root)) {
        // Normalize OS aliases through an existing parent. The final leaf
        // stays unresolved so a write cannot follow a target symlink.
        let mut ancestor = p.as_path();
        let mut suffix = Vec::new();
        while let Some(name) = ancestor.file_name() {
            suffix.push(name.to_owned());
            ancestor = ancestor.parent().context("Invalid absolute path")?;
            if let Ok(mut resolved) = std::fs::canonicalize(ancestor) {
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                p = resolved;
                break;
            }
        }
    }
    let (root, rel) = roots
        .iter()
        .filter_map(|root| p.strip_prefix(root).ok().map(|rel| (root, rel)))
        .max_by_key(|(root, _)| root.components().count())
        .or_else(|| {
            if c.session_roots.is_none() {
                p.strip_prefix(&c.workspace)
                    .ok()
                    .map(|rel| (&roots[0], rel))
            } else {
                None
            }
        })
        .context("Path is outside this session's granted workspaces")?;
    Ok((
        root.to_owned(),
        if rel.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            rel.to_owned()
        },
    ))
}
pub async fn call(c: &Config, cwd: &Path, name: &str, args: &Value) -> Result<Value> {
    if name == "clock" {
        return Ok(json!({"utc":crate::store::now()}));
    }
    if name == "workspace_run" {
        #[cfg(windows)]
        return crate::windows_command::run(c, cwd, args).await;
        #[cfg(not(windows))]
        return run(c, cwd, args).await;
    }
    let (root, p) = relative(c, cwd, args["path"].as_str().context("Missing path")?)?;
    let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    match name {
        "workspace_view_image" | "workspace_attach_image" => {
            use base64::Engine;
            use std::io::Read;
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            let f = root.open_with(p, &options)?;
            ensure!(
                f.metadata()?.is_file(),
                "Only regular image files can be read"
            );
            const MAX_IMAGE: u64 = 2 * 1024 * 1024;
            ensure!(
                f.metadata()?.len() <= MAX_IMAGE,
                "Image exceeds 2 MiB limit"
            );
            let mut bytes = Vec::new();
            f.take(MAX_IMAGE + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= MAX_IMAGE, "Image exceeds 2 MiB limit");
            let mime = crate::media::image_mime(&bytes)?;
            let image = json!({"type":"image","mimeType":mime,"data":base64::engine::general_purpose::STANDARD.encode(&bytes)});
            let key = if name == "workspace_attach_image" {
                "_acp_content"
            } else {
                "_tool_images"
            };
            Ok(
                json!({"path":args["path"],"bytes":bytes.len(),"mime_type":mime,
                "attached_to_current_conversation":name == "workspace_attach_image", key:[image]}),
            )
        }
        "workspace_list" => {
            let entries=root.open_dir(p)?.entries()?.take(501).map(|e|e.map(|e|json!({"name":e.file_name().to_string_lossy(),"directory":e.file_type().is_ok_and(|t|t.is_dir())}))).collect::<Result<Vec<_>,_>>()?;
            Ok(
                json!({"truncated":entries.len()>500,"entries":entries.into_iter().take(500).collect::<Vec<_>>()}),
            )
        }
        "workspace_read" => {
            use std::io::Read;
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            let f = root.open_with(p, &options)?;
            ensure!(f.metadata()?.is_file(), "Only regular files can be read");
            ensure!(
                f.metadata()?.len() <= c.limits.tool_output_bytes as u64,
                "File exceeds read budget"
            );
            let mut content = String::new();
            f.take(c.limits.tool_output_bytes as u64 + 1)
                .read_to_string(&mut content)?;
            ensure!(
                content.len() <= c.limits.tool_output_bytes,
                "File exceeds read budget"
            );
            Ok(json!({"content":content}))
        }
        "workspace_write" | "workspace_save_artifact" => {
            use base64::Engine;
            use std::io::Write;
            let bytes = if name == "workspace_save_artifact" {
                let data = args["data"].as_str().context("Missing artifact data")?;
                ensure!(data.len() <= crate::media::MAX_MEDIA, "Artifact too large");
                base64::engine::general_purpose::STANDARD.decode(data)?
            } else {
                let text = args["content"].as_str().context("Missing content")?;
                ensure!(text.len() <= 1024 * 1024, "File exceeds write budget");
                text.as_bytes().to_vec()
            };
            if let Some(parent) = p.parent() {
                root.create_dir_all(parent)?;
            }
            // Inspect without following a target symlink, then replace by rename.
            // A fresh inode prevents writes through Windows hard links too.
            #[cfg(unix)]
            let mut mode = 0o600;
            match root.symlink_metadata(&p) {
                Ok(meta) => {
                    ensure!(meta.is_file(), "Only regular files can be written");
                    #[cfg(unix)]
                    {
                        use cap_std::fs::MetadataExt;
                        ensure!(
                            meta.nlink() == 1,
                            "Refusing to modify a multiply linked file"
                        );
                        mode = meta.mode() & 0o777;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let temporary = p.with_file_name(format!(".agent-write-{}", uuid::Uuid::new_v4()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let result = (|| -> Result<()> {
                let mut file = root.open_with(&temporary, &options)?;
                file.write_all(&bytes)?;
                #[cfg(unix)]
                {
                    use cap_std::fs::PermissionsExt;
                    file.set_permissions(cap_std::fs::Permissions::from_mode(mode))?;
                }
                file.sync_all()?;
                drop(file);
                root.rename(&temporary, &root, &p)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = root.remove_file(&temporary);
            }
            result?;
            Ok(json!({"written_bytes":bytes.len()}))
        }
        _ => bail!("Tool is not available"),
    }
}
#[cfg(not(windows))]
struct ProcessGroup {
    id: u32,
    container_admin: bool,
}
#[cfg(not(windows))]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        let _ = self.id;
        #[cfg(target_os = "linux")]
        if self.container_admin {
            // Container-local sudo children can change uid. The unprivileged
            // runtime cannot kill those itself. Reap this command's group with
            // the installation's sudo, bounded even if sudo is misconfigured.
            // The admin image disables sudo's extra PTY/process group.
            let _ = std::process::Command::new("/usr/bin/timeout")
                .args([
                    "--kill-after=1",
                    "1",
                    "/usr/bin/sudo",
                    "-n",
                    "/usr/bin/kill",
                    "-KILL",
                    "--",
                ])
                .arg(format!("-{}", self.id))
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(target_os = "linux"))]
        let _ = self.container_admin;
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.id as i32), libc::SIGKILL);
        }
    }
}
#[cfg(not(windows))]
async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut r: R,
    limit: usize,
) -> Result<(String, bool)> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut b = [0u8; 4096];
    loop {
        let n = r.read(&mut b).await?;
        if n == 0 {
            break;
        }
        let take = (limit - output.len()).min(n);
        output.extend_from_slice(&b[..take]);
        truncated |= take < n;
    }
    Ok((String::from_utf8_lossy(&output).into_owned(), truncated))
}
#[cfg(not(windows))]
#[cfg(not(target_os = "macos"))]
fn isolated_account() -> Result<tempfile::TempDir> {
    let account = tempfile::Builder::new()
        .prefix("agent-command-account-")
        .tempdir()?;
    // Supply only the mapped command identity, never the host account database.
    // These lookups let ordinary build/test tools resolve their current user.
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    std::fs::write(
        account.path().join("passwd"),
        format!("agent:x:{uid}:{gid}:Isolated agent:/tmp:/bin/sh\n"),
    )?;
    std::fs::write(account.path().join("group"), format!("agent:x:{gid}:\n"))?;
    Ok(account)
}
#[cfg(not(windows))]
async fn run(c: &Config, cwd: &Path, args: &Value) -> Result<Value> {
    anyhow::ensure!(
        c.command_network == crate::config::CommandNetwork::Isolated || cfg!(target_os = "linux"),
        "Inherited command networking is currently supported on Linux only"
    );
    ensure!(
        command_isolation_for(c).is_some(),
        "Isolated commands require Linux bubblewrap or macOS Seatbelt; no unrestricted fallback"
    );
    let command = args["command"].as_str().context("Missing command")?;
    let arguments: Vec<String> = serde_json::from_value(args["args"].clone())?;
    ensure!(
        !command.is_empty()
            && command.len() <= 4096
            && arguments.len() <= 128
            && arguments.iter().all(|s| s.len() <= 16384),
        "Command exceeds bounds"
    );
    let roots = c.effective_roots()?;
    let cwd = std::fs::canonicalize(cwd)?;
    ensure!(
        roots.iter().any(|root| cwd.starts_with(root)),
        "Command folder is outside workspace"
    );
    #[cfg(target_os = "macos")]
    let scratch = tempfile::Builder::new()
        .prefix("agent-command-")
        .tempdir()?;
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let scratch = std::fs::canonicalize(scratch.path())?;
        let mut cmd = tokio::process::Command::new("/usr/bin/sandbox-exec");
        cmd.env_clear()
            .env("HOME", &scratch)
            .env("TMPDIR", &scratch)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .arg("-p")
            .arg(macos_profile(&roots, &scratch)?)
            .current_dir(&cwd);
        cmd
    };
    #[cfg(not(target_os = "macos"))]
    let account = isolated_account()?;
    #[cfg(not(target_os = "macos"))]
    let mut cmd = tokio::process::Command::new(
        if c.command_boundary == crate::config::CommandBoundary::Container {
            command
        } else {
            "/usr/bin/bwrap"
        },
    );
    #[cfg(not(target_os = "macos"))]
    {
        if c.command_boundary == crate::config::CommandBoundary::Container {
            ensure!(
                container_boundary_available()
                    && c.command_network == crate::config::CommandNetwork::Inherit,
                "Container command boundary is unavailable"
            );
            cmd.env_clear()
                .env("HOME", "/home/agent")
                .env(
                    "PATH",
                    "/home/agent/.local/bin:/usr/local/bin:/usr/bin:/bin",
                )
                .env("LANG", "C.UTF-8")
                .env("AGENT_BROWSER_SOCKET_DIR", "/tmp/ab")
                .current_dir(&cwd);
            for name in ["PLAYWRIGHT_BROWSERS_PATH", "AGENT_BROWSER_EXECUTABLE_PATH"] {
                if let Ok(value) = std::env::var(name) {
                    cmd.env(name, value);
                }
            }
        } else {
            cmd.env_clear().args([
                "--unshare-all",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--ro-bind",
                "/usr",
                "/usr",
            ]);
            for name in ["bin", "sbin", "lib", "lib64"] {
                let p = PathBuf::from(format!("/{name}"));
                if p.exists() {
                    if p.is_symlink() {
                        cmd.arg("--symlink").arg(std::fs::read_link(&p)?).arg(&p);
                    } else {
                        cmd.arg("--ro-bind").arg(&p).arg(&p);
                    }
                }
            }
            cmd.args([
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--setenv",
                "HOME",
                "/tmp",
                "--setenv",
                "PATH",
                "/usr/local/bin:/usr/bin:/bin",
                "--setenv",
                "LANG",
                "C.UTF-8",
            ]);
            for name in ["passwd", "group"] {
                cmd.arg("--ro-bind")
                    .arg(account.path().join(name))
                    .arg(format!("/etc/{name}"));
            }
            // Debian-family toolchains use these aliases from /usr/bin. Preserve
            // only aliases resolving into the already exposed system tool tree;
            // never mount the entire /etc directory or arbitrary alternatives.
            for name in ["cc", "c++", "cpp", "awk", "pkg-config"] {
                let alias = PathBuf::from(format!("/etc/alternatives/{name}"));
                if let Ok(target) = std::fs::canonicalize(&alias)
                    && target.starts_with("/usr")
                    && target.is_file()
                {
                    cmd.arg("--symlink").arg(target).arg(&alias);
                }
            }
            if c.command_network == crate::config::CommandNetwork::Inherit {
                // Keep filesystem, PID, environment and user isolation. Only the
                // network namespace is inherited (including an outer container's
                // network policy). Never bind /etc or a credential directory.
                cmd.arg("--share-net");
                for file in [
                    "/etc/resolv.conf",
                    "/etc/hosts",
                    "/etc/nsswitch.conf",
                    "/etc/ssl/certs/ca-certificates.crt",
                    "/etc/pki/tls/certs/ca-bundle.crt",
                    "/etc/ssl/ca-bundle.pem",
                ] {
                    if Path::new(file).is_file() {
                        cmd.arg("--ro-bind").arg(file).arg(file);
                    }
                }
            }
            for root in &roots {
                cmd.arg("--bind").arg(root).arg(root);
            }
            cmd.arg("--chdir").arg(&cwd).arg("--");
            cmd.arg(command);
        }
    }
    #[cfg(target_os = "macos")]
    cmd.arg(command);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("Unable to start isolated command")?;
    let guard = ProcessGroup {
        id: child.id().context("Command failed to start")?,
        container_admin: c.command_boundary == crate::config::CommandBoundary::Container,
    };
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let wait = async {
        let status = child.wait().await;
        drop(guard);
        status
    };
    let (status, out, err) =
        tokio::time::timeout(Duration::from_secs(c.limits.command_seconds), async {
            tokio::join!(
                wait,
                capture(out, c.limits.tool_output_bytes),
                capture(err, c.limits.tool_output_bytes)
            )
        })
        .await
        .context("Command timed out; inspect files before retrying")?;
    let status = status?;
    let (out, ot) = out?;
    let (err, et) = err?;
    Ok(
        json!({"exit_code":status.code(),"success":status.success(),"stdout":out,"stderr":err,"truncated":ot||et}),
    )
}
