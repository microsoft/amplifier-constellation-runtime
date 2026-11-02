# Connect a ChatGPT subscription

Version 0.4 implements OpenAI's public **Sign in with ChatGPT for open-source agents** flow directly in Rust. It has its own registration and private credential store. It never reads or repurposes Codex credentials. The model/tool loop remains this runtime's loop.

On the machine where the browser runs:

```sh
agent-runtime --home /absolute/engine chatgpt-sign-in
agent-runtime --home /absolute/engine chatgpt-status
agent-runtime --home /absolute/engine chatgpt-models
agent-runtime --home /absolute/engine configure-provider \
  --provider openai --model ACCOUNT_MODEL_SLUG --auth chatgpt \
  --chatgpt-credentials-file /absolute/engine/accounts/chatgpt/credentials.json
```

The browser asks the user to authorize this installation. Granting account identity alone does not grant plan inference: `chatgpt-status` reports whether plan sharing was granted. Choose a slug actually returned by `chatgpt-models`; the runtime does not silently substitute an API key or a different model. Responses requests use the public endpoint with `store: false` and streaming.

For a headless server, forward its loopback callback to the browser computer, then sign in on the server with `--no-browser`:

```sh
# Run on the browser computer; keep this connection open.
ssh -L 1455:127.0.0.1:1455 your-server
# In that remote shell:
agent-runtime --home /absolute/engine chatgpt-sign-in --no-browser --port 1455
```

Open the printed authorization URL on the browser computer. Port 1455 is a default, not a required port; both sides of the forwarding and `--port` must match. The callback listens only on loopback and expires after five minutes. The URL contains ephemeral sign-in material, not a saved access token.

For deployments where forwarding is impractical, the official self-hosted VM flow permits transferring this application's own credential record securely. Sign in using a separate local runtime home, transfer only its private `accounts/chatgpt/credentials.json` over SSH, then run `chatgpt-import /absolute/private-transfer.json` on the server. On Unix that file must be owner-readable only. Import verifies the account's signed ID token, assigns the server its own host identity, and refuses to replace an established different account. Remove transfer files and stop using the source authorization so the server is the sole refresh owner. Imported identity tokens must still be valid; sign in again before transferring an expired record. Prefer loopback forwarding when possible.

The runtime verifies signature, issuer, audience, expiry, nonce, state, and PKCE. It saves the issued dynamic client registration, preserves the account binding through sign-out, refreshes tokens under a cross-process lock, and stops if sharing is withdrawn. A different ChatGPT account belongs in a different runtime home. `chatgpt-sign-out` revokes the account session before deleting credentials; registration and history remain. Failed revocation preserves credentials for a deliberate retry. Account usage is visible at [ChatGPT usage settings](https://chatgpt.com/settings/usage).

Validation covers callback rejection, local PKCE flow, JWT signature and claim verification, and configuration boundaries. **An actual user sign-in, plan entitlement, refresh, and inference remain to be exercised with new user consent.** These are not established by protocol tests or by the presence of an API key.

Official references: [overview](https://developers.openai.com/cookbook/articles/sign-in-with-chatgpt), [registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in), [models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference), and [self-hosted VMs](https://developers.openai.com/siwc/token-sharing-open-source/self-hosted-vms).
