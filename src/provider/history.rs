//! Native continuation payloads stay private. ACP sees only normalized text
//! and tool calls; each provider receives its own original assistant payload.
use crate::config::ProviderKind;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn text_item(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}],"_provider_shadow":true})
}
pub fn call_item(id: &str, name: &str, args: &Value, wire_id: Option<&str>) -> Result<Value> {
    ensure!(
        !id.is_empty() && !name.is_empty(),
        "Provider returned a tool call without identity"
    );
    let args = if let Some(s) = args.as_str() {
        serde_json::from_str::<Value>(s).context("Provider returned invalid tool JSON")?
    } else {
        args.clone()
    };
    ensure!(
        args.is_object(),
        "Provider tool arguments must be an object"
    );
    Ok(
        json!({"type":"function_call","call_id":id,"name":name,"arguments":serde_json::to_string(&args)?,"_wire_call_id":wire_id,"_provider_shadow":true}),
    )
}
pub fn normalized(
    kind: ProviderKind,
    message: Value,
    public: Vec<Value>,
    status: &str,
    usage: Value,
    stop: &str,
) -> Value {
    let mut output = vec![json!({"type":"provider_state","provider":kind,"message":message})];
    output.extend(public);
    json!({"output":output,"status":status,"usage":usage,"stop_reason":stop})
}
fn push(messages: &mut Vec<Value>, role: &str, field: &str, part: Value) {
    if let Some(last) = messages.last_mut().filter(|v| v["role"] == role) {
        last[field].as_array_mut().unwrap().push(part);
    } else {
        messages.push(json!({"role":role,field:[part]}));
    }
}
pub fn messages(kind: ProviderKind, items: &[Value]) -> Result<Vec<Value>> {
    let calls: HashMap<_, _> = items
        .iter()
        .filter(|v| v["type"] == "function_call")
        .filter_map(|v| v["call_id"].as_str().map(|id| (id, v)))
        .collect();
    let mut result = Vec::new();
    for item in items {
        if item["type"] == "provider_state" {
            ensure!(
                item["provider"] == kind.id(),
                "History contains another provider's private continuation state; start a new session"
            );
            let message = &item["message"];
            if kind == ProviderKind::Anthropic {
                for part in message["content"]
                    .as_array()
                    .context("Invalid saved Anthropic content")?
                {
                    push(&mut result, "assistant", "content", part.clone());
                }
            } else if kind == ProviderKind::Gemini {
                for part in message["parts"]
                    .as_array()
                    .context("Invalid saved Gemini parts")?
                {
                    push(&mut result, "model", "parts", part.clone());
                }
            } else {
                result.push(message.clone());
            }
        } else if item["_provider_shadow"] == true {
            continue;
        } else if item["role"] == "user" {
            let parts = crate::media::user_parts(&item["content"])?;
            let mut chat = vec![];
            let mut ollama_images = vec![];
            for p in parts {
                let text = p["text"].as_str();
                if kind == ProviderKind::Anthropic {
                    let part = if let Some(text) = text {
                        json!({"type":"text","text":text})
                    } else {
                        let (mime, data) = crate::media::image(&p)?;
                        json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}})
                    };
                    push(&mut result, "user", "content", part);
                } else if kind == ProviderKind::Gemini {
                    let part = if let Some(text) = text {
                        json!({"text":text})
                    } else if p["type"] == "input_audio" {
                        json!({"inlineData":{"mimeType":p["mime_type"],"data":p["data"]}})
                    } else {
                        let (mime, data) = crate::media::image(&p)?;
                        json!({"inlineData":{"mimeType":mime,"data":data}})
                    };
                    push(&mut result, "user", "parts", part);
                } else if let Some(text) = text {
                    chat.push(json!({"type":"text","text":text}));
                } else {
                    let (_, data) = crate::media::image(&p)?;
                    ollama_images.push(data.to_owned());
                    chat.push(json!({"type":"image_url","image_url":{"url":p["image_url"]}}));
                }
            }
            if !matches!(kind, ProviderKind::Anthropic | ProviderKind::Gemini) {
                let text = chat
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if kind == ProviderKind::Ollama {
                    let mut m = json!({"role":"user","content":text});
                    if !ollama_images.is_empty() {
                        m["images"] = json!(ollama_images);
                    }
                    result.push(m);
                } else {
                    result.push(json!({"role":"user","content":if ollama_images.is_empty(){json!(text)}else{json!(chat)}}));
                }
            }
        } else if item["type"] == "function_call_output" {
            let id = item["call_id"].as_str().context("Tool result missing ID")?;
            let call = calls
                .get(id)
                .context("Tool result has no corresponding call")?;
            let output = item["output"]
                .as_str()
                .context("Expected text tool output")?;
            if kind == ProviderKind::Anthropic {
                push(
                    &mut result,
                    "user",
                    "content",
                    json!({"type":"tool_result","tool_use_id":id,"content":output}),
                );
            } else if kind == ProviderKind::Gemini {
                let mut response = json!({"name":call["name"],"response":{"result":serde_json::from_str::<Value>(output).unwrap_or(json!(output))}});
                if call["_wire_call_id"].is_string() {
                    response["id"] = call["_wire_call_id"].clone();
                }
                push(
                    &mut result,
                    "user",
                    "parts",
                    json!({"functionResponse":response}),
                );
            } else if kind == ProviderKind::Ollama {
                result.push(json!({"role":"tool","tool_name":call["name"],"content":output}));
            } else {
                result.push(json!({"role":"tool","tool_call_id":id,"content":output}));
            }
        } else {
            bail!("This session contains a different provider's history; start a new session");
        }
    }
    Ok(result)
}

pub fn usage(input: &Value, output: &Value, native: &Value) -> Value {
    json!({"input_tokens":input,"output_tokens":output,"provider_usage":native})
}

/// Image tool IDs may expire or be unavailable with store=false. Keep our own
/// bytes and submit them as explicitly labeled evidence on subsequent turns.
/// The stored transcript still contains the original tool result for replay.
pub fn responses(items: &[Value]) -> Vec<Value> {
    items.iter().map(|item|{
        if item["type"]=="image_generation_call" {
            if let Some(data)=item["result"].as_str(){
                use sha2::{Digest,Sha256};
                let mime=match item["output_format"].as_str(){Some("jpeg")=>"image/jpeg",Some("webp")=>"image/webp",_=>"image/png"};
                return json!({"role":"user","content":[{"type":"input_text","text":format!("Retained image previously generated by this agent. Artifact ID: {:x}. This is prior output, not a new user request.",Sha256::digest(data.as_bytes()))},{"type":"input_image","image_url":format!("data:{mime};base64,{data}")}]});
            }
            return json!({"role":"assistant","content":[{"type":"output_text","text":"The earlier image generation did not return an image."}]});
        }
        let mut v=item.clone();if let Some(o)=v.as_object_mut(){o.retain(|k,_|!k.starts_with('_'));}v
    }).collect()
}
