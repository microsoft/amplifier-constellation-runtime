"""Saved provider identity across JSON feature changes; local fixture only."""
from contextlib import closing
import hashlib
import json
import sqlite3
import unittest

import protocol


def provider_key(provider, *, sorted_keys):
    # These are the two byte encodings actually written by released runtimes.
    identity = {key: provider.get(key) for key in
                ('kind', 'endpoint', 'model', 'api_version')}
    if provider.get('auth') == 'chatgpt':
        identity['chatgpt_credentials_file'] = provider['chatgpt_credentials_file']
    return hashlib.sha256(json.dumps(identity, sort_keys=sorted_keys,
                                    separators=(',', ':')).encode()).hexdigest()


class Tests(unittest.TestCase):
    setUpClass = classmethod(protocol.Tests.setUpClass.__func__)
    tearDownClass = classmethod(protocol.Tests.tearDownClass.__func__)
    setUp = protocol.Tests.setUp
    tearDown = protocol.Tests.tearDown
    client = protocol.Tests.client
    new = protocol.Tests.new
    prompt = protocol.Tests.prompt

    def seed(self, client, key):
        sid = self.new(client)
        client.call('session/close', {'sessionId': sid})
        history = [
            {'role': 'user', 'content': 'Original saved conversation'},
            {'type': 'function_call', 'call_id': 'settled-before-upgrade',
             'name': 'workspace_write', 'arguments': '{"path":"old-effect"}'},
            {'type': 'function_call_output', 'call_id': 'settled-before-upgrade',
             'output': 'Previously completed; do not repeat'},
        ]
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
            db.execute('UPDATE sessions SET items=? WHERE id=?',
                       (json.dumps(history), sid))
            db.execute('INSERT INTO session_providers VALUES (?,?)', (sid, key))
            db.execute('INSERT INTO turn_inputs(session,turn,input_id,item,state) '
                       "VALUES (?,?,?,?,'unknown')",
                       (sid, 'old-turn', 'uncertain-before-upgrade',
                        json.dumps({'role': 'user', 'content': 'NEVER-REPLAY-THIS'})))
        return sid, history

    def snapshot(self, sid):
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
            return {
                'binding': db.execute('SELECT provider_key FROM session_providers '
                                      'WHERE session=?', (sid,)).fetchone()[0],
                'history': json.loads(db.execute('SELECT items FROM sessions WHERE id=?',
                                                (sid,)).fetchone()[0]),
                'receipts': db.execute('SELECT * FROM turn_inputs WHERE session=?',
                                       (sid,)).fetchall(),
                'turns': db.execute('SELECT * FROM turns WHERE session=?',
                                    (sid,)).fetchall(),
            }

    def test_both_released_encodings_resume_without_reset_or_replay(self):
        provider = json.loads((self.home / 'runtime.json').read_text())['provider']
        for sorted_keys in (True, False):
            with self.subTest(sorted_keys=sorted_keys):
                client = self.client()
                key = provider_key(provider, sorted_keys=sorted_keys)
                sid, history = self.seed(client, key)
                before = self.snapshot(sid)
                client.close()
                self.clients.remove(client)
                client = self.client()
                loaded = client.call('session/load', {
                    'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []})
                self.assertIn('result', loaded)
                self.assertEqual(before, self.snapshot(sid))
                response = self.prompt(client, sid, 'A fresh independent greeting')
                self.assertEqual(response.get('result', {}).get('stopReason'),
                                 'end_turn', response)
                after = self.snapshot(sid)
                self.assertEqual(after['binding'], key)
                self.assertEqual(after['receipts'], before['receipts'])
                self.assertEqual(after['history'][:len(history)], history)
                self.assertEqual(len(after['turns']), 1)
                self.assertFalse((self.work / 'old-effect').exists())
                self.assertNotIn('NEVER-REPLAY-THIS', json.dumps(protocol.REQUESTS[-1]['input']))

    def test_actual_destination_changes_reject_before_input_or_model(self):
        provider = json.loads((self.home / 'runtime.json').read_text())['provider']
        for sorted_keys in (True, False):
            for field, value in (('model', 'different-model'),
                                 ('endpoint', 'https://different.example/responses'),
                                 ('kind', 'azure-openai'), ('api_version', 'different-version')):
                with self.subTest(sorted_keys=sorted_keys, field=field):
                    client = self.client()
                    old = dict(provider, **{field: value})
                    sid, _ = self.seed(client, provider_key(old, sorted_keys=sorted_keys))
                    self.assertIn('result', client.call('session/load', {
                        'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []}))
                    before = self.snapshot(sid)
                    count = len(protocol.REQUESTS)
                    response = self.prompt(client, sid, 'Do not send this to another provider')
                    self.assertIn('error', response)
                    self.assertIn('different provider', response['error']['message'])
                    self.assertEqual(self.snapshot(sid), before)
                    self.assertEqual(len(protocol.REQUESTS), count)
                    with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
                        events = [json.loads(row[0]) for row in db.execute(
                            "SELECT data FROM events WHERE session=? AND kind='turn.rejected'", (sid,))]
                    self.assertEqual(events, [{'stage': 'provider_binding',
                                              'reason': 'provider_identity_mismatch',
                                              'input_recorded': False, 'model_started': False}])


if __name__ == '__main__':
    unittest.main()
