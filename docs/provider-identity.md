# Saved-session provider identity

The runtime binds saved conversations to their provider, endpoint, model and API
version. An explicit ChatGPT account registration is also part of that identity.
Rotating an API credential does not change it. An incompatible installation
configuration remains a rejected continuation. Explicit per-session model
selection follows the [ACP capability contract](acp-optional-capabilities.md#session-model-and-reasoning-choices):
configured same-route choices permit empty or ordinary text-only histories,
while tool, media, private-continuation and unknown history shapes refuse a model
change. The runtime never transfers opaque provider state automatically.

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
