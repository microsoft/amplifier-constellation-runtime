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

### Conversation participation before a reasoning turn

`agent-runtime --home /private/helper participate jev context.json` evaluates
the shared `conversation-participation-v1` policy. Use `-` instead of a filename
to read one JSON object from stdin, or `--dry-run` to inspect the typed request
without contacting a provider. This is an optional helper for any ACP client and
agent implementation; it does not require a native chat agent or extend ACP.

The bounded context has `conversation_id`, `agent`, `participants`, `messages`,
`trigger_id`, and optional `responsibilities`. Participants have `id`, `name`,
optional `role` and `is_agent`; messages have `id`, `sender`, `text`, optional
`recipients` and `reply_to`. Supply only evidence visible to this conversation's
audience. Include the trigger and this agent among the known participants.
The limit is 40 messages, 64 participants and 32 KiB of context.

The policy considers implicit addressees, group questions, follow-ups, switching
conversation partners, useful silence and duplicate responses. Names and mentions
are clues, not prerequisites. The result binds `action: engage | hold` to the
conversation and trigger IDs, retaining the normalized provider result and usage.
Engagement requires an `engage` judgment with probability at least 0.6; uncertainty
and refusals hold. These thresholds are policy choices, not calibrated guarantees.

Clients must check that evidence is still current before admitting an agent turn.
Changed conversation should be reassessed, not acted on using stale advice.
Run on changed conversation evidence, not an idle timer that keeps prompting.
Record decision attempts, actual gate admissions and actual reasoning-model
requests separately. Retain direct-message contracts, cancellation and existing
work limits. Admission is an invitation to reason, not permission to start work,
renew an expired task or force a reply. The agent may still choose silence.
Keep per-instance credentials, context and state local; no fleet coordinator is
required. Experience adapters map native messages and enforce delivery boundaries.

The actual OpenAI Decisions endpoint passed a predicate/choice/score check. All five providers have real-process protocol tests covering authentication, payload translation, normalization, malformed responses, refusal, and cancellation. JEV, Microsoft Decision One, H2O, and local-decisions have not been live-validated here because their credentials or running endpoints were unavailable. These checks establish integration behavior, not calibration or quality for an operational routing policy.

References: [OpenAI Decisions](https://developers.openai.com/api/docs/guides/decisions), [JEV SystemOne](https://docs.typesafe.ai/api/system-one), [Microsoft Decision One](https://learn.microsoft.com/en-us/azure/foundry/foundry-models/how-to/use-foundry-models-microsoft-decision), [H2O Lightning](https://huggingface.co/h2oai/h2o-lightning-4b), and [Amplifier local-decisions](https://github.com/microsoft/amplifier-module-provider-local-decisions).
