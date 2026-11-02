# Configure one runtime

The management interface is optional and specific to this engine. It does not extend ACP or make other ACP implementations depend on our configuration format. An Agent Instance host can discover this interface, render its own controls, or embed its existing single-agent editor.

All configuration operations are available without a browser:

```sh
agent-runtime config-schema
agent-runtime --home /absolute/engine config-inspect
agent-runtime --home /absolute/engine config-validate /absolute/candidate.json
agent-runtime --home /absolute/engine configure /absolute/candidate.json \
  --expected-revision SHA256_FROM_INSPECT
```

`config-schema` returns JSON Schema plus `x-agent-runtime`: contract version, engine version, human-readable sections, provider presets, decision presets, credential policy, and application semantics. `config-inspect` returns the complete configuration, its revision, and capability report. It never reads secret values. `config-validate` checks a complete proposed configuration against the running version without saving it. Unknown fields and unsupported capability/provider combinations are errors.

`configure` requires the revision from inspection. A concurrent edit is rejected without overwriting it. The previous file is preserved under `configuration-backups/`; the response includes its path. Updates are private atomic replacements under a process lock. `configure-provider`, `configure-capabilities`, and `configure-decision` use the same mechanism.

Changes apply to **new ACP processes**, not already executing turns. Stop or finish existing work before reconnecting. A changed model/provider/endpoint/account requires a new engine session; historical sessions remain inspectable, and their original configuration can still continue them. The host should invalidate any scheduler approval that covered the prior execution configuration.

Configuration contains credential references: environment variable names for API keys, and a private credentials-file path for an explicitly connected ChatGPT account. Keep actual secrets in the host's private secret store or the account flow. Workspace file tools cannot read the runtime home. Do not place credentials in instructions, profiles, model options, or the exposed workspace.

The API is intentionally sufficient for a CLI, the packaged single-agent UI, an external fleet UI, and an agent performing authorized setup. The higher-level host owns authentication, view state, presentation, and launching/restarting the engine.
