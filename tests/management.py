"""Actual CLI configuration and OAuth callback contracts. No live account used."""
import hashlib, json, queue, subprocess, tempfile, threading, unittest, urllib.parse, urllib.request
from pathlib import Path
from protocol import BINARY

class Tests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.home=Path(self.temp.name)/'state';self.work=Path(self.temp.name)/'workspace'
        self.run_cli('init','--workspace',self.work,'--provider','mock')
    def run_cli(self,*args,check=True):
        return subprocess.run([str(BINARY),'--home',str(self.home),*[str(a) for a in args]],capture_output=True,text=True,check=check)
    def test_schema_inspection_and_concurrent_edit_protection(self):
        schema=json.loads(self.run_cli('config-schema').stdout)
        self.assertEqual(schema['x-agent-runtime']['contract_version'],1)
        self.assertIn('context',schema['properties']);self.assertIn('Native',schema['$defs'])
        inspected=json.loads(self.run_cli('config-inspect').stdout)
        self.assertFalse(inspected['credential_values_included'])
        expected=hashlib.sha256((self.home/'runtime.json').read_bytes()).hexdigest()
        self.assertEqual(inspected['revision'],expected)
        config=inspected['config'];config['name']='Changed safely'
        candidate=Path(self.temp.name)/'candidate.json';candidate.write_text(json.dumps(config))
        self.assertTrue(json.loads(self.run_cli('config-validate',candidate).stdout)['valid'])
        changed=json.loads(self.run_cli('configure',candidate,'--expected-revision',expected).stdout)
        self.assertTrue(Path(changed['previous_config_file']).is_file())
        self.assertEqual(self.run_cli('configure',candidate,'--expected-revision',expected,check=False).returncode,1)
        config['provider']['invented_setting']=True;candidate.write_text(json.dumps(config))
        self.assertNotEqual(self.run_cli('config-validate',candidate,check=False).returncode,0)
        self.assertEqual(json.loads(self.run_cli('config-inspect').stdout)['config']['name'],'Changed safely')
    def test_unlisted_model_needs_an_explicit_opt_in_and_is_reported_unverified(self):
        unlisted='claude-unlisted-fixture';before=(self.home/'runtime.json').read_bytes()
        refused=self.run_cli('configure-provider','--provider','anthropic','--model',unlisted,check=False)
        self.assertNotEqual(refused.returncode,0);self.assertIn('--allow-unverified-model',refused.stderr)
        self.assertEqual((self.home/'runtime.json').read_bytes(),before)
        self.run_cli('configure-provider','--provider','anthropic','--model',unlisted,'--allow-unverified-model')
        inspected=json.loads(self.run_cli('config-inspect').stdout)
        self.assertTrue(inspected['validation']['valid'])
        self.assertEqual(inspected['config']['provider']['model'],unlisted)
        self.assertIs(inspected['config']['provider']['allow_unverified_model'],True)
        report=inspected['capabilities']
        self.assertEqual((report['model'],report['model_catalog_status']),(unlisted,'unverified'))
        self.assertEqual(report['model_options'],[{'model':unlisted,'model_catalog_status':'unverified'}])
        self.assertIs(report['account_and_model_support_verified'],False)
        check=json.loads(self.run_cli('check').stdout)
        self.assertEqual((check['model'],check['model_catalog_status']),(unlisted,'unverified'))
        self.assertNotIn(unlisted,json.dumps(json.loads(self.run_cli('config-schema').stdout)['x-agent-runtime']))
        candidate=Path(self.temp.name)/'candidate.json';config=inspected['config']
        candidate.write_text(json.dumps(config))
        self.assertEqual(json.loads(self.run_cli('config-validate',candidate).stdout)['model_catalog_status'],'unverified')
        # Turning the opt-in off is refused while the unlisted name is configured.
        del config['provider']['allow_unverified_model'];candidate.write_text(json.dumps(config))
        self.assertIn('allow_unverified_model',self.run_cli('config-validate',candidate,check=False).stderr)
        # A listed model needs no opt-in and saves none; other routes have nothing to opt in to.
        self.run_cli('configure-provider','--provider','anthropic','--model','claude-sonnet-5-5')
        inspected=json.loads(self.run_cli('config-inspect').stdout)
        self.assertNotIn('allow_unverified_model',inspected['config']['provider'])
        self.assertEqual(inspected['capabilities']['model_catalog_status'],'listed')
        other=self.run_cli('configure-provider','--provider','gemini','--model','gemini-3.8-flash','--allow-unverified-model',check=False)
        self.assertNotEqual(other.returncode,0);self.assertIn('applies only',other.stderr)
        schema=json.loads(self.run_cli('config-schema').stdout)
        self.assertEqual(schema['$defs']['Provider']['properties']['allow_unverified_model']['type'],'boolean')
        self.assertNotIn('allow_unverified_model',schema['$defs']['Provider']['required'])
    def test_real_loopback_signin_rejects_bad_state_without_credential_write(self):
        process=subprocess.Popen([str(BINARY),'--home',str(self.home),'chatgpt-sign-in','--port','0','--no-browser'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            lines=queue.Queue();threading.Thread(target=lambda:[lines.put(line) for line in process.stdout],daemon=True).start()
            info=json.loads(lines.get(timeout=10))
            auth=urllib.parse.urlparse(info['authorization_url']);params=urllib.parse.parse_qs(auth.query)
            self.assertEqual(auth.hostname,'auth.openai.com');self.assertEqual(params['client_id'],['dynamic_agent_client'])
            self.assertEqual(params['code_challenge_method'],['S256']);self.assertEqual(len(params['code_challenge'][0]),43)
            self.assertEqual(urllib.parse.urlparse(info['callback']).hostname,'127.0.0.1')
            self.assertEqual(params['redirect_uri'],[info['callback']])
            with self.assertRaises(urllib.error.HTTPError) as caught:
                urllib.request.urlopen(info['callback']+'?code=not-real&state=wrong&client_id=oaiapp_fixture',timeout=5)
            self.assertEqual(caught.exception.code,400)
            process.wait(timeout=10);self.assertNotEqual(process.returncode,0)
            self.assertFalse((self.home/'accounts/chatgpt/credentials.json').exists())
            self.assertFalse(json.loads(self.run_cli('chatgpt-status').stdout)['connected'])
        finally:
            if process.poll() is None:process.kill();process.wait()
            process.stdout.close();process.stderr.close()

if __name__=='__main__':unittest.main(verbosity=2)
