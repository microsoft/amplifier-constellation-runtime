# Test-only signing material

`oidc-test-private.pem` and the matching `oidc-test-jwks.json` public key are deliberately public,
synthetic test fixtures. They sign fake OIDC tokens for local verification tests.
They are not credentials for an account or service and must never be used in a
deployment. No live identity-provider key is included.

`mcp-tls/` contains deliberately public certificate authorities and a loopback
server certificate/key for the HTTPS MCP transport tests. The server certificate
names only `127.0.0.1`; `localhost` deliberately fails hostname verification.
The certificates expire in 2126. They need no external service or certificate
generation tool at test time and must never be used by an installation.
