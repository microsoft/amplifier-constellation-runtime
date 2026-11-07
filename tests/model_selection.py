"""Actual ACP subprocess and local provider, including durable choices and refusals."""
from contextlib import closing
import json
import sqlite3
import subprocess
import unittest

import protocol


class Tests(unittest.TestCase):
    setUpClass = classmethod(protocol.Tests.setUpClass.__func__)
    tearDownClass = classmethod(protocol.Tests.tearDownClass.__func__)
    tearDown = protocol.Tests.tearDown
    client = protocol.Tests.client
    new = protocol.Tests.new
    prompt = protocol.Tests.prompt

    def setUp(self):
        protocol.Tests.setUp(self)
        self.path = self.home / 'runtime.json'
        config = json.loads(self.path.read_text())
        config['model_options'] = [
            {'model': 'fixture', 'name': 'Default', 'reasoning_efforts': ['default', 'low', 'high']},
            {'model': 'fixture-fast', 'name': 'Fast', 'reasoning_effort': 'low',
             'reasoning_efforts': ['low', 'medium']},
        ]
        self.path.write_text(json.dumps(config))

    def select(self, client, sid, key, value):
        return client.call('session/set_config_option',
                           {'sessionId': sid, 'configId': key, 'value': value})

    def snapshot(self, sid):
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            return {table: db.execute(f'SELECT * FROM {table} WHERE '
                                     f'{"id" if table == "sessions" else "session"}=?',
                                     (sid,)).fetchall()
                    for table in ['sessions', 'session_providers', 'session_model_choices',
                                  'turn_inputs', 'turns', 'context_checkpoints', 'events']}

    def test_choice_survives_restart_without_changing_installation_or_other_session(self):
        original = self.path.read_bytes()
        c = self.client()
        sid = self.new(c)
        result = self.select(c, sid, 'model', 'fixture-fast')
        self.assertIn('result', result)
        values = {x['id']: x['currentValue'] for x in result['result']['configOptions']}
        self.assertEqual(values, {'mode': 'ask', 'model': 'fixture-fast', 'reasoning_effort': 'low'})
        self.assertIn('result', self.select(c, sid, 'reasoning_effort', 'medium'))
        self.prompt(c, sid, 'remember apricot')
        self.assertEqual(protocol.REQUESTS[-1]['model'], 'fixture-fast')
        self.assertEqual(protocol.REQUESTS[-1]['reasoning']['effort'], 'medium')
        other = self.new(c)
        self.prompt(c, other, 'another conversation')
        self.assertEqual(protocol.REQUESTS[-1]['model'], 'fixture')
        c.close()
        self.clients.remove(c)
        c = self.client()
        loaded = c.call('session/load', {'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []})
        values = {x['id']: x['currentValue'] for x in loaded['result']['configOptions']}
        self.assertEqual(values['model'], 'fixture-fast')
        self.assertEqual(values['reasoning_effort'], 'medium')
        self.prompt(c, sid, 'recall')
        self.assertEqual(protocol.REQUESTS[-1]['model'], 'fixture-fast')
        self.assertIn('remember apricot', json.dumps(protocol.REQUESTS[-1]['input']))
        self.assertEqual(self.path.read_bytes(), original)

    def test_other_loaded_connection_reads_latest_durable_selection_at_turn_boundary(self):
        c = self.client()
        sid = self.new(c)
        other = self.client()
        other.call('session/load', {'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []})
        self.assertIn('result', self.select(c, sid, 'model', 'fixture-fast'))
        self.prompt(other, sid, 'fresh input')
        self.assertEqual(protocol.REQUESTS[-1]['model'], 'fixture-fast')

    def test_text_history_switch_preserves_original_items_and_unknown_receipts(self):
        c = self.client()
        sid = self.new(c)
        self.prompt(c, sid, 'retained text')
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
            db.execute("INSERT INTO turn_inputs(session,turn,input_id,item,state) VALUES(?,?,?,?,'unknown')",
                       (sid, 'old-turn', 'unknown-old-input', '{"role":"user","content":"NEVER_REPLAY"}'))
        before = self.snapshot(sid)
        self.assertIn('result', self.select(c, sid, 'model', 'fixture-fast'))
        after = self.snapshot(sid)
        for field in ['sessions', 'turn_inputs', 'turns', 'context_checkpoints']:
            self.assertEqual(before[field], after[field])
        self.prompt(c, sid, 'new request')
        self.assertIn('retained text', json.dumps(protocol.REQUESTS[-1]['input']))
        self.assertNotIn('NEVER_REPLAY', json.dumps(protocol.REQUESTS[-1]['input']))

    def test_opaque_or_effect_history_refuses_without_normalization(self):
        c = self.client()
        for item in [
            {'type': 'reasoning', 'encrypted_content': 'opaque'},
            {'type': 'provider_state', 'provider': 'anthropic', 'message': {'content': []}},
            {'type': 'function_call', 'call_id': 'old-effect', 'name': 'workspace_write', 'arguments': '{}'},
            {'role': 'user', 'content': [{'type': 'input_image', 'image_url': 'data:image/png;base64,AAAA'}]},
        ]:
            sid = self.new(c)
            with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
                db.execute('UPDATE sessions SET items=? WHERE id=?', (json.dumps([item]), sid))
            before = self.snapshot(sid)
            result = self.select(c, sid, 'model', 'fixture-fast')
            self.assertIn('error', result)
            self.assertIn('saved history is preserved', result['error']['message'])
            self.assertEqual(before, self.snapshot(sid))

    def test_invalid_values_are_atomic_and_do_not_change_permissions(self):
        c = self.client()
        sid = self.new(c)
        before = self.snapshot(sid)
        for key, value in [('model', 'unlisted'), ('reasoning_effort', 'max'),
                           ('model', {'endpoint': 'https://other.invalid'}), ('endpoint', 'other')]:
            self.assertEqual(self.select(c, sid, key, value)['error']['code'], -32602)
            self.assertEqual(before, self.snapshot(sid))

    def test_active_turn_refuses_model_and_reasoning_changes(self):
        c = self.client()
        sid = self.new(c)
        other = self.client()
        other.call('session/load', {'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []})
        protocol.CLOSE_STARTED.clear()
        protocol.CLOSE_RELEASE.clear()
        request = c.request('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'hold-for-close'}]})
        try:
            self.assertTrue(protocol.CLOSE_STARTED.wait(5))
            for key, value in [('model', 'fixture-fast'), ('reasoning_effort', 'high')]:
                self.assertIn('error', self.select(other, sid, key, value))
        finally:
            protocol.CLOSE_RELEASE.set()
        self.assertIn('result', c.finish(request))
        self.assertEqual(protocol.REQUESTS[-1]['model'], 'fixture')

    def test_changed_account_or_retired_model_cannot_fall_back(self):
        c = self.client()
        sid = self.new(c)
        self.select(c, sid, 'model', 'fixture-fast')
        self.prompt(c, sid, 'retained conversation must not replay on failed load')
        original = json.loads(self.path.read_text())
        before = self.snapshot(sid)
        for kind in ['account', 'retired']:
            config = json.loads(json.dumps(original))
            if kind == 'account':
                config['provider']['api_key_env'] = 'DIFFERENT_ACCOUNT_SELECTOR'
            else:
                config['model_options'] = config['model_options'][:1]
            self.path.write_text(json.dumps(config))
            other = self.client()
            count = len(protocol.REQUESTS)
            result = other.call('session/load', {'sessionId': sid, 'cwd': str(self.work), 'mcpServers': []})
            self.assertIn('error', result)
            self.assertEqual(other.updates, [])
            self.assertEqual(len(protocol.REQUESTS), count)
            self.assertEqual(before, self.snapshot(sid))
        self.path.write_text(json.dumps(original))

    def test_reasoning_default_is_only_available_when_explicitly_offered(self):
        c = self.client()
        sid = self.new(c)
        self.assertIn('result', self.select(c, sid, 'reasoning_effort', 'high'))
        self.assertIn('result', self.select(c, sid, 'reasoning_effort', 'default'))
        self.select(c, sid, 'model', 'fixture-fast')
        before = self.snapshot(sid)
        self.assertEqual(self.select(c, sid, 'reasoning_effort', 'default')['error']['code'], -32602)
        self.assertEqual(before, self.snapshot(sid))
        self.assertIn('result', c.call('session/delete', {'sessionId': sid}))
        self.assertEqual(self.snapshot(sid)['session_model_choices'], [])

    def test_invalid_owner_options_fail_configuration_without_touching_history(self):
        original = json.loads(self.path.read_text())
        for mutate in [
            lambda c: c['model_options'].append(c['model_options'][0]),
            lambda c: c['model_options'][0].update(reasoning_efforts=['bogus']),
            lambda c: c['model_options'][0].update(reasoning_effort='low'),
            lambda c: c['model_options'][0].update(endpoint='https://other.invalid'),
        ]:
            config = json.loads(json.dumps(original))
            mutate(config)
            self.path.write_text(json.dumps(config))
            result = subprocess.run([str(protocol.BINARY), '--home', str(self.home), 'capabilities'],
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0, result.stdout)
        self.path.write_text(json.dumps(original))


if __name__ == '__main__':
    unittest.main(verbosity=2)
