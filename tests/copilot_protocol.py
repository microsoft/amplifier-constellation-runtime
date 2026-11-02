"""Real Rust processes with a fake SDK bridge; fixtures only, no paid calls."""
import json,os,subprocess,sys,tempfile,time,unittest
from pathlib import Path
from unittest.mock import patch
import protocol

@unittest.skipUnless(os.name=='posix','SDK child-tree cleanup is currently Unix only')
class Tests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.home=Path(self.temp.name)/'state';self.work=Path(self.temp.name)/'workspace'
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--provider','github-copilot','--model','fixture','--key-env','COPILOT_FIXTURE_KEY','--copilot-python',sys.executable,'--copilot-bridge',str(Path(__file__).parent/'fixtures/copilot_provider.py')],check=True,stdout=subprocess.DEVNULL)
        self.env=patch.dict(os.environ,{'COPILOT_FIXTURE_KEY':'gho_fixture'});self.env.start();self.addCleanup(self.env.stop)
    def client(self):
        client=protocol.Client(self.home);client.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});return client
    def new(self,c):return c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
    def prompt(self,c,sid,text,approve=False):
        request=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':text}]})
        return c.finish(request,approve=approve)
    def test_rust_controls_permissions_and_restart_history(self):
        c=self.client()
        try:
            sid=self.new(c);r=self.prompt(c,sid,'write proof',approve=False)
            self.assertEqual(r['result']['stopReason'],'end_turn');self.assertFalse((self.work/'proof.txt').exists());self.assertTrue(c.callbacks)
            r=self.prompt(c,sid,'write proof',approve=True);self.assertEqual(r['result']['stopReason'],'end_turn');self.assertEqual((self.work/'proof.txt').read_text(),'verified')
        finally:c.close()
        c=self.client()
        try:
            c.call('session/load',{'sessionId':sid,'cwd':str(self.work),'mcpServers':[]});c.updates=[]
            self.prompt(c,sid,'what happened')
            self.assertIn('saved history includes write proof',json.dumps(c.updates))
        finally:c.close()
    def test_unadvertised_sdk_tools_are_not_executed(self):
        c=self.client()
        try:
            r=self.prompt(c,self.new(c),'unadvertised');self.assertIn('error',r);self.assertFalse(c.callbacks)
        finally:c.close()
    def test_cancel_terminates_bridge_descendants(self):
        c=self.client()
        try:
            sid=self.new(c);rid=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'cancel:'+str(self.work)}]})
            deadline=time.monotonic()+5
            while not (self.work/'started').exists() and time.monotonic()<deadline:time.sleep(.02)
            self.assertTrue((self.work/'started').exists())
            c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':sid}})
            r=c.finish(rid);self.assertEqual(r['result']['stopReason'],'cancelled');(self.work/'release').write_text('go');time.sleep(.3);self.assertFalse((self.work/'escaped').exists())
        finally:c.close()

if __name__=='__main__':unittest.main(verbosity=2)
