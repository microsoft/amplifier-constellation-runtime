//! Optional descriptions of reusable approvals. The installed host owns policy;
//! this module neither remembers decisions nor authorizes any operation.
use crate::{
    config::{Config, Mode},
    mcp::Tool,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const META: &str = "com.microsoft.amplifier.approval-policy";
pub const PRODUCER: &str = "microsoft.amplifier-constellation-runtime";

pub struct Context<'a> {
    pub config: &'a Config,
    pub mode: Mode,
    pub cwd: &'a Path,
    pub items: &'a [Value],
}

pub fn negotiated(params: &Value) -> bool {
    let value = &params["_meta"][META];
    value["version"].as_u64() == Some(1)
        && value["owner"] == "instance"
        && value["instanceId"]
            .as_str()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

pub fn capability() -> Value {
    json!({"version":1,"owner":"instance","producer":PRODUCER,"reusableRequests":true})
}

/// Objects use sorted keys, arrays retain order. No source/display text, request
/// ID, session ID or credential value is exposed by the resulting descriptor.
pub(crate) fn digest(domain: &str, value: &Value) -> String {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"amplifier-runtime.approval-policy/1\0");
    hash.update(domain.as_bytes());
    hash.update([0]);
    // serde_json::Value contains no unsupported map keys or non-finite numbers.
    hash.update(serde_json::to_vec(&canonical(value)).expect("JSON value serialization"));
    format!("{:x}", hash.finalize())
}

fn policy(config: &Config, mode: Mode, cwd: &Path) -> Option<Value> {
    let mut roots = config.effective_roots().ok()?;
    roots.sort();
    let roots = roots
        .iter()
        .map(|p| p.to_str())
        .collect::<Option<Vec<_>>>()?;
    let cwd = std::fs::canonicalize(cwd).ok()?;
    Some(json!({"cwd":cwd.to_str()?,"roots":roots,"mode":mode,
        "platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},
        "commandBoundary":config.command_boundary,"commandNetwork":config.command_network,
        "limits":config.limits,"allowedMcpMutations":config.allowed_mcp_mutations,
        "trustedMcpReadonlyUrls":config.trusted_mcp_readonly_urls,
        "allowClientStdioMcp":config.allow_client_stdio_mcp}))
}

fn file_args(config: &Config, cwd: &Path, args: &Value) -> Option<Value> {
    let (root, relative) = crate::tools::relative(config, cwd, args["path"].as_str()?).ok()?;
    let relative: std::path::PathBuf = relative.components().collect();
    let mut effective = args.clone();
    effective["path"] = json!({"root":root.to_str()?,"relative":relative.to_str()?});
    Some(effective)
}

fn descriptor(scope: Value, description: &'static str) -> Value {
    json!({"version":1,"producer":PRODUCER,"key":digest("resolved-request",&scope),
        "description":description,"allowOptionId":"allow_once","authorityLifetime":"request"})
}

fn command_arguments(args: &Value, browser_paths: Option<[Option<String>; 2]>) -> Value {
    let mut input = args.clone();
    if input.get("args").is_none() {
        input["args"] = json!([]);
    }
    json!({"input":input,"containerBrowserPaths":browser_paths})
}

/// Failure to describe a reusable scope leaves the original Once request alone.
/// The normal executor still performs every schema, mode and resource check.
pub fn annotate(
    call: &mut Value,
    context: Context<'_>,
    definition: &Value,
    args: &Value,
    tool: Option<&Tool>,
) {
    let Context {
        config,
        mode,
        cwd,
        items,
    } = context;
    let Some(policy) = policy(config, mode, cwd) else {
        return;
    };
    let (operation, effective, description) = if let Some(tool) = tool {
        let Some(binding) = &tool.approval_binding else {
            return;
        };
        (
            json!({"kind":"mcp","name":tool.name,"connection":binding,
            "schema":tool.definition["parameters"],"readonly":tool.readonly}),
            args.clone(),
            "Same MCP tool and exact arguments at the same configured connection, schema, workspace grants and mode.",
        )
    } else {
        let Some(name) = definition["name"].as_str() else {
            return;
        };
        let (effective, description) = match name {
            "workspace_write" => {
                let Some(effective) = file_args(config, cwd, args) else {
                    return;
                };
                (
                    effective,
                    "Same file path and exact content under the same workspace grants and mode.",
                )
            }
            "workspace_run" => {
                // The executor has a fixed environment except for these two
                // installer-selected container browser locations. Bind the
                // same effective values without exposing them in metadata.
                let browser_paths = if cfg!(target_os = "linux")
                    && config.command_boundary == crate::config::CommandBoundary::Container
                {
                    Some([
                        std::env::var("PLAYWRIGHT_BROWSERS_PATH").ok(),
                        std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok(),
                    ])
                } else {
                    None
                };
                (
                    command_arguments(args, browser_paths),
                    "Same exact command and arguments in the same working folder, with the same workspace grants, isolation and limits.",
                )
            }
            "workspace_save_artifact" => {
                let Ok(artifacts) = crate::media::artifacts(items) else {
                    return;
                };
                let artifact = if args["artifact"] == "latest" {
                    artifacts.last()
                } else {
                    artifacts.iter().find(|a| a["id"] == args["artifact"])
                };
                let Some(artifact) = artifact else {
                    return;
                };
                let Some(effective) = file_args(
                    config,
                    cwd,
                    &json!({"path":args["path"],"data":artifact["data"]}),
                ) else {
                    return;
                };
                (
                    effective,
                    "Same actual artifact bytes and file path under the same workspace grants and mode; a different latest artifact asks again.",
                )
            }
            _ => return,
        };
        (
            json!({"kind":"builtin","name":name,"schema":definition["parameters"]}),
            effective,
            description,
        )
    };
    call["_meta"][META] = descriptor(
        json!({"operation":operation,"arguments":effective,"policy":policy}),
        description,
    );
}

pub fn annotate_computer(
    call: &mut Value,
    context: Context<'_>,
    tool: &Tool,
    args: &Value,
    checks: &Value,
) {
    let Context {
        config, mode, cwd, ..
    } = context;
    let Some(policy) = policy(config, mode, cwd) else {
        return;
    };
    let Some(binding) = &tool.approval_binding else {
        return;
    };
    call["_meta"][META] = descriptor(
        json!({"operation":{"kind":"builtin","name":"computer",
        "configuration":config.provider.native.computer,"connection":binding,
        "mcpTool":tool.name,"schema":tool.definition["parameters"]},
        "arguments":args,"safetyChecks":checks,"policy":policy}),
        "Same exact computer actions and safety checks in the same configured environment, with the same workspace grants and mode.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(config: &Config, definition: &Value, args: &Value) -> Value {
        let mut call = json!({"toolCallId":"not-part-of-rule"});
        annotate(
            &mut call,
            Context {
                config,
                mode: Mode::Ask,
                cwd: &config.workspace,
                items: &[],
            },
            definition,
            args,
            None,
        );
        call["_meta"][META].clone()
    }

    #[test]
    fn resolved_write_binds_exact_bytes_schema_and_current_grants_without_exposing_them() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let mut config = Config::initial(root.clone(), "Fixture".into(), "fixture".into());
        let mut definition = crate::tools::definitions_for_config(&config)
            .into_iter()
            .find(|d| d["name"] == "workspace_write")
            .unwrap();
        let args = json!({"path":"same.txt","content":"SECRET-content"});
        let first = write(&config, &definition, &args);
        assert!(
            first["key"]
                .as_str()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(first["key"].as_str().unwrap().len(), 64);
        assert!(!first.to_string().contains("SECRET-content"));
        assert!(!first.to_string().contains(root.to_str().unwrap()));
        assert_eq!(first["authorityLifetime"], "request");
        assert_eq!(first["allowOptionId"], "allow_once");
        definition["description"] = json!("Display copy is not authority");
        assert_eq!(write(&config, &definition, &args), first);
        assert_eq!(
            write(
                &config,
                &definition,
                &json!({"path":root.join("same.txt"),"content":"SECRET-content"})
            ),
            first
        );
        for changed in [
            json!({"path":"other.txt","content":"SECRET-content"}),
            json!({"path":"same.txt","content":"changed"}),
        ] {
            assert_ne!(write(&config, &definition, &changed)["key"], first["key"]);
        }
        definition["parameters"]["properties"]["content"]["maxLength"] = json!(12);
        assert_ne!(write(&config, &definition, &args)["key"], first["key"]);
        definition["parameters"]["properties"]["content"]
            .as_object_mut()
            .unwrap()
            .remove("maxLength");
        let extra = root.join("extra");
        std::fs::create_dir(&extra).unwrap();
        config.session_roots = Some(vec![root, extra]);
        assert_ne!(write(&config, &definition, &args)["key"], first["key"]);
    }

    #[test]
    fn unresolved_or_unrepresentable_resources_keep_once_without_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::initial(temp.path().to_owned(), "Fixture".into(), "fixture".into());
        let definition = json!({"name":"workspace_write","parameters":{}});
        assert!(
            write(
                &config,
                &definition,
                &json!({"path":"../outside","content":"no"})
            )
            .is_null()
        );
        assert!(
            write(
                &config,
                &json!({"name":"workspace_save_artifact","parameters":{}}),
                &json!({"path":"image.png","artifact":"latest"})
            )
            .is_null()
        );
    }

    #[test]
    fn artifact_latest_binds_actual_saved_bytes_and_not_the_selector() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::initial(temp.path().to_owned(), "Fixture".into(), "fixture".into());
        let definition = json!({"name":"workspace_save_artifact","parameters":{"type":"object"}});
        let items = [json!({"type":"image_generation_call","result":"Zmlyc3Q="})];
        let mut call = json!({});
        let annotate = |call: &mut Value, items: &[Value], artifact: &str| {
            super::annotate(
                call,
                Context {
                    config: &config,
                    mode: Mode::Ask,
                    cwd: &config.workspace,
                    items,
                },
                &definition,
                &json!({"path":"saved.png","artifact":artifact}),
                None,
            )
        };
        annotate(&mut call, &items, "latest");
        let first = call["_meta"][META].clone();
        assert!(first.is_object());
        let artifacts = crate::media::artifacts(&items).unwrap();
        annotate(&mut call, &items, artifacts[0]["id"].as_str().unwrap());
        assert_eq!(call["_meta"][META], first);
        annotate(
            &mut call,
            &[json!({"type":"image_generation_call","result":"c2Vjb25k"})],
            "latest",
        );
        assert_ne!(call["_meta"][META]["key"], first["key"]);
    }
    #[test]
    fn negotiation_is_explicit_and_unknown_owners_or_versions_stay_legacy() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(negotiated(
            &json!({"_meta":{META:{"version":1,"owner":"instance","instanceId":id}}})
        ));
        for value in [
            Value::Null,
            json!({"version":1,"owner":"instance","instanceId":"not-uuid"}),
            json!({"version":2,"owner":"instance","instanceId":id}),
            json!({"version":1,"owner":"engine","instanceId":id}),
        ] {
            assert!(!negotiated(&json!({"_meta":{META:value}})));
        }
        assert_eq!(capability()["producer"], PRODUCER);
    }
    #[test]
    fn canonical_key_is_order_independent_but_domain_and_values_are_bound() {
        let a: Value = serde_json::from_str(r#"{"b":[2,1],"a":{"y":1,"x":2}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":2,"y":1},"b":[2,1]}"#).unwrap();
        assert_eq!(digest("request", &a), digest("request", &b));
        assert_ne!(digest("connection", &a), digest("request", &a));
        assert_ne!(
            digest("request", &a),
            digest("request", &json!({"b":[1,2],"a":{"x":2,"y":1}}))
        );
    }

    #[test]
    fn command_scope_binds_exact_effective_optional_browser_paths() {
        let args = json!({"command":"agent-browser"});
        let key = |paths| digest("resolved-request", &command_arguments(&args, paths));
        assert_eq!(
            command_arguments(&args, None),
            command_arguments(&json!({"command":"agent-browser","args":[]}), None)
        );
        let empty = key(Some([None, None]));
        assert_ne!(empty, key(Some([Some("/first".into()), None])));
        assert_ne!(empty, key(Some([None, Some("/browser".into())])));
        assert_ne!(
            key(Some([Some("/first".into()), None])),
            key(Some([Some("/second".into()), None]))
        );
        assert_ne!(
            key(Some([None, Some("/first".into())])),
            key(Some([None, Some("/second".into())]))
        );
        assert_ne!(key(None), empty);
    }
}
