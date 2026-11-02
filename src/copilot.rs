//! Bounded official-SDK bridge. The Rust engine retains execution authority.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub python: PathBuf,
    pub script: PathBuf,
}
struct Group(u32);
impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = self.0;
    }
}
const MAX: usize = 8 * 1024 * 1024;

pub async fn invoke(
    config: &Config,
    key: &str,
    mut request: Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let bridge = config
        .provider
        .copilot
        .as_ref()
        .context("Configure the official Copilot SDK bridge")?;
    ensure!(
        cfg!(unix),
        "Copilot bridge process-tree cleanup is currently verified only on Unix"
    );
    let scratch = tempfile::Builder::new()
        .prefix("agent-copilot-")
        .tempdir()?;
    request["protocol_version"] = json!(1);
    request["timeout_seconds"] = json!(config.limits.turn_seconds.min(180));
    let bytes = serde_json::to_vec(&request)?;
    ensure!(bytes.len() <= MAX, "Copilot request exceeds bridge budget");
    let mut command = tokio::process::Command::new(&bridge.python);
    command
        .arg(&bridge.script)
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("COPILOT_GITHUB_TOKEN", key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let work = async {
        let mut child = command
            .spawn()
            .context("Could not start Copilot SDK bridge")?;
        let guard = Group(child.id().context("Bridge did not start")?);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap().take(MAX as u64 + 1);
        let write = async {
            stdin.write_all(&bytes).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, anyhow::Error>(())
        };
        let read = async {
            let mut output = vec![];
            stdout.read_to_end(&mut output).await?;
            ensure!(
                output.len() <= MAX,
                "Copilot bridge output exceeded its bound"
            );
            Ok::<_, anyhow::Error>(output)
        };
        let wait = async {
            let result = child.wait().await;
            drop(guard);
            result
        };
        let (_, output, status) =
            tokio::try_join!(write, read, async { Ok::<_, anyhow::Error>(wait.await?) })?;
        ensure!(
            status.success(),
            "Copilot SDK step failed; check the pinned SDK, explicit token, model and account entitlement"
        );
        let result: Value =
            serde_json::from_slice(&output).context("Invalid Copilot bridge response")?;
        ensure!(
            result["protocol_version"] == 1 && result.get("error").is_none(),
            "Copilot bridge returned an error"
        );
        Ok(result)
    };
    tokio::select! {
        biased;
        _=cancel.cancelled()=>bail!("Copilot step cancelled"),
        result=tokio::time::timeout(Duration::from_secs(config.limits.turn_seconds.min(180)+15),work)=>result.context("Copilot SDK step timed out")?
    }
}

pub async fn complete(
    config: &Config,
    key: &str,
    instructions: &str,
    items: &[Value],
    tools: &[Value],
    cancel: &CancellationToken,
) -> Result<Value> {
    ensure!(
        items
            .iter()
            .all(|v| v["type"] != "provider_state" && v["type"] != "reasoning"),
        "Another provider's continuation cannot be sent to Copilot"
    );
    let result = invoke(config,key,json!({"operation":"complete","model":config.provider.model,"instructions":instructions,"messages":items,"tools":tools,"max_output_tokens":config.limits.max_output_tokens,"reasoning_effort":config.provider.reasoning_effort}),cancel).await?;
    ensure!(
        result["status"] == "completed" && result["sdk_tools_executed"] == false,
        "Copilot bridge did not preserve the execution boundary"
    );
    let output = result["output"]
        .as_array()
        .context("Copilot returned no output")?;
    ensure!(
        !output.is_empty() && output.len() <= 256,
        "Invalid Copilot output count"
    );
    let mut ids = std::collections::HashSet::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") => {
                ensure!(
                    item["role"] == "assistant"
                        && item["content"].as_array().is_some_and(|a| a
                            .iter()
                            .all(|p| p["type"] == "output_text" && p["text"].is_string())),
                    "Invalid Copilot text output"
                );
            }
            Some("function_call") => {
                let id = item["call_id"]
                    .as_str()
                    .context("Missing Copilot call ID")?;
                ensure!(
                    !id.is_empty()
                        && ids.insert(id)
                        && tools.iter().any(|t| t["name"] == item["name"]),
                    "Invalid, duplicated, or unadvertised Copilot tool call"
                );
                let arguments: Value = serde_json::from_str(
                    item["arguments"]
                        .as_str()
                        .context("Missing tool arguments")?,
                )?;
                ensure!(
                    arguments.is_object(),
                    "Copilot tool arguments must be an object"
                );
            }
            _ => bail!("Unsupported Copilot output"),
        }
    }
    Ok(result)
}
