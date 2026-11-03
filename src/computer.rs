//! Native computer calls use an explicitly configured MCP environment. They never
//! gain authority from the model, a workspace mode, or an MCP read-only hint.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Computer {
    pub mcp_server: String,
    pub mcp_tool: String,
    pub display_width: u32,
    pub display_height: u32,
}
impl Computer {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [&self.mcp_server, &self.mcp_tool]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)),
            "Use bounded MCP computer server/tool names"
        );
        ensure!(
            (64..=4096).contains(&self.display_width) && (64..=4096).contains(&self.display_height),
            "Computer display dimensions must be 64–4096 pixels"
        );
        Ok(())
    }
    pub fn arguments(&self, call: &Value) -> Result<Value> {
        ensure!(
            call.to_string().len() <= 65536,
            "Computer call is too large"
        );
        let actions = call["actions"]
            .as_array()
            .context("Computer call needs ordered actions")?;
        ensure!(
            !actions.is_empty() && actions.len() <= 32,
            "Computer call needs 1–32 actions"
        );
        let point = |v: &Value| -> Result<()> {
            ensure!(
                v["x"]
                    .as_u64()
                    .is_some_and(|n| n < self.display_width as u64)
                    && v["y"]
                        .as_u64()
                        .is_some_and(|n| n < self.display_height as u64),
                "Computer coordinates exceed the configured display"
            );
            Ok(())
        };
        for a in actions {
            if a.get("keys").is_some_and(|v| !v.is_null()) && a["type"] != "keypress" {
                ensure!(
                    a["keys"].as_array().is_some_and(|keys| keys.len() <= 4
                        && keys.iter().all(|k| k
                            .as_str()
                            .is_some_and(|s| ["ALT", "CTRL", "CONTROL", "CMD", "META", "SHIFT"]
                                .contains(&s.to_uppercase().as_str())))),
                    "Unsupported mouse modifier"
                );
            }

            let keys: &[&str] = match a["type"].as_str().unwrap_or("") {
                "click" => {
                    point(a)?;
                    ensure!(
                        ["left", "right", "wheel", "back", "forward"]
                            .contains(&a["button"].as_str().unwrap_or("")),
                        "Unsupported mouse button"
                    );
                    &["type", "x", "y", "button", "keys"]
                }
                "double_click" | "move" => {
                    point(a)?;
                    &["type", "x", "y", "keys"]
                }
                "scroll" => {
                    point(a)?;
                    for k in ["scroll_x", "scroll_y"] {
                        ensure!(
                            a[k].as_i64().is_some_and(|n| n.unsigned_abs() <= 10000),
                            "Scroll exceeds limit"
                        );
                    }
                    &["type", "x", "y", "scroll_x", "scroll_y"]
                }
                "drag" => {
                    let path = a["path"].as_array().context("Missing drag path")?;
                    ensure!(
                        !path.is_empty() && path.len() <= 100,
                        "Drag path exceeds limit"
                    );
                    for p in path {
                        point(p)?;
                        ensure!(
                            p.as_object().is_some_and(|o| o.len() == 2),
                            "Invalid drag point"
                        );
                    }
                    &["type", "path"]
                }
                "keypress" => {
                    let keys = a["keys"].as_array().context("Missing keys")?;
                    ensure!(
                        !keys.is_empty()
                            && keys.len() <= 8
                            && keys.iter().all(|k| k.as_str().is_some_and(|s| !s.is_empty()
                                && s.len() <= 40
                                && !s.chars().any(char::is_control))),
                        "Invalid key combination"
                    );
                    &["type", "keys"]
                }
                "type" => {
                    ensure!(
                        a["text"]
                            .as_str()
                            .is_some_and(|s| s.len() <= 10000 && !s.contains('\0')),
                        "Typed text exceeds limit"
                    );
                    &["type", "text"]
                }
                "wait" | "screenshot" => &["type"],
                _ => anyhow::bail!("Unsupported computer action"),
            };
            ensure!(
                a.as_object()
                    .is_some_and(|o| o.keys().all(|k| keys.contains(&k.as_str()))),
                "Unknown computer action fields"
            );
        }
        if let Some(checks) = call.get("pending_safety_checks") {
            ensure!(
                checks.as_array().is_some_and(|v| v.len() <= 16
                    && v.iter().all(|v| v["id"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 200))),
                "Invalid computer safety checks"
            );
        }
        Ok(
            json!({"actions":actions,"display_width":self.display_width,"display_height":self.display_height}),
        )
    }
    pub fn screenshot(&self, result: &Value) -> Result<String> {
        ensure!(
            result["isError"] != true,
            "Computer environment reported a failed action; inspect it before retrying"
        );
        let images: Vec<_> = result["content"]
            .as_array()
            .context("Computer environment returned no content")?
            .iter()
            .filter(|c| c["type"] == "image")
            .collect();
        ensure!(
            images.len() == 1 && images[0]["mimeType"] == "image/png",
            "Computer environment must return one PNG screenshot"
        );
        let data = images[0]["data"]
            .as_str()
            .context("Screenshot bytes missing")?;
        ensure!(
            data.len() <= 8 * 1024 * 1024,
            "Screenshot exceeds 8 MiB encoded limit"
        );
        let bytes = STANDARD.decode(data)?;
        ensure!(
            bytes.len() >= 24
                && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                && &bytes[12..16] == b"IHDR",
            "Invalid PNG screenshot"
        );
        ensure!(
            u32::from_be_bytes(bytes[16..20].try_into()?) == self.display_width
                && u32::from_be_bytes(bytes[20..24].try_into()?) == self.display_height,
            "Screenshot dimensions differ from the configured display"
        );
        Ok(data.to_owned())
    }
}
