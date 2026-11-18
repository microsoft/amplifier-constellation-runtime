"""Actual owner CLI and ACP reload of saved choices; disposable state, no prompts."""
from contextlib import closing
import copy
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import unittest
import uuid

import protocol

RECONCILIATION_BINARY = Path(os.environ.get(
    'NATIVE_TOOLS_RECONCILIATION_BINARY', str(protocol.BINARY)))


def digest(data):
    return hashlib.sha256(data).hexdigest()


class Tests(unittest.TestCase):
    setUpClass = classmethod(protocol.Tests.setUpClass.__func__)
    tearDownClass = classmethod(protocol.Tests.tearDownClass.__func__)
    client = protocol.Tests.client
    new = protocol.Tests.new

    def setUp(self):
        protocol.Tests.setUp(self)
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self.close_clients)
        self.config_path = self.home / 'runtime.json'
        self.request_count = len(protocol.REQUESTS)
        config = json.loads(self.config_path.read_text())
        config['model_options'] = [
            {'model': 'fixture', 'name': 'Default'},
            {'model': 'fixture-fast', 'name': 'Selected',
             'reasoning_effort': 'low', 'reasoning_efforts': ['low', 'high']},
        ]
        self.configure(config)
        client = self.client()
        self.original = self.selected_session(client)
        self.close_clients()
        self.seed(self.original, 'original')
        config['provider'].setdefault('native', {})['tools'] = [{'type': 'web_search'}]
        changed = self.configure(config)
        self.previous = Path(changed['previous_config_file'])
        self.current_config = self.config_path.read_bytes()
        self.previous_config = self.previous.read_bytes()
        client = self.client()
        self.other = self.selected_session(client)
        self.close_clients()
        self.seed(self.other, 'other-current-route')

    def close_clients(self):
        while self.clients:
            client = self.clients.pop()
            try:
                client.close()
            finally:
                if client.proc.poll() is None:
                    client.proc.terminate()
                    try:
                        client.proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        client.proc.kill()
                        client.proc.wait(timeout=5)
                for stream in (client.proc.stdin, client.proc.stdout, client.proc.stderr):
                    if stream is not None and not stream.closed:
                        stream.close()

    def cli(self, *arguments, success=True):
        result = subprocess.run(
            [str(RECONCILIATION_BINARY), '--home', str(self.home), *map(str, arguments)],
            capture_output=True, text=True, encoding='utf-8', timeout=15)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def configure(self, config):
        candidate = Path(self.tmp.name) / 'candidate.json'
        candidate.write_text(json.dumps(config), encoding='utf-8')
        return self.cli('configure', candidate, '--expected-revision',
                        digest(self.config_path.read_bytes()))

    def selected_session(self, client):
        session = self.new(client)
        for key, value in [('model', 'fixture-fast'), ('reasoning_effort', 'high')]:
            result = client.call('session/set_config_option',
                                 {'sessionId': session, 'configId': key, 'value': value})
            self.assertIn('result', result)
        self.assertIn('result', client.call('session/close', {'sessionId': session}))
        return session

    def seed(self, session, marker):
        # These are retained fixture records, never newly submitted work. Include
        # opaque continuation and effects so a destructive "repair" cannot pass.
        history = [
            {'role': 'user', 'content': f'{marker}: retained conversation'},
            {'type': 'provider_state', 'provider': 'anthropic',
             'message': {'content': [{'type': 'thinking', 'thinking': 'opaque fixture',
                                      'signature': 'retained-signature'}]}},
            {'type': 'function_call', 'call_id': f'{marker}-settled-effect',
             'name': 'workspace_write',
             'arguments': json.dumps({'path': 'must-not-exist', 'content': 'never replay'})},
            {'type': 'function_call_output', 'call_id': f'{marker}-settled-effect',
             'output': 'Previously completed; preserve this receipt'},
        ]
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
            db.execute('UPDATE sessions SET items=? WHERE id=?', (json.dumps(history), session))
            db.execute("INSERT INTO turns VALUES (?,?,'completed',?,?)",
                       (f'{marker}-old-turn', session, '2026-01-01T00:00:00Z',
                        '2026-01-01T00:00:01Z'))
            db.execute('INSERT INTO turn_inputs(session,turn,input_id,item,state,reason) '
                       "VALUES (?,?,?,?,'unknown','retained_unknown')",
                       (session, f'{marker}-old-turn', f'{marker}-unknown-input',
                        json.dumps({'role': 'user', 'content': 'NEVER-REPLAY-UNKNOWN'})))
            provider_key = db.execute('SELECT provider_key FROM session_providers '
                                      'WHERE session=?', (session,)).fetchone()[0]
            db.execute('INSERT INTO context_checkpoints VALUES (?,?,?,?,?)',
                       (session, digest(json.dumps(history).encode()), provider_key,
                        '2026-01-01T00:00:01Z', '{"fixture":"original checkpoint"}'))

    def snapshot(self):
        with closing(sqlite3.connect(
                (self.home / 'history.sqlite3').as_uri() + '?mode=ro', uri=True)) as db:
            names = [row[0] for row in db.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name")]
            return {name: db.execute(f'SELECT * FROM "{name}" ORDER BY rowid').fetchall()
                    for name in names}

    def prepare(self):
        before = self.snapshot()
        files = sorted(str(p.relative_to(self.home)) for p in self.home.rglob('*'))
        plan = self.cli('reconcile-native-tools', 'prepare', '--session', self.original,
                        '--previous-config', self.previous)
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(sorted(str(p.relative_to(self.home)) for p in self.home.rglob('*')),
                         files, 'Preparation must not create receipt or lock files')
        self.assertEqual(plan['session_id'], self.original)
        self.assertEqual(str(uuid.UUID(plan['operation_id'])), plan['operation_id'])
        self.assertEqual(plan['previous_config_sha256'], digest(self.previous_config))
        self.assertEqual(plan['current_config_sha256'], digest(self.current_config))
        self.assertEqual(plan['previous_tools'], [])
        self.assertEqual(plan['current_tools'], [{'type': 'web_search'}])
        self.assertNotEqual(plan['previous_route'], plan['current_route'])
        return plan

    def apply(self, plan, **kwargs):
        path = Path(self.tmp.name) / 'plan.json'
        path.write_text(json.dumps(plan), encoding='utf-8')
        return self.cli('reconcile-native-tools', 'apply', path, **kwargs)

    def inspect(self, plan):
        return self.cli('reconcile-native-tools', 'inspect',
                        '--operation', plan['operation_id'])

    def load(self, client, session):
        return client.call('session/load',
                           {'sessionId': session, 'cwd': str(self.work), 'mcpServers': []})

    def assert_no_work(self):
        self.assertEqual(len(protocol.REQUESTS), self.request_count)
        self.assertFalse((self.work / 'must-not-exist').exists())
        self.assertEqual(self.config_path.read_bytes(), self.current_config)
        self.assertEqual(self.previous.read_bytes(), self.previous_config)
        for client in self.clients:
            self.assertEqual(client.callbacks, [])

    def test_original_acp_load_repaired_without_replay_or_other_session_change(self):
        before = self.snapshot()
        client = self.client()
        refused = self.load(client, self.original)
        self.assertIn('error', refused)
        self.assertIn('different provider route', refused['error']['message'])
        self.assertEqual(client.updates, [], 'Refusal precedes history replay')
        self.assertEqual(self.snapshot(), before)
        self.assertIn('result', self.load(client, self.other))
        self.assertEqual(self.snapshot(), before)
        self.close_clients()

        plan = self.prepare()
        self.assertEqual(self.inspect(plan)['status'], 'not_committed')
        self.assertEqual(self.snapshot(), before)
        receipt = self.apply(plan)
        self.assertEqual(receipt['status'], 'committed')
        self.assertEqual(receipt['plan'], plan)
        self.assertFalse(receipt['work_started'])
        self.assertTrue(receipt['history_preserved'] and receipt['inputs_preserved'])
        after = self.snapshot()
        for name in before.keys() | after.keys():
            if name not in ('session_model_choices', 'events'):
                self.assertEqual(after[name], before[name], name)
        old_choices = dict(before['session_model_choices'])
        new_choices = dict(after['session_model_choices'])
        self.assertEqual(new_choices[self.other], old_choices[self.other])
        old = json.loads(old_choices[self.original])
        new = json.loads(new_choices[self.original])
        self.assertEqual(new, dict(old, route=plan['current_route']))
        self.assertEqual(receipt['previous_choice_json'], old_choices[self.original])
        self.assertEqual(receipt['current_choice_json'], new_choices[self.original])
        self.assertEqual(after['events'][:-1], before['events'])
        event = after['events'][-1]
        self.assertEqual(event[1:4], (self.original, None, 'session.native_tools_reconciled'))
        self.assertEqual(json.loads(event[5]), receipt)

        self.assertEqual(self.inspect(plan), receipt)
        self.assertEqual(self.apply(plan), receipt, 'Identical replay returns committed receipt')
        self.assertEqual(self.snapshot(), after)
        conflict = copy.deepcopy(plan)
        conflict['current_route'] = '0' * 64
        self.assertIn('different plan', self.apply(conflict, success=False).stderr)
        self.assertEqual(self.snapshot(), after)

        client = self.client()
        loaded = self.load(client, self.original)
        self.assertIn('result', loaded)
        choices = {v['id']: v['currentValue'] for v in loaded['result']['configOptions']}
        self.assertEqual(choices['model'], 'fixture-fast')
        self.assertEqual(choices['reasoning_effort'], 'high')
        self.assertIn('original: retained conversation', json.dumps(client.updates))
        self.assertNotIn('NEVER-REPLAY-UNKNOWN', json.dumps(client.updates))
        self.assertIn('result', self.load(client, self.other))
        self.assertEqual(self.snapshot(), after)
        self.assert_no_work()
        print(json.dumps({'fixture': 'native-tools-reconciliation',
                          'original_session_preserved': True, 'sessions': len(after['sessions']),
                          'retained_turns': len(after['turns']),
                          'unknown_inputs': sum(row[5] == 'unknown' for row in after['turn_inputs']),
                          'owner_events_added': len(after['events']) - len(before['events']),
                          'provider_requests': len(protocol.REQUESTS) - self.request_count,
                          'permission_callbacks': sum(len(c.callbacks) for c in self.clients)}))

    def test_cli_refuses_config_and_session_drift_without_committing(self):
        plan = self.prepare()
        for change in ('configuration', 'session', 'foreign_native_grant'):
            with self.subTest(change=change):
                before = self.snapshot()
                config = json.loads(self.current_config)
                if change == 'configuration':
                    config['provider']['api_key_env'] = 'DIFFERENT_FIXTURE_ACCOUNT'
                    self.config_path.write_text(json.dumps(config), encoding='utf-8')
                elif change == 'foreign_native_grant':
                    config['provider']['native']['image_input'] = True
                    self.config_path.write_text(json.dumps(config), encoding='utf-8')
                else:
                    with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
                        db.execute('UPDATE sessions SET title=? WHERE id=?',
                                   ('A concurrent owner change', self.original))
                changed = self.snapshot()
                refused = self.apply(plan, success=False)
                self.assertTrue(refused.stderr.strip())
                self.assertEqual(self.snapshot(), changed)
                self.assertEqual(self.inspect(plan)['status'], 'not_committed')
                self.config_path.write_bytes(self.current_config)
                if change == 'session':
                    original_title = next(row[3] for row in before['sessions']
                                          if row[0] == self.original)
                    with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db, db:
                        db.execute('UPDATE sessions SET title=? WHERE id=?',
                                   (original_title, self.original))
                self.assertEqual(self.snapshot(), before)
        self.assert_no_work()


if __name__ == '__main__':
    unittest.main(verbosity=2)
