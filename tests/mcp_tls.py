"""Actual Native ACP and HTTPS MCP, with public test certificates and no model account."""
import json
import os
from pathlib import Path
import ssl
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

import protocol

CERTIFICATES = Path(__file__).resolve().parent / 'fixtures' / 'mcp-tls'


class McpHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        if self.headers.get('Authorization') != 'Bearer private-fixture':
            self.send_response(401)
            self.end_headers()
            return
        message = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.server.messages.append(message)
        method = message['method']
        if method == 'notifications/initialized':
            self.send_response(202)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        if method == 'initialize':
            result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}},
                      'serverInfo': {'name': 'private-ca-fixture', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'test_read', 'description': 'Read fixture evidence',
                                  'inputSchema': {'type': 'object', 'properties': {}},
                                  'annotations': {'readOnlyHint': True}}]}
        else:
            assert method == 'tools/call'
            assert message['params']['name'] == 'test_read'
            result = {'content': [{'type': 'text', 'text': 'private CA verified evidence'}]}
        encoded = json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': result}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(encoded)))
        self.send_header('Mcp-Session-Id', 'private-ca-session')
        self.end_headers()
        self.wfile.write(encoded)


class Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.model = ThreadingHTTPServer(('127.0.0.1', 0), protocol.Handler)
        cls.mcp = ThreadingHTTPServer(('127.0.0.1', 0), McpHandler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(CERTIFICATES / 'server.pem', CERTIFICATES / 'server-key.pem')
        cls.mcp.socket = context.wrap_socket(cls.mcp.socket, server_side=True)
        for server in (cls.model, cls.mcp):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        for server in (cls.model, cls.mcp):
            server.shutdown()
            server.server_close()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.home, self.work = self.root / 'state', self.root / 'work'
        self.clients = []
        self.mcp.messages = []
        self.url = f'https://127.0.0.1:{self.mcp.server_port}/mcp'
        subprocess.run([str(protocol.BINARY), '--home', str(self.home), 'init', '--workspace',
                        str(self.work), '--model', 'fixture'], check=True, stdout=subprocess.DEVNULL)
        self.config = json.loads((self.home / 'runtime.json').read_text())
        self.config['provider'].update(endpoint=f'http://127.0.0.1:{self.model.server_port}/responses',
                                       api_key_env='RUNTIME_FIXTURE_KEY', reasoning_effort=None)
        self.config['context']['provider_compaction'] = False
        self.save_config()

    def tearDown(self):
        for client in self.clients:
            client.close()
        self.tmp.cleanup()

    def save_config(self):
        (self.home / 'runtime.json').write_text(json.dumps(self.config))

    def connect(self, ca=None, url=None):
        environment = dict(os.environ)
        environment.pop('SSL_CERT_FILE', None)
        if ca is not None:
            environment['SSL_CERT_FILE'] = str(ca)
        with patch.dict(os.environ, environment, clear=True):
            client = protocol.Client(self.home)
        self.clients.append(client)
        self.assertIn('result', client.call('initialize', {'protocolVersion': 1, 'clientCapabilities': {}}))
        response = client.call('session/new', {
            'cwd': str(self.work), 'mcpServers': [{'type': 'http', 'name': 'private-fixture',
                'url': url or self.url, 'headers': [{'name': 'Authorization', 'value': 'Bearer private-fixture'}]}]})
        return client, response

    def assert_refused(self, ca=None, url=None):
        _, response = self.connect(ca, url)
        self.assertIn('error', response)
        self.assertEqual(self.mcp.messages, [], 'TLS refusal must precede authenticated MCP requests')

    def test_owner_ca_allows_https_without_granting_tool_permission(self):
        client, response = self.connect(CERTIFICATES / 'ca.pem')
        self.assertIn('result', response)
        sid = response['result']['sessionId']
        prompt = {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'use mcp'}]}
        self.assertIn('result', client.call('session/prompt', prompt))
        self.assertEqual(len(client.callbacks), 1)
        self.assertFalse(any(m['method'] == 'tools/call' for m in self.mcp.messages))
        self.assertIn('result', client.call('session/prompt', prompt, approve=True))
        self.assertEqual(len(client.callbacks), 2)
        self.assertEqual(sum(m['method'] == 'tools/call' for m in self.mcp.messages), 1)
        self.assertIn('private CA verified evidence', json.dumps(client.updates))

    def test_ca_bundle_and_existing_exact_endpoint_permission_work_together(self):
        bundle = self.root / 'bundle.pem'
        bundle.write_bytes((CERTIFICATES / 'wrong-ca.pem').read_bytes() + (CERTIFICATES / 'ca.pem').read_bytes())
        self.config['trusted_mcp_readonly_urls'] = [self.url]
        self.save_config()
        client, response = self.connect(bundle)
        self.assertIn('result', response)
        result = client.call('session/prompt', {'sessionId': response['result']['sessionId'],
                    'prompt': [{'type': 'text', 'text': 'use mcp'}]})
        self.assertIn('result', result)
        self.assertEqual(client.callbacks, [])
        self.assertEqual(sum(m['method'] == 'tools/call' for m in self.mcp.messages), 1)

    def test_untrusted_private_ca_is_refused(self):
        self.assert_refused()

    def test_wrong_ca_is_refused(self):
        self.assert_refused(CERTIFICATES / 'wrong-ca.pem')

    def test_valid_ca_does_not_disable_hostname_verification(self):
        self.assert_refused(CERTIFICATES / 'ca.pem', self.url.replace('127.0.0.1', 'localhost'))

    def test_missing_empty_malformed_and_invalid_der_ca_files_are_refused(self):
        for name, content in [('missing', None), ('empty', b''), ('malformed', b'not a certificate'),
                              ('invalid-der', b'-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n')]:
            with self.subTest(name=name):
                ca = self.root / name
                if content is not None:
                    ca.write_bytes(content)
                self.assert_refused(ca)

    def test_model_route_ca_does_not_silently_authorize_mcp(self):
        self.config['provider']['tls_ca_file'] = str(CERTIFICATES / 'ca.pem')
        self.save_config()
        self.assert_refused()

    def test_loopback_http_does_not_require_the_https_ca_file(self):
        _, response = self.connect(self.root / 'missing', f'http://127.0.0.1:{self.model.server_port}/mcp')
        self.assertIn('result', response)


if __name__ == '__main__':
    unittest.main()
