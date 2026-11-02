# Moving public conversation history into Constellation

The native runtime owns its saved conversations. Its read-only `history-query`
projection exposes them to an authorized context service without exporting and
reimporting each turn. External systems retain their own membership and context
connectors.

For a one-time engine change, `scripts/export-public-acp.py` can obtain a public
text projection through an existing agent's standard `session/list` and
`session/load` methods. It does not issue a model prompt. Stop active writers
first; any failed replay fails the export instead of silently omitting a session.

Run `agent-runtime --home ENGINE_HOME history-import PRIVATE_PACKET.json` against
the new engine. The packet uses `constellation.public-history/1`; see
`tests/migration.rs` for its schema. Session IDs and titles are retained. The
import is atomic, refuses collisions and conflicting reimports, and does not
rewind a session when the same packet is imported again after new work.

This transfers visible user and assistant text, not executable continuation
state. Provider reasoning, tool transcripts, credentials, permissions and pending
operations are excluded. Non-text content is marked as omitted; preserve the
original engine files if those are needed. Imported sessions use the new
installation's provider and permission policy. Reads remain subject to the
context service's current grants; import does not grant access.

The fleet migration retains the original engine folder and takes a private
configuration/database backup before switching the executable. Rollback selects
the previous engine and configuration; it never overwrites conversations created
after the cutover.
