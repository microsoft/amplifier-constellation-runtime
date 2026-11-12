"""Actual ACP/local-provider tests for a settled turn reaching context capacity.

The one effect is a disposable MCP fixture receipt. No real model or existing
session is used, and the next prompt is independently submitted by the client.
"""
import hashlib
import json
import os
import queue
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import protocol
import steering
from responses_ws import ResponsesWebSocketMixin


META = 'io.github.bkrabach.agent-runtime'
REASON = 'active_turn_context_capacity'
RECEIPT_TEXT = 'SETTLED-FIXTURE-RECEIPT\n' + ''.join(
    f'{i:05d}: unique retained measurement for disposable fixture\n'
    for i in range(1600)
)
RECEIPT = {'content': [{'type': 'text', 'text': RECEIPT_TEXT}], 'isError': False}


class Handler(ResponsesWebSocketMixin, BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value, status=200):
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        try:
            self.wfile.write(json.dumps(value).encode())
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass  # An explicit Stop may close this disposable tool connection.

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path == '/mcp':
            method = body.get('method')
            if method.startswith('notifications/'):
                self.send_response(202)
                self.end_headers()
                return
            if method == 'initialize':
                result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}},
                          'serverInfo': {'name': 'capacity-fixture', 'version': '1'}}
            elif method == 'tools/list':
                result = {'tools': [
                    {'name': name, 'description': 'Disposable receipt fixture',
                     'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False},
                     'annotations': {'readOnlyHint': False}}
                    for name in ['capacity_receipt', 'unknown_effect']
                ]}
            elif method == 'tools/call':
                name = body['params']['name']
                self.server.effects.append(name)
                self.server.tool_entered.set()
                if self.server.hold_tool:
                    if not self.server.tool_release.wait(10):
                        raise AssertionError('Test did not release the tool response')
                # A committed fixture effect with a mismatched response ID must
                # remain uncertain; it cannot qualify for a settled limit stop.
                if name == 'unknown_effect':
                    return self.reply({'jsonrpc': '2.0', 'id': 'wrong-request-id',
                                       'result': {'content': [{'type': 'text', 'text': 'Not correlated'}]}})
                result = self.server.receipt
            else:
                raise AssertionError(method)
            return self.reply({'jsonrpc': '2.0', 'id': body['id'], 'result': result})

        if self.path == '/chat/completions':
            assert self.headers.get('Authorization') == 'Bearer fixture'
            self.server.requests.append(body)
            latest = next(m['content'] for m in reversed(body['messages']) if m.get('role') == 'user')
            text = latest if isinstance(latest, str) else ''.join(p.get('text', '') for p in latest)
            if text.startswith('RUN_CAPACITY_TOOL'):
                name = next(t['function']['name'] for t in body['tools']
                            if t['function']['name'].endswith('_capacity_receipt'))
                delta = {'tool_calls': [{'index': 0, 'id': 'local-capacity-call', 'type': 'function',
                                        'function': {'name': name, 'arguments': '{}'}}]}
                reason = 'tool_calls'
            else:
                delta = {'content': 'Fresh request completed without repeating the receipt effect.'}
                reason = 'stop'
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            event = {'choices': [{'delta': delta, 'finish_reason': reason}],
                     'usage': {'prompt_tokens': 50, 'completion_tokens': 20}}
            self.wfile.write(('data: ' + json.dumps(event) + '\n\ndata: [DONE]\n\n').encode())
            self.wfile.flush()
            return

        assert self.path == '/responses', self.path
        assert self.headers.get('Authorization') == 'Bearer fixture'
        self.server.requests.append(body)
        if self.server.provider_error:
            return self.reply({'error': {'code': 'unrelated_fixture_failure'}}, 500)
        text = next(item['content'] for item in reversed(body['input']) if item.get('role') == 'user')
        if text.startswith('RUN_CAPACITY_TOOL'):
            names = ['capacity_receipt']
            if self.server.unknown_effect:
                names.append('unknown_effect')
            output = [
                {'type': 'function_call', 'call_id': f'capacity-call-{i}',
                 'name': next(t['name'] for t in body['tools'] if t['name'].endswith('_' + name)),
                 'arguments': '{}'}
                for i, name in enumerate(names)
            ]
        else:
            output = [{'type': 'message', 'role': 'assistant',
                       'content': [{'type': 'output_text', 'text': 'Fresh request completed without repeating the receipt effect.'}]}]
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        event = {'type': 'response.completed', 'response': {
            'status': 'completed', 'output': output,
            'usage': {'input_tokens': 50, 'output_tokens': 20}}}
        self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


class ControlledHandler(Handler):
    # Reuse the existing event-driven native transport fixture for steering;
    # the ordinary Responses shim intentionally does not emulate that protocol.
    do_GET = steering.Wire.do_GET


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name) / 'home'
        self.work = Path(self.tmp.name) / 'work'
        self.c = None
        self.results = []
        self.boundary_observations = []
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.requests = []
        self.server.effects = []
        self.server.receipt = RECEIPT
        self.server.provider_error = False
        self.server.unknown_effect = False
        self.server.hold_tool = False
        self.server.tool_entered = threading.Event()
        self.server.tool_release = threading.Event()
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        subprocess.run([str(protocol.BINARY), '--home', str(self.home), 'init',
                        '--workspace', str(self.work), '--model', 'fixture'],
                       check=True, capture_output=True)
        path = self.home / 'runtime.json'
        config = json.loads(path.read_text())
        config['provider'].update(endpoint=f'http://127.0.0.1:{self.server.server_port}/responses',
                                  api_key_env='RUNTIME_FIXTURE_KEY', reasoning_effort=None)
        config['context']['provider_compaction'] = False
        config['limits'].update(max_context_bytes=64000, tool_output_bytes=128000)
        path.write_text(json.dumps(config))
        self.mcp = [{'type': 'http', 'name': 'fixture',
                     'url': f'http://127.0.0.1:{self.server.server_port}/mcp', 'headers': []}]
        self.client()
        self.sid = self.c.call('session/new', {'cwd': str(self.work), 'mcpServers': self.mcp})['result']['sessionId']

    def tearDown(self):
        self.server.tool_release.set()
        if self.c:
            self.c.close()
        self.server.shutdown()
        self.server.server_close()
        evidence = os.environ.get('RUNTIME_CONTEXT_CAPACITY_EVIDENCE_DIR')
        if evidence:
            target = Path(evidence) / self._testMethodName
            target.mkdir(parents=True, exist_ok=True)
            with closing(sqlite3.connect(self.home / 'history.sqlite3')) as source:
                with closing(sqlite3.connect(target / 'history.sqlite3')) as destination:
                    source.backup(destination)
            (target / 'result.json').write_text(json.dumps({
                'responses': self.results, 'provider_calls': len(self.server.requests),
                'effect_calls': self.server.effects,
                'canonical_sha256': hashlib.sha256(json.dumps(self.items(), sort_keys=True).encode()).hexdigest(),
                'events': self.events(),
                'boundary_observations': self.boundary_observations,
            }, indent=2) + '\n')
        self.tmp.cleanup()

    def client(self):
        self.c = protocol.Client(self.home)
        self.c.call('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})

    def prompt(self, text):
        response = self.c.call('session/prompt', {'sessionId': self.sid,
                              'prompt': [{'type': 'text', 'text': text}]}, approve=True)
        self.results.append(response)
        return response

    def items(self):
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            return json.loads(db.execute('SELECT items FROM sessions WHERE id=?', (self.sid,)).fetchone()[0])

    def events(self, kind=None):
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            return [{'kind': k, 'data': json.loads(d)} for k, d in db.execute(
                'SELECT kind,data FROM events WHERE session=? ORDER BY seq', (self.sid,))
                if kind is None or kind == k]

    def assert_capacity_diagnostic(self, event, detail, private_values):
        observed = subprocess.run([str(protocol.BINARY), '--home', str(self.home), 'diagnostics'],
                                  check=True, capture_output=True, text=True, timeout=10)
        report = json.loads(observed.stdout)
        summary = report['latest_capacity']
        self.assertEqual(summary['event'], event)
        self.assertEqual(summary['outcome'], 'boundary_closed' if event == 'context.limit' else 'rejected')
        for key, value in detail.items():
            self.assertEqual(summary[key], value, key)
        for private in private_values:
            self.assertNotIn(private, json.dumps(detail))
            self.assertNotIn(private, json.dumps(report))

    def test_settled_capacity_is_a_budget_terminal_and_original_session_can_continue(self):
        response = self.prompt('RUN_CAPACITY_TOOL once, retaining its original receipt')
        self.assertIn('result', response, response)
        self.assertEqual(response['result']['stopReason'], 'max_turn_requests')
        meta = response['result']['_meta'][META]
        self.assertEqual((meta['state'], meta['stopCause']), ('budget_exhausted', REASON))
        self.assertEqual(self.server.effects, ['capacity_receipt'])
        self.assertEqual(len(self.server.requests), 1, 'No second model request may be sent after the local limit')
        self.assertEqual(len(self.c.callbacks), 1)
        limits = self.events('context.limit')
        self.assertEqual(len(limits), 1)
        detail = limits[0]['data']
        self.assertEqual(detail['reason'], REASON)
        self.assertEqual((detail['model_calls'], detail['tool_calls']), (1, 1))
        self.assertTrue(detail['full_history_preserved'])
        self.assertGreater(detail['required_context_bytes'], detail['available_context_bytes'])
        self.assertNotIn('SETTLED-FIXTURE-RECEIPT', json.dumps(detail))
        self.assertEqual(self.events('turn.finished')[-1]['data']['reason'], REASON)
        before = self.items()
        saved_output = next(i['output'] for i in before if i.get('type') == 'function_call_output')
        self.assertEqual(json.loads(saved_output), RECEIPT)
        completed = [u['params']['update'] for u in self.c.updates
                     if u.get('params', {}).get('update', {}).get('sessionUpdate') == 'tool_call_update'
                     and u['params']['update'].get('status') == 'completed']
        self.assertEqual(len(completed), 1)
        self.assertEqual(json.loads(completed[0]['content'][0]['content']['text']), RECEIPT)
        self.c.close()
        self.client()
        loaded = self.c.call('session/load', {'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp})
        self.assertIn('result', loaded)
        self.assertEqual(self.items(), before)
        self.assertEqual((len(self.server.requests), self.server.effects), (1, ['capacity_receipt']))
        response = self.prompt('FRESH_CONTINUATION: inspect the recorded outcome without repeating its effect')
        self.assertEqual(response['result']['stopReason'], 'end_turn')
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(self.server.effects, ['capacity_receipt'])
        self.assertFalse(self.c.callbacks, 'Loading and continuing must not ask to repeat the old MCP effect')
        self.assertEqual(self.items()[:len(before)], before)
        self.assertIn('session_history', json.dumps(self.server.requests[-1]['input']))
        self.assertEqual(len(self.events('context.limit')), 1)

    def test_initial_oversized_request_remains_an_error(self):
        response = self.prompt('INITIAL_OVERSIZED_REQUEST ' + 'x' * 100000)
        self.assertEqual(response['error']['code'], -32603)
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.server.effects, [])
        self.assertEqual(self.events('context.limit'), [])
        self.assertEqual(self.events('turn.finished')[-1]['data']['state'], 'failed')
        self.assertIn('INITIAL_OVERSIZED_REQUEST', self.items()[0]['content'])

    def test_unrelated_provider_error_remains_an_error(self):
        self.server.provider_error = True
        response = self.prompt('Provider failure is not a context budget terminal')
        self.assertEqual(response['error']['code'], -32603)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.server.effects, [])
        self.assertEqual(self.events('context.limit'), [])
        self.assertEqual(self.events('turn.finished')[-1]['data']['state'], 'failed')

    def test_unknown_tool_result_disqualifies_settled_capacity_and_is_not_replayed(self):
        self.server.unknown_effect = True
        response = self.prompt('RUN_CAPACITY_TOOL and preserve the uncorrelated second outcome')
        self.assertEqual(response['error']['code'], -32603)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.server.effects, ['capacity_receipt', 'unknown_effect'])
        self.assertEqual(self.events('context.limit'), [])
        self.assertEqual(self.events('turn.finished')[-1]['data']['state'], 'failed')
        before = self.items()
        outputs = [i['output'] for i in before if i.get('type') == 'function_call_output']
        self.assertEqual(json.loads(outputs[0]), RECEIPT)
        self.assertIn('unknown', outputs[1].lower())
        self.c.close()
        self.client()
        self.assertIn('result', self.c.call('session/load', {
            'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp}))
        self.assertEqual(self.items(), before)
        self.assertEqual((len(self.server.requests), self.server.effects), (1, ['capacity_receipt', 'unknown_effect']))

    def start_held_tool(self, controlled=False):
        self.server.hold_tool = True
        if controlled:
            self.server.RequestHandlerClass = ControlledHandler
            self.server.connection = queue.Queue()
            self.server.frames = queue.Queue()
        pending = self.c.request('session/prompt', {'sessionId': self.sid,
                                 'prompt': [{'type': 'text', 'text': 'RUN_CAPACITY_TOOL once'}]})
        wire = None
        if controlled:
            wire = self.server.connection.get(timeout=5)
            body = self.server.frames.get(timeout=5)
            self.assertEqual(body['type'], 'response.create')
            self.server.requests.append(body)
            name = next(t['name'] for t in body['tools'] if t['name'].endswith('_capacity_receipt'))
            wire._ws_send({'type': 'response.created', 'response': {'id': 'held-response'}})
            wire._ws_send({'type': 'response.completed', 'response': {
                'id': 'held-response', 'status': 'completed',
                'output': [{'type': 'function_call', 'call_id': 'held-call', 'name': name, 'arguments': '{}'}],
                'usage': {'input_tokens': 50, 'output_tokens': 20}}})
        while True:
            message = self.c.q.get(timeout=5)
            if message.get('method') == 'session/request_permission':
                self.c.callbacks.append(message)
                self.c.send({'jsonrpc': '2.0', 'id': message['id'], 'result': {
                    'outcome': {'outcome': 'selected', 'optionId': 'allow_once'}}})
                break
            self.assertIn('method', message, message)
            self.c.updates.append(message)
        self.assertTrue(self.server.tool_entered.wait(5))
        return pending, wire

    def test_pending_native_steering_is_not_reported_handled_at_capacity(self):
        pending, wire = self.start_held_tool(controlled=True)
        marker = 'UNINCORPORATED_CORRECTION_KEEP_ORIGINAL_ID'
        accepted = self.c.call('_session/steering', {'sessionId': self.sid,
            'prompt': [{'type': 'text', 'text': marker}],
            '_meta': {'steering': {'inputId': 'capacity-correction', 'idleBehavior': 'promptRequired'}}})
        self.assertEqual(accepted['result']['outcome'], 'injected')
        submitted = self.server.frames.get(timeout=5)
        self.assertEqual(submitted['type'], 'response.steer')
        wire._ws_send({'type': 'response.steer.accepted', 'steer': {
            'id': 'accepted-correction', 'previous_response_id': 'held-response'}})
        while True:
            message = self.c.q.get(timeout=5)
            self.assertIn('method', message, message)
            self.c.updates.append(message)
            receipt = message.get('params', {}).get('update', {}).get('_meta', {}).get(META, {}).get('inputReceipt', {})
            if receipt.get('state') == 'remoteQueued':
                break
        self.server.tool_release.set()
        response = self.c.finish(pending)
        self.results.append(response)
        self.assertEqual(response['error']['code'], -32603)
        self.assertEqual(self.events('context.limit'), [])
        rejected = self.events('context.rejected')
        self.assertEqual(len(rejected), 1)
        detail = rejected[0]['data']
        self.assertEqual(detail['capacity_boundary'], 'active_continuation')
        self.assertEqual(detail['yield_blocker'], 'provider_boundary_unsettled')
        self.assertEqual(detail['input_states'], {'remoteQueued': 1})
        self.assert_capacity_diagnostic('context.rejected', detail,
                                        [marker, 'capacity-correction', self.sid, 'held-response'])
        self.assertEqual(self.server.effects, ['capacity_receipt'])
        self.assertTrue(self.server.frames.empty(), 'The accepted correction must not be redispatched')
        receipts = self.c.call('_session/input_receipts', {'sessionId': self.sid})['result']['inputReceipts']
        self.assertEqual(len(receipts), 1)
        self.assertEqual(receipts[0]['state'], 'unknown')
        self.assertEqual(receipts[0]['inputId'], 'capacity-correction')
        self.assertEqual(receipts[0]['prompt'][0]['text'], marker)
        before = self.items()
        self.assertNotIn(marker, json.dumps(before), 'Queued input must not become incorporated history')
        self.assertEqual(json.loads(next(i['output'] for i in before if i.get('type') == 'function_call_output')), RECEIPT)
        self.c.close()
        self.client()
        self.c.call('session/load', {'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp})
        restored = self.c.call('_session/input_receipts', {'sessionId': self.sid})['result']['inputReceipts']
        self.assertEqual(restored, receipts)
        self.assertEqual(self.items(), before)
        self.assertEqual(self.server.effects, ['capacity_receipt'])
        self.assertEqual(len(self.server.requests), 1)
        self.assertTrue(self.server.frames.empty())

    def local_pending_capacity(self, *, pending_overflow, auto_compact=True):
        # A non-native HTTP route holds new inputs locally until the next
        # request. The MCP response barrier makes admission deterministic;
        # unlike the native negative control, nothing is remotely submitted.
        self.c.close()
        path = self.home / 'runtime.json'
        config = json.loads(path.read_text())
        config['provider'].update(kind='chat-completions',
                                  endpoint=f'http://127.0.0.1:{self.server.server_port}/chat/completions')
        config['context']['auto_compact'] = auto_compact
        path.write_text(json.dumps(config))
        self.client()
        self.assertIn('result', self.c.call('session/load', {
            'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp}))
        self.assertEqual(self.server.requests, [])
        if pending_overflow:
            # Canonical history fits by itself; two never-forwarded inputs make
            # only the prospective request exceed the configured byte budget.
            receipt_text = 'SMALL_SETTLED_RECEIPT_CANARY\n' + 'r' * 30000
            self.server.receipt = {'content': [{'type': 'text', 'text': receipt_text}], 'isError': False}
            markers = ['LOCAL_PENDING_ONE_CANARY ' + 'a' * 22000,
                       'LOCAL_PENDING_TWO_CANARY ' + 'b' * 22000]
        else:
            receipt_text = RECEIPT_TEXT
            markers = ['LOCAL_PENDING_CORRECTION_CANARY_KEEP_ORIGINAL_SOURCE']
        pending, _ = self.start_held_tool()
        expected_ids = [f'local-capacity-correction-{i}' for i in range(len(markers))]
        for input_id, marker in zip(expected_ids, markers):
            accepted = self.c.call('_session/steering', {
                'sessionId': self.sid, 'prompt': [{'type': 'text', 'text': marker}],
                '_meta': {'steering': {'inputId': input_id, 'idleBehavior': 'promptRequired'}}})
            self.assertEqual(accepted['result']['outcome'], 'injected')
        query = ('SELECT sequence,input_id,item,state,recorded,reason,remote_parent,remote_id,successor '
                 'FROM turn_inputs WHERE session=? ORDER BY sequence')
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            admitted = db.execute(query, (self.sid,)).fetchall()
        self.assertEqual([r[1] for r in admitted], expected_ids)
        self.assertTrue(all(r[3:] == ('received', 0, None, None, None, None) for r in admitted))
        self.assertEqual(len(self.server.requests), 1)
        for marker in markers:
            self.assertNotIn(marker, json.dumps(self.server.requests))

        self.server.tool_release.set()
        response = self.c.finish(pending)
        self.results.append(response)
        before = self.items()
        self.assertEqual(json.loads(next(i['output'] for i in before
                                       if i.get('type') == 'function_call_output')), self.server.receipt)
        started = self.events('model.started')
        self.assertEqual(len(started), 1, 'No oversized continuation may reach a second provider request')
        fixed = started[0]['data']['instructions_bytes'] + started[0]['data']['tool_definitions_bytes']
        # With this text-only fixture, serialized history is a conservative
        # upper bound, and the literal text bytes are a lower bound. These
        # inequalities independently distinguish the two reproduced paths.
        canonical_upper = len(json.dumps(before, ensure_ascii=False).encode()) + fixed
        literal_lower = len(receipt_text.encode()) + sum(len(m.encode()) for m in markers) + fixed
        self.boundary_observations.append({
            'case': 'pending_inputs' if pending_overflow else 'active_continuation',
            'limit': config['limits']['max_context_bytes'], 'fixed_bytes': fixed,
            'canonical_serialized_upper_bound': canonical_upper,
            'receipt_and_pending_literal_lower_bound': literal_lower,
            'received_inputs': len(admitted),
        })
        if pending_overflow:
            self.assertLess(canonical_upper, config['limits']['max_context_bytes'])
            self.assertGreater(literal_lower, config['limits']['max_context_bytes'])
        else:
            self.assertGreater(len(receipt_text.encode()) + fixed, config['limits']['max_context_bytes'])

        expected_reason = REASON if auto_compact else 'failed'
        if auto_compact:
            self.assertIn('result', response, response)
            self.assertEqual(response['result']['stopReason'], 'max_turn_requests')
            meta = response['result']['_meta'][META]
            self.assertEqual((meta['state'], meta['stopCause']), ('budget_exhausted', REASON))
        else:
            self.assertEqual(response['error']['code'], -32603)
            self.assertEqual(self.events('context.limit'), [])
        self.assertEqual((len(self.server.requests), self.server.effects), (1, ['capacity_receipt']))
        self.assertEqual(len(self.c.callbacks), 1)
        event = 'context.limit' if auto_compact else 'context.rejected'
        limits = self.events(event)
        self.assertEqual(len(limits), 1)
        detail = limits[0]['data']
        self.assertEqual(detail['reason'], REASON)
        self.assertEqual(detail['capacity_boundary'], 'pending_inputs' if pending_overflow else 'active_continuation')
        self.assertEqual(detail['input_states'], {'notAccepted' if auto_compact else 'received': len(markers)})
        self.assertEqual(detail['yield_blocker'], None if auto_compact else 'compaction_disabled')
        self.assertEqual((detail['model_calls'], detail['tool_calls']), (1, 1))
        self.assertTrue(detail['full_history_preserved'])
        self.assertGreater(detail['required_context_bytes'], detail['available_context_bytes'])
        self.assertNotIn(receipt_text[:24], json.dumps(detail))
        for marker in markers:
            self.assertNotIn(marker[:24], json.dumps(detail))
            self.assertNotIn(marker, json.dumps(before))
        self.assert_capacity_diagnostic(event, detail, expected_ids + [self.sid, receipt_text[:24]]
                                        + [m[:24] for m in markers])
        self.assertEqual(self.events('turn.finished')[-1]['data']['reason'],
                         REASON if auto_compact else 'provider_or_execution_failure')
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            terminal = db.execute(query, (self.sid,)).fetchall()
            canonical_raw = db.execute('SELECT items FROM sessions WHERE id=?', (self.sid,)).fetchone()[0]
        self.assertEqual([r[:3] for r in terminal], [r[:3] for r in admitted])
        self.assertTrue(all(r[3:] == ('notAccepted', 0, expected_reason, None, None, None) for r in terminal))
        receipts = self.c.call('_session/input_receipts', {'sessionId': self.sid})['result']['inputReceipts']
        # All these receipts have the same priority/state. The owner API
        # returns them newest first, while the canonical admission rows above
        # deliberately use ascending sequence to verify their original order.
        self.assertEqual([r['sequence'] for r in receipts], [r[0] for r in reversed(terminal)])
        self.assertEqual([r['inputId'] for r in receipts], list(reversed(expected_ids)))
        self.assertEqual({r['inputId']: r['prompt'][0]['text'] for r in receipts},
                         dict(zip(expected_ids, markers)))
        self.assertTrue(all(r['recorded'] is False for r in receipts))
        self.assertTrue(all(r['state'] == 'notAccepted' and r['reason'] == expected_reason for r in receipts))

        self.c.close()
        self.client()
        self.assertIn('result', self.c.call('session/load', {
            'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp}))
        self.assertEqual(self.c.call('_session/input_receipts', {
            'sessionId': self.sid})['result']['inputReceipts'], receipts)
        with closing(sqlite3.connect(self.home / 'history.sqlite3')) as db:
            self.assertEqual(db.execute(query, (self.sid,)).fetchall(), terminal)
            self.assertEqual(db.execute('SELECT items FROM sessions WHERE id=?',
                                        (self.sid,)).fetchone()[0], canonical_raw)
        self.assertEqual((len(self.server.requests), self.server.effects), (1, ['capacity_receipt']))

        fresh = self.prompt('FRESH_CONTINUATION: inspect the recorded result without repeating its effect')
        self.assertEqual(fresh['result']['stopReason'], 'end_turn')
        self.assertEqual((len(self.server.requests), self.server.effects), (2, ['capacity_receipt']))
        self.assertFalse(self.c.callbacks)
        self.assertEqual(self.items()[:len(before)], before)
        self.assertEqual(self.c.call('_session/input_receipts', {
            'sessionId': self.sid})['result']['inputReceipts'], receipts)
        for marker in markers:
            self.assertNotIn(marker, json.dumps(self.server.requests[-1]))
        self.assertEqual(len(self.events('context.limit')), 1 if auto_compact else 0)

    def test_active_capacity_preserves_never_forwarded_correction(self):
        self.local_pending_capacity(pending_overflow=False)

    def test_pending_input_capacity_preserves_never_forwarded_batch(self):
        self.local_pending_capacity(pending_overflow=True)

    def test_pending_capacity_does_not_override_disabled_compaction(self):
        self.local_pending_capacity(pending_overflow=True, auto_compact=False)

    def test_stop_during_tool_response_remains_cancelled_without_replay(self):
        pending, _ = self.start_held_tool()
        self.c.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': self.sid}})
        response = self.c.finish(pending)
        self.results.append(response)
        self.assertEqual(response['result']['stopReason'], 'cancelled')
        self.assertEqual(response['result']['_meta'][META]['state'], 'cancelled')
        self.assertEqual(self.events('context.limit'), [])
        self.assertEqual(self.server.effects, ['capacity_receipt'])
        self.assertEqual(len(self.server.requests), 1)
        before = self.items()
        self.assertIn('unknown', next(i['output'] for i in before if i.get('type') == 'function_call_output').lower())
        self.server.tool_release.set()
        self.c.close()
        self.client()
        self.c.call('session/load', {'sessionId': self.sid, 'cwd': str(self.work), 'mcpServers': self.mcp})
        self.assertEqual(self.items(), before)
        self.assertEqual((len(self.server.requests), self.server.effects), (1, ['capacity_receipt']))


if __name__ == '__main__':
    unittest.main()
