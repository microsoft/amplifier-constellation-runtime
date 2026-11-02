"""Installer credential isolation with a real runtime and an in-memory host stub."""
import contextlib, importlib.util, io, json, os, subprocess, sys, tempfile, unittest
from pathlib import Path
from unittest.mock import patch, MagicMock
from types import SimpleNamespace
import protocol

spec=importlib.util.spec_from_file_location('installer',protocol.ROOT/'scripts/install-instance.py')
installer=importlib.util.module_from_spec(spec);spec.loader.exec_module(installer)

class Tests(unittest.TestCase):
    def install(self,provider,extra=(),key=None):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.home=Path(self.tmp.name)/'agent';self.host_created=False
        real_run=installer.run
        def run(*args):
            if args[0]=='bash':
                self.host_created=True;self.home.mkdir();(self.home/'workspace').mkdir()
                (self.home/'config.json').write_text(json.dumps({'revision':1,'config':{'instance_id':'fixture','server':{}}}))
                return subprocess.CompletedProcess(args,0,'','')
            return real_run(*args)
        argv=['install-instance','--host-repo',self.tmp.name,'--host-binary',sys.executable,'--runtime-binary',str(protocol.BINARY),'--home',str(self.home),'--provider',provider,'--no-start',*extra]
        with patch.object(installer,'run',run),patch.object(sys,'argv',argv),patch.dict(os.environ,{'INSTALL_TEST_KEY':key or '', 'UNRELATED_SECRET':'never-copy'}),contextlib.redirect_stdout(io.StringIO()):
            installer.main()
        self.doc=json.loads((self.home/'config.json').read_text())['config']
        self.config=json.loads((self.home/'engine/runtime.json').read_text())
    def test_keyless_mock_does_not_require_or_copy_credentials(self):
        self.install('mock')
        self.assertEqual(self.doc['backend']['secret_env'],{})
        self.assertFalse((self.home/'secrets/native-model-key').exists())
        self.assertEqual(self.config['provider']['kind'],'mock')
    def test_selected_provider_gets_only_its_explicit_credential(self):
        self.install('anthropic',['--model','fixture','--key-env','INSTALL_TEST_KEY','--reasoning-effort','medium'],key='fake-credential\n')
        self.assertEqual(self.config['provider']['api_key_env'],'AGENT_MODEL_API_KEY')
        self.assertEqual(self.doc['backend']['env'],{})
        self.assertEqual(self.doc['backend']['secret_env'],{'AGENT_MODEL_API_KEY':'native-model-key'})
        self.assertEqual((self.home/'secrets/native-model-key').read_text(),'fake-credential\n')
        self.assertNotIn('fake-credential',json.dumps(self.config))
        self.assertNotIn('UNRELATED_SECRET',json.dumps(self.doc))
    def test_invalid_provider_settings_fail_before_a_host_is_created(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.install('azure-openai',['--model','fixture','--key-env','INSTALL_TEST_KEY'],key='fake')
        self.assertFalse(self.host_created)
    def test_native_platform_registration_waits_until_configuration_is_ready(self):
        for system,service in [('Darwin',{'kind':'launchd','name':'one'}),('Windows',{'kind':'scheduled-task','name':'one'})]:
            with self.subTest(system=system),tempfile.TemporaryDirectory() as tmp:
                home=Path(tmp)/'instance';home.mkdir()
                (home/'install.json').write_text(json.dumps({'current':{'version':'fixture'},'service':None}))
                a=SimpleNamespace(host_repo=Path(tmp),home=home,name='One',host_binary=Path(sys.executable),no_start=True)
                native=MagicMock();native.make_service.return_value=service
                with patch.object(installer.platform,'system',return_value=system),patch.object(installer,'run') as run,patch.object(installer.importlib.util,'spec_from_file_location') as spec,patch.object(installer.importlib.util,'module_from_spec',return_value=native):
                    self.assertIs(installer.platform_host_install(a),native)
                    command=run.call_args.args
                    self.assertIn('--skip-runtime',command);self.assertIn('--no-service',command)
                    self.assertEqual(installer.platform_start(a,{'instance_id':'one'},native),service)
                    native.make_service.assert_called_once()
                    native.service_command.assert_not_called();native.health.assert_not_called()
                    self.assertEqual(native.save.call_args.args[1]['service'],service)
                    a.no_start=False
                    installer.platform_start(a,{'instance_id':'one'},native)
                    native.service_command.assert_called_once_with(service,'start')
                    native.health.assert_called_once_with(home,'one','fixture')

if __name__=='__main__':unittest.main(verbosity=2)
