# Test-only signing material

`oidc-test-private.pem` and the matching `oidc-test-jwks.json` public key are deliberately public,
synthetic test fixtures. They sign fake OIDC tokens for local verification tests.
They are not credentials for an account or service and must never be used in a
deployment. No live identity-provider key is included.
