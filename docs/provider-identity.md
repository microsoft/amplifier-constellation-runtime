# Saved-session provider identity

The runtime binds saved conversations to their provider, endpoint, model and API
version. An explicit ChatGPT account registration is also part of that identity.
Rotating an API credential does not change it. An incompatible installation
configuration remains a rejected continuation. Explicit per-session model
selection follows the [ACP capability contract](acp-optional-capabilities.md#session-model-and-reasoning-choices):
configured same-route choices permit empty or ordinary text-only histories,
while tool, media, private-continuation and unknown history shapes refuse a model
change. The runtime never transfers opaque provider state automatically.

## Reconcile a provider-hosted tool change

A saved model choice also records the installation's provider configuration,
including its native tool grants. Adding or removing a provider-hosted tool can
therefore prevent an older session from loading even when its provider, account,
endpoint and model are unchanged. Changing the installation back can block newer
sessions that already use the current configuration.

The installation's OS owner can explicitly reconcile **only** a change to
`provider.native.tools` for one existing session. This is local management,
not an ACP option or an agent tool. Keep the session idle and use the original
engine home and a retained configuration backup that matches its saved choice:

```sh
agent-runtime --home /absolute/engine reconcile-native-tools prepare \
  --session SESSION_ID --previous-config /absolute/previous-runtime.json \
  > /private/reconciliation-plan.json
agent-runtime --home /absolute/engine reconcile-native-tools apply \
  /private/reconciliation-plan.json
agent-runtime --home /absolute/engine reconcile-native-tools inspect \
  --operation OPERATION_ID_FROM_PLAN
```

Preparation verifies the evidence and produces a plan bound to the current
configuration and saved session, including the exact old and new tool declarations
for owner review. It does not change the installation or session. The existing
history store, configuration lock and target-session lock must already exist;
missing installation state is refused rather than initialized.
Application checks those bindings again under the configuration and session
locks. It refuses concurrent work, stale evidence, a changed provider or account,
a different model, and changes to other native capabilities. Missing original
evidence is not permission to infer the former configuration.

Successful application updates that session's saved configuration fingerprint and
appends an owner reconciliation receipt. It preserves the original session ID,
model and reasoning choice, provider binding, history, input uncertainty and turn
records. The installation configuration and other sessions remain unchanged.
No provider request, tool execution, session load or recovery runs as part of the
command. Existing unknown work still needs its own supported reconciliation.

Use `inspect` with the plan's operation ID if the application response is lost.
An identical repeated application returns the committed receipt; reusing an
operation ID for a different plan is refused. An `inspect` result of
`not_committed` describes that read; it does not prove an in-flight application
cannot still commit. Retain its operation ID and reconcile its completion.
After a verified result, the
connection owner can restore the original session through its existing lifecycle.
The result does not authorize a new prompt or replay earlier work.

## Fingerprint compatibility

Provider fingerprints use explicit sorted keys, independent of dependency
features. The runtime recognizes both released encodings of the current identity:
the original sorted form and the SDK-era insertion-order form. It leaves existing
bindings, conversation history and input receipts intact. This does not waive
provider isolation, acknowledge uncertain inputs, replay work, or create a new
session. A provider-context cache stored under the alternate identity may require
fresh compaction; its original history remains available.

An actual identity mismatch records a content-free `turn.rejected` event with
`stage=provider_binding`, `reason=provider_identity_mismatch`,
`input_recorded=false` and `model_started=false`. The diagnostics report includes
the rejection count and latest rejection. This distinguishes a startup rejection
from a provider request that failed after work began. Earlier binaries did not
record this preflight rejection; absence of their event is not evidence of success.

[The provider-identity fixture](../tests/provider_identity.py) exercises persisted bindings from both encodings
through real ACP processes and a local provider fixture. It also checks that
changed destinations remain rejected and that completed effects and uncertain
inputs are not replayed. No model account is used by these tests.
