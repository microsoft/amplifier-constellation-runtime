//! Typed decision sidecars. No chat routing, permission granting, or fallback.
use crate::{config::ProviderAuth, tools};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Openai,
    Jev,
    MicrosoftDecision,
    H2oLightning,
    LocalDecisions,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    pub name: String,
    pub provider: Kind,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default)]
    pub expose_to_agent: bool,
    #[serde(default)]
    pub tls_ca_file: Option<PathBuf>,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    30
}
impl Backend {
    pub fn preset(kind: Kind, name: String) -> Self {
        let (endpoint, model, key) = match kind {
            Kind::Openai => (
                "https://api.openai.com/v1/decisions",
                "gpt-6-luna",
                "OPENAI_API_KEY",
            ),
            Kind::Jev => (
                "https://api.typesafe.ai/v1/systemone",
                "jev-latest",
                "TYPESAFE_API_KEY",
            ),
            Kind::MicrosoftDecision => ("", "Microsoft-Decision-1", "AZURE_DECISIONS_API_KEY"),
            Kind::H2oLightning => (
                "http://127.0.0.1:8741/v1/systemone",
                "h2oai/h2o-lightning-4b",
                "H2O_DECISIONS_API_KEY",
            ),
            Kind::LocalDecisions => (
                "http://127.0.0.1:18990/v1/systemone",
                "d3-mini-int8",
                "LOCAL_DECISIONS_API_KEY",
            ),
        };
        Self {
            name,
            provider: kind,
            endpoint: endpoint.into(),
            model: model.into(),
            api_key_env: key.into(),
            auth: ProviderAuth::Auto,
            expose_to_agent: false,
            tls_ca_file: None,
            allow_insecure_http: false,
            timeout_seconds: timeout(),
        }
    }
}
pub fn validate_backends(backends: &[Backend]) -> Result<()> {
    ensure!(backends.len() <= 16, "Too many decision backends");
    let mut names = HashSet::new();
    for b in backends {
        ensure!(
            b.auth != ProviderAuth::Chatgpt,
            "ChatGPT plan usage currently applies to Responses, not decision services"
        );
        ensure!(
            !b.name.is_empty() && b.name.len() <= 80 && names.insert(&b.name),
            "Decision backend names must be unique"
        );
        ensure!(
            !b.model.is_empty() && b.model.len() <= 200,
            "Set the decision model or deployment"
        );
        ensure!(
            (1..=300).contains(&b.timeout_seconds),
            "Invalid decision timeout"
        );
        ensure!(
            b.auth == ProviderAuth::None
                || (!b.api_key_env.is_empty()
                    && b.api_key_env
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_')),
            "Invalid decision credential reference"
        );
        let u =
            reqwest::Url::parse(&b.endpoint).context("Set the complete decision endpoint URL")?;
        ensure!(
            u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none(),
            "Decision endpoint cannot contain credentials or query parameters"
        );
        ensure!(
            u.scheme() == "https"
                || (u.scheme() == "http"
                    && (b.allow_insecure_http
                        || matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))),
            "Decision endpoint requires HTTPS or loopback HTTP"
        );
        if let Some(p) = &b.tls_ca_file {
            ensure!(
                p.is_absolute() && p.is_file(),
                "Decision CA must be an existing absolute file"
            );
        }
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub state: Value,
    pub questions: Vec<Question>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionText {
    pub value: String,
    pub description: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Question {
    Predicate {
        name: String,
        instructions: String,
    },
    Choice {
        name: String,
        instructions: String,
        choices: Vec<OptionText>,
    },
    Score {
        name: String,
        instructions: String,
        levels: Vec<String>,
    },
}
impl Question {
    fn name(&self) -> &str {
        match self {
            Self::Predicate { name, .. } | Self::Choice { name, .. } | Self::Score { name, .. } => {
                name
            }
        }
    }
    fn instructions(&self) -> &str {
        match self {
            Self::Predicate { instructions, .. }
            | Self::Choice { instructions, .. }
            | Self::Score { instructions, .. } => instructions,
        }
    }
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.state.is_string() || self.state.is_object() || self.state.is_array(),
            "Decision state must be text or structured JSON"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= 256 * 1024,
            "Decision request exceeds 256 KiB"
        );
        ensure!(
            (1..=64).contains(&self.questions.len()),
            "Supply 1 to 64 decision questions"
        );
        let mut names = HashSet::new();
        for q in &self.questions {
            ensure!(
                !q.name().is_empty() && q.name().len() <= 100 && names.insert(q.name()),
                "Question names must be nonempty and unique"
            );
            ensure!(
                !q.instructions().trim().is_empty(),
                "Question instructions are required"
            );
            let labels: Vec<_> = match q {
                Question::Choice { choices, .. } => {
                    ensure!(
                        (2..=255).contains(&choices.len()),
                        "Supply 2 to 255 choices"
                    );
                    choices.iter().map(|c| c.value.as_str()).collect()
                }
                Question::Score { levels, .. } => {
                    ensure!(
                        (2..=10).contains(&levels.len()),
                        "Supply 2 to 10 ordered levels"
                    );
                    levels.iter().map(String::as_str).collect()
                }
                _ => vec![],
            };
            let unique: HashSet<_> = labels.iter().collect();
            ensure!(
                unique.len() == labels.len() && labels.iter().all(|s| !s.trim().is_empty()),
                "Options must be nonempty and unique"
            );
        }
        Ok(())
    }
    pub fn wire(&self, b: &Backend) -> Result<Value> {
        self.validate()?;
        if b.provider == Kind::Openai {
            let qs:Vec<_>=self.questions.iter().map(|q|match q {
                Question::Score{name,instructions,levels}=>json!({"type":"score","name":name,"instructions":instructions,"levels":levels.iter().map(|l|json!({"label":l})).collect::<Vec<_>>()}),
                _=>serde_json::to_value(q).unwrap(),
            }).collect();
            let input = if self.state.is_string() {
                self.state.clone()
            } else {
                json!([{"role":"user","content":[{"type":"input_json","data":self.state}]}])
            };
            Ok(json!({"model":b.model,"input":input,"questions":qs}))
        } else {
            let mut qs = serde_json::Map::new();
            for q in &self.questions {
                let question = match q {
                    Question::Predicate { instructions, .. } => {
                        json!({"type":"noul","instructions":instructions})
                    }
                    Question::Choice {
                        instructions,
                        choices,
                        ..
                    } => {
                        json!({"type":"choice","instructions":instructions,"criteria":choices.iter().map(|c|(c.value.clone(),json!(c.description))).collect::<serde_json::Map<_,_>>()})
                    }
                    Question::Score {
                        instructions,
                        levels,
                        ..
                    } => json!({"type":"score","instructions":instructions,"criteria":levels}),
                };
                qs.insert(q.name().into(), question);
            }
            Ok(json!({"model":b.model,"state":self.state,"questions":qs}))
        }
    }
}
fn probability(v: &Value) -> Result<f64> {
    let p = v.as_f64().context("Decision probability missing")?;
    ensure!(
        p.is_finite() && (0.0..=1.0).contains(&p),
        "Invalid decision probability"
    );
    Ok(p)
}
pub fn normalize(r: &Request, b: &Backend, v: &Value) -> Result<Value> {
    let mut answers = BTreeMap::new();
    if b.provider == Kind::Openai {
        for a in v["answers"]
            .as_array()
            .context("Decision answers missing")?
        {
            let name = a["name"].as_str().context("Answer name missing")?;
            ensure!(
                answers.insert(name, a.clone()).is_none(),
                "Duplicate decision answer"
            );
        }
    } else {
        for (name, a) in v["answers"]
            .as_object()
            .context("Decision answers missing")?
        {
            answers.insert(name.as_str(), a.clone());
        }
    }
    ensure!(
        answers.len() == r.questions.len(),
        "Decision answer count mismatch"
    );
    let mut out = vec![];
    for q in &r.questions {
        let a = answers
            .get(q.name())
            .context("Missing requested decision answer")?;
        if a["type"] == "refusal" {
            out.push(json!({"name":q.name(),"type":"refusal"}));
            continue;
        }
        let mut answer = json!({"name":q.name()});
        let keys: Vec<String> = match q {
            Question::Predicate { .. } => {
                ensure!(
                    a["type"]
                        == if b.provider == Kind::Openai {
                            "predicate"
                        } else {
                            "noul"
                        },
                    "Wrong decision answer type"
                );
                answer["type"] = json!("predicate");
                answer["probability"] = json!(probability(
                    &a[if b.provider == Kind::Openai {
                        "probability"
                    } else {
                        "noul"
                    }]
                )?);
                vec![]
            }
            Question::Choice { choices, .. } => {
                ensure!(a["type"] == "choice", "Wrong decision answer type");
                let choice = a["choice"].as_str().context("Choice missing")?;
                ensure!(
                    choices.iter().any(|c| c.value == choice),
                    "Decision chose an unknown option"
                );
                answer["type"] = json!("choice");
                answer["choice"] = json!(choice);
                choices.iter().map(|c| c.value.clone()).collect()
            }
            Question::Score { levels, .. } => {
                ensure!(a["type"] == "score", "Wrong decision answer type");
                let score = a["score"].as_f64().context("Score missing")?;
                ensure!(
                    score.is_finite() && score >= 0.0 && score <= (levels.len() - 1) as f64,
                    "Score outside rubric"
                );
                answer["type"] = json!("score");
                answer["score"] = json!(score);
                answer["levels"] = json!(levels);
                (0..levels.len()).map(|i| i.to_string()).collect()
            }
        };
        if !keys.is_empty() {
            let mut ps = BTreeMap::new();
            if b.provider == Kind::Openai {
                for p in a["probabilities"]
                    .as_array()
                    .context("Decision distribution missing")?
                {
                    let key = if matches!(q, Question::Score { .. }) {
                        p["value"]
                            .as_u64()
                            .context("Score level index missing")?
                            .to_string()
                    } else {
                        p["value"].as_str().context("Choice key missing")?.into()
                    };
                    ensure!(
                        ps.insert(key, probability(&p["probability"])?).is_none(),
                        "Duplicate option probability"
                    );
                }
            } else {
                for (key, p) in a["probabilities"]
                    .as_object()
                    .context("Decision distribution missing")?
                {
                    ps.insert(key.clone(), probability(p)?);
                }
            }
            ensure!(
                ps.len() == keys.len() && keys.iter().all(|k| ps.contains_key(k)),
                "Decision distribution does not match options"
            );
            ensure!(
                (ps.values().sum::<f64>() - 1.0).abs() <= 0.02,
                "Decision probabilities do not sum to one"
            );
            if matches!(q, Question::Score { .. }) {
                let mean: f64 = keys.iter().enumerate().map(|(i, k)| i as f64 * ps[k]).sum();
                ensure!(
                    (mean - answer["score"].as_f64().unwrap()).abs() <= 0.03,
                    "Score disagrees with probability distribution"
                );
            }
            answer["probabilities"] = json!(ps);
            if !a["confidence"].is_null() {
                answer["confidence"] = json!(probability(&a["confidence"])?);
            }
        }
        out.push(answer);
    }
    Ok(
        json!({"backend":b.name,"provider":b.provider,"requested_model":b.model,"model":v["model"].as_str().unwrap_or(&b.model),"answers":out,"usage":v["usage"],"metadata":{"probability_source":v["probability_source"],"effort_used":v["effort_used"],"attention_mode":v["attention_mode"]}}),
    )
}
pub async fn evaluate(b: &Backend, r: &Request, cancel: &CancellationToken) -> Result<Value> {
    validate_backends(std::slice::from_ref(b))?;
    let body = r.wire(b)?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(b.timeout_seconds));
    if let Some(p) = &b.tls_ca_file {
        for cert in reqwest::Certificate::from_pem_bundle(&std::fs::read(p)?)? {
            builder = builder.add_root_certificate(cert);
        }
    }
    let mut req = builder.build()?.post(&b.endpoint).json(&body);
    if b.auth != ProviderAuth::None {
        let key = std::env::var(&b.api_key_env)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        // The official local H2O shim has no auth by default. Other services do.
        ensure!(
            key.is_some()
                || (b.provider == Kind::H2oLightning
                    && b.auth == ProviderAuth::Auto
                    && reqwest::Url::parse(&b.endpoint)?.host_str() == Some("127.0.0.1")),
            "Configure {} for this decision backend",
            b.api_key_env
        );
        if let Some(key) = key {
            ensure!(
                !key.chars().any(char::is_control),
                "Invalid decision credential"
            );
            req = if b.auth == ProviderAuth::ApiKey
                || (b.auth == ProviderAuth::Auto && b.provider == Kind::MicrosoftDecision)
            {
                req.header("api-key", key)
            } else {
                req.bearer_auth(key)
            };
        }
    }
    let start = Instant::now();
    let future = async {
        let resp = req
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Decision endpoint could not be reached"))?;
        ensure!(
            resp.status().is_success(),
            "Decision request failed (HTTP {}); no retry or fallback",
            resp.status().as_u16()
        );
        let request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut chunks = resp.bytes_stream();
        let mut bytes = vec![];
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.context("Decision response interrupted")?;
            ensure!(
                bytes.len() + chunk.len() <= 2 * 1024 * 1024,
                "Decision response too large"
            );
            bytes.extend(chunk);
        }
        let v: Value = serde_json::from_slice(&bytes).context("Invalid decision response JSON")?;
        let mut out = normalize(r, b, &v)?;
        out["elapsed_ms"] = json!(start.elapsed().as_millis());
        out["request_id"] = json!(request_id);
        Ok(out)
    };
    tokio::select! {biased;_=cancel.cancelled()=>bail!("Decision cancelled; remote work may still finish"),v=future=>v}
}
pub fn definition(backends: &[Backend]) -> Option<Value> {
    let names: Vec<_> = backends
        .iter()
        .filter(|b| b.expose_to_agent)
        .map(|b| &b.name)
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(tools::definition(
        "decision_evaluate",
        "Evaluate supplied evidence using an explicitly configured decision model. Returns probabilities, a closed choice, or a rubric score. These are estimates, not permission to take action. Supply only relevant evidence; no other history is added. Errors and refusals require caller judgment, not an automatic fallback.",
        json!({"backend":{"type":"string","enum":names},"state":{},"questions":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"type":{"type":"string","enum":["predicate","choice","score"]},"instructions":{"type":"string"},"choices":{"type":"array","items":{"type":"object","properties":{"value":{"type":"string"},"description":{"type":"string"}},"required":["value","description"],"additionalProperties":false}},"levels":{"type":"array","items":{"type":"string"}}},"required":["name","type","instructions"],"additionalProperties":false}}}),
        vec!["backend", "state", "questions"],
    ))
}
