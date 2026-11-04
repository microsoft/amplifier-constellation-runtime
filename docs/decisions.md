# Decision services

Decision services answer bounded predicate, choice, and score questions independently of the conversational model. A native agent can keep its normal chat provider while using one or more named decision services for routing, relevance, prioritization, or an experiment. Scores do not grant permissions or change participation policy automatically.

## Providers

`agent-runtime decision-providers` prints editable configuration presets without connecting or downloading anything.

| Provider ID | Endpoint / model | Authentication |
| --- | --- | --- |
| `openai` | `https://api.openai.com/v1/decisions`, `gpt-6-luna` | Bearer `OPENAI_API_KEY` |
| `jev` | `https://api.typesafe.ai/v1/systemone`, `jev-latest` | Bearer `TYPESAFE_API_KEY` |
| `microsoft-decision` | Full Foundry URL ending `/providers/microsoft/v1/systemone`; set the deployment/model name | `api-key` from `AZURE_DECISIONS_API_KEY`, or explicit bearer token |
| `h2o-lightning` | Official local SystemOne shim at `http://127.0.0.1:8741/v1/systemone`, `h2oai/h2o-lightning-4b` | Optional local key; remote deployments require explicit authentication |
| `local-decisions` | Separately running Amplifier-compatible server at `http://127.0.0.1:18990/v1/systemone`, `d3-mini-int8` by default | Bearer `LOCAL_DECISIONS_API_KEY`, or explicit `auth: "none"` for a keyless deployment |

Model/server installation, cloud provisioning, Azure token refresh, and model weights remain external. Use the actual endpoint/deployment for your account. HTTPS is verified; private CAs are supported. HTTP outside loopback requires explicit opt-in. Secrets stay in environment references, never the configuration examples.

## Configure and call

```sh
agent-runtime --home /absolute/path/to/agent configure-decision examples/decisions/openai.json
agent-runtime --home /absolute/path/to/agent decide routing examples/decisions/request.json
```

`configure-decision` adds or replaces the named entry in `runtime.json`'s `decisions` array. Configure multiple names to compare providers. `decide` works without a chat turn. Set `expose_to_agent: true` to additionally offer the `decision_evaluate` function tool to the agent; it can select only explicitly exposed backends. Reconnect after configuration changes. Disable exposure or remove the entry in configuration to revoke access. Native capability settings that disable function tools also hide decision tools.

For a managed Native Workshop installation using the same OpenAI account, set `api_key_env` to `AGENT_MODEL_API_KEY`, the existing host-injected credential reference. Additional providers need their own private secret/environment mapping in the host; existing unrelated credentials are not copied automatically.

The common request contains text or structured JSON `state` and a list of named questions:

- `predicate`: instructions defining a proposition; returns probability in [0, 1].
- `choice`: instructions and 2–255 `{value, description}` alternatives; returns the selected value and the provider's probability distribution when available.
- `score`: instructions and 2–10 ordered text levels; returns a score between zero and the last index. Some providers return the distribution's weighted mean, rather than a discrete index.

Question names must be unique. Requests are capped at 64 questions and 256 KiB. Image decisions are not part of this shared interface yet. Native OpenAI questions map to its Decisions endpoint; JEV, Microsoft Decision One, H2O, and local-decisions map to SystemOne's `noul`, choice criteria, and score criteria.

Results retain the requested/resolved model, backend, answers, distributions, confidence when supplied, usage, elapsed time, request ID, and available effort/probability metadata. Refusal stays a refusal. Missing/duplicate/unknown answers and invalid probabilities fail validation. Confidence is not assumed to equal the largest probability: providers do not all define it identically.

Timeouts, cancellation, authentication failures, or rate limits produce errors; there is no silent chat fallback, guessed answer, or automatic retry. A caller can deliberately handle a refusal or failure without treating it as a negative predicate or permission grant.

## Validation

The actual OpenAI Decisions endpoint passed a predicate/choice/score check. All five providers have real-process protocol tests covering authentication, payload translation, normalization, malformed responses, refusal, and cancellation. JEV, Microsoft Decision One, H2O, and local-decisions have not been live-validated here because their credentials or running endpoints were unavailable. These checks establish integration behavior, not calibration or quality for an operational routing policy.

References: [OpenAI Decisions](https://developers.openai.com/api/docs/guides/decisions), [JEV SystemOne](https://docs.typesafe.ai/api/system-one), [Microsoft Decision One](https://learn.microsoft.com/en-us/azure/foundry/foundry-models/how-to/use-foundry-models-microsoft-decision), [H2O Lightning](https://huggingface.co/h2oai/h2o-lightning-4b), and [Amplifier local-decisions](https://github.com/microsoft/amplifier-module-provider-local-decisions).

## Ownership: social admission versus reasoning tools

Reusable pre-turn social attention belongs to an installed Instance, for any ACP
backend. A native runtime and a Codex-backed runtime can therefore receive the
same ordinary admitted prompts. The runtime must not independently apply a second
social gate to those prompts. A direct ACP client can still prompt this runtime
without installing a host or opting into attention.

This module remains useful for explicit within-turn typed decisions and existing
agent-authored watches. Those advisory results do not grant tool permissions,
create responsibilities, renew deadlines or prove task completion. The old
unmerged `participate` CLI proposal is superseded by Instance's optional
`com.microsoft.amplifier.attention` profile; it is not a runtime requirement.
See the [family attention contract](https://github.com/microsoft/amplifier-constellation/blob/main/contracts/attention.v1-candidate.md).
