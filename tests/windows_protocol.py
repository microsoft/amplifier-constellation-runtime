"""Actual Windows commands through the stdio ACP permission contract."""
import json, os, subprocess, tempfile, threading, unittest
from pathlib import Path
from http.server import ThreadingHTTPServer
from protocol import BINARY, Client, Handler

@unittest.skipUnless(os.name == 'nt', 'Native Windows process test')
class Tests(unittest.TestCase):
    def test_command_permission_readonly_and_actual_effects(self):
        with tempfile.TemporaryDirectory() as temp:
            home, work = Path(temp)/'state', Path(temp)/'work with spaces'
            subprocess.run([str(BINARY),'--home',str(home),'init','--workspace',str(work),'--model','fixture'],check=True,capture_output=True)
            server = ThreadingHTTPServer(('127.0.0.1',0),Handler)
            threading.Thread(target=server.serve_forever,daemon=True).start()
            try:
                config=json.loads((home/'runtime.json').read_text())
                config['provider'].update(endpoint=f'http://127.0.0.1:{server.server_port}/responses',api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None)
                (home/'runtime.json').write_text(json.dumps(config))
                check=json.loads(subprocess.check_output([str(BINARY),'--home',str(home),'check']))
                self.assertEqual(check['local_command_isolation'],'windows-appcontainer')
                client=Client(home)
                try:
                    client.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
                    session=client.call('session/new',{'cwd':str(work),'mcpServers':[]})['result']['sessionId']
                    def prompt(approve=False):
                        result=client.call('session/prompt',{'sessionId':session,'prompt':[{'type':'text','text':'run native windows'}]},approve=approve)
                        self.assertEqual(result['result']['stopReason'],'end_turn')
                    prompt()
                    self.assertEqual(len(client.callbacks),1)
                    self.assertFalse((work/'command-proof.txt').exists())
                    client.call('session/set_mode',{'sessionId':session,'modeId':'read_only'})
                    prompt(approve=True)
                    self.assertEqual(len(client.callbacks),1)
                    self.assertFalse((work/'command-proof.txt').exists())
                    client.call('session/set_mode',{'sessionId':session,'modeId':'ask'})
                    prompt(approve=True)
                    self.assertEqual(len(client.callbacks),2)
                    self.assertEqual((work/'command-proof.txt').read_text().strip(),'ACP-command-proof')
                finally:
                    client.close()
            finally:
                server.shutdown();server.server_close()

if __name__ == '__main__':unittest.main(verbosity=2)
