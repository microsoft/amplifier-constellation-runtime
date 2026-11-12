# Validation

Run the repository's automated checks from a clean checkout. The source fixtures
use fictional identities and temporary state. They do not contain installed
agents, personal profiles, account credentials, or exported conversations.

```sh
cargo fmt --check
cargo test --locked
cargo build --locked
python3 tests/protocol.py
python3 tests/providers.py
python3 tests/capabilities.py
python3 tests/management.py
cargo clippy --locked --all-targets -- -D warnings
```

See the test scripts and CI workflow for prerequisites and platform conditions.
The native matrix also runs protocol, permission, session-load and helper checks
against the release executable before packaging. `python3 tests/package_release.py`
checks architecture refusal, Windows runtime imports, version/provenance binding
and reproducible archive contents. The package script needs the actual native
executable for its version check; a cross-build alone does not complete that step.
Browser tests use disposable local services and require Playwright/Chromium.
Linux service tests may require PAM development headers; isolated command tests
also need the documented OS isolation prerequisites. Opt-in real account, model,
or messaging tests remain separate from deterministic checks.

The [Actions page](https://github.com/microsoft/amplifier-constellation-runtime/actions) records checks
against the published source. Earlier private deployment reports and their
transcripts are excluded. A passing fixture verifies its tested boundary, not
general agent quality, human approval, or live tenant compatibility.
