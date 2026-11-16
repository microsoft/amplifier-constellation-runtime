"""Real Rust processes with a fake SDK bridge; fixtures only, no paid calls."""
import json,os,sqlite3,subprocess,sys,tempfile,threading,time,unittest
from http.server import ThreadingHTTPServer
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
    def test_hosted_approval_scope_uses_same_rust_boundary_and_never_caches_a_grant(self):
        import uuid
        meta='com.microsoft.amplifier.approval-policy'
        scopes=[]
        for approve in (True,False):
            c=protocol.Client(self.home)
            try:
                initialized=c.call('initialize',{'protocolVersion':1,'clientCapabilities':{},'_meta':{meta:{'version':1,'owner':'instance','instanceId':str(uuid.uuid4())}}})['result']
                self.assertEqual(initialized['_meta'][meta]['producer'],'microsoft.amplifier-constellation-runtime')
                sid=self.new(c)
                result=self.prompt(c,sid,'write proof',approve=approve)
                self.assertEqual(result['result']['stopReason'],'end_turn')
                self.assertEqual(len(c.callbacks),1)
                call=c.callbacks[0]['params']['toolCall']
                scopes.append(call['_meta'][meta])
                self.assertEqual(call['_meta']['com.microsoft.amplifier.tool'],{'version':1,'kind':'builtin','name':'workspace_write'})
                self.assertEqual((self.work/'proof.txt').exists(),approve)
                if approve:(self.work/'proof.txt').unlink()
            finally:c.close()
        self.assertEqual(scopes[0],scopes[1])
        self.assertEqual(scopes[0]['authorityLifetime'],'request')
    def sequence(self,c,sid,calls,**kwargs):
        program={'calls':calls,**kwargs.pop('program',{})}
        return self.prompt(c,sid,'sequence:'+json.dumps(program),**kwargs)
    @staticmethod
    def tool(name,arguments):return {'name':name,'arguments':json.dumps(arguments)}
    def items(self,sid):
        with sqlite3.connect(self.home/'history.sqlite3') as db:
            return json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(sid,)).fetchone()[0])
    def assert_no_unknown_announcement(self,c):
        ids=[frame['params']['update'].get('toolCallId') for frame in c.updates if frame.get('method')=='session/update']
        self.assertNotIn('call-0',ids)
    def test_unlisted_name_then_exact_builtin_preserves_permission_and_identity(self):
        for approve in (False,True):
            with self.subTest(approve=approve):
                c=self.client()
                try:
                    sid=self.new(c)
                    result=self.sequence(c,sid,[self.tool('workspace_wrote',{'path':'wrong.txt','content':'never'}),
                                               self.tool('workspace_write',{'path':'proof.txt','content':'verified'})],approve=approve)
                    self.assertIn('result',result)
                    self.assertEqual(result['result']['stopReason'],'end_turn',result)
                    self.assertFalse((self.work/'wrong.txt').exists())
                    self.assertEqual((self.work/'proof.txt').exists(),approve)
                    self.assert_no_unknown_announcement(c)
                    self.assertEqual(len(c.callbacks),1)
                    call=c.callbacks[0]['params']['toolCall']
                    self.assertEqual(call['toolCallId'],'call-1')
                    self.assertEqual(call['_meta']['com.microsoft.amplifier.tool'],
                                     {'version':1,'kind':'builtin','name':'workspace_write'})
                    outputs=[item for item in self.items(sid) if item.get('type')=='function_call_output']
                    self.assertEqual([item['call_id'] for item in outputs],['call-0','call-1'])
                    self.assertIn('nothing was run',outputs[0]['output'])
                    self.assertNotIn('workspace_wrote',outputs[0]['output'])
                    self.assertIn('nothing was run',json.dumps(c.updates))
                finally:c.close()
    def test_unlisted_mcp_name_is_not_an_alias_and_exact_call_needs_permission(self):
        from tool_permissions import Handler
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler);server.effects=[]
        threading.Thread(target=server.serve_forever,daemon=True).start()
        c=self.client()
        try:
            sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[{'name':'minecraft','type':'http','url':f'http://127.0.0.1:{server.server_port}/mcp','headers':[]}]})['result']['sessionId']
            result=self.sequence(c,sid,[self.tool('mcp_9_observe',{}),self.tool('@mcp',{})],approve=True)
            self.assertEqual(result['result']['stopReason'],'end_turn',result)
            self.assert_no_unknown_announcement(c)
            self.assertEqual(server.effects,[{'name':'observe','arguments':{}}])
            self.assertEqual(len(c.callbacks),1)
            call=c.callbacks[0]['params']['toolCall']
            self.assertEqual(call['toolCallId'],'call-1')
            self.assertEqual(call['_meta']['com.microsoft.amplifier.tool'],
                             {'version':1,'kind':'mcp','name':'observe','server':'minecraft'})
            failure=next(item for item in self.items(sid) if item.get('type')=='function_call_output')
            self.assertIn('mcp_0_observe',failure['output'])
            self.assertNotIn('mcp_9_observe',failure['output'])
        finally:
            c.close();server.shutdown();server.server_close()
    def test_corrected_builtin_schema_still_precedes_permission_and_effects(self):
        c=self.client()
        try:
            sid=self.new(c)
            result=self.sequence(c,sid,[self.tool('workspace_wrote',{}),self.tool('workspace_write',
                                 {'path':'injected.txt','content':'never','server':'minecraft','tool':'observe'})],approve=True)
            self.assertEqual(result['result']['stopReason'],'end_turn',result)
            self.assertFalse(c.callbacks);self.assertFalse((self.work/'injected.txt').exists())
            self.assert_no_unknown_announcement(c)
            outputs=[item for item in self.items(sid) if item.get('type')=='function_call_output']
            self.assertEqual(len(outputs),2);self.assertIn('error',json.loads(outputs[1]['output']))
        finally:c.close()
    def test_corrected_call_still_respects_read_only_mode(self):
        c=self.client()
        try:
            sid=self.new(c)
            self.assertIn('result',c.call('session/set_mode',{'sessionId':sid,'modeId':'read_only'}))
            result=self.sequence(c,sid,[self.tool('workspace_wrote',{}),
                                       self.tool('workspace_write',{'path':'readonly.txt','content':'never'})],approve=True)
            self.assertEqual(result['result']['stopReason'],'end_turn',result)
            self.assertFalse(c.callbacks);self.assertFalse((self.work/'readonly.txt').exists())
            self.assert_no_unknown_announcement(c)
        finally:c.close()
    def test_repeated_unlisted_calls_consume_existing_tool_and_model_budgets(self):
        for limits,cause in [({'max_tool_calls':2,'max_model_calls':8},'tool_call_budget'),
                             ({'max_tool_calls':8,'max_model_calls':2},'max_turn_requests')]:
            with self.subTest(limits=limits):
                path=self.home/'runtime.json';config=json.loads(path.read_text());config['limits'].update(limits);path.write_text(json.dumps(config))
                c=self.client()
                try:
                    sid=self.new(c)
                    result=self.sequence(c,sid,[self.tool('not_offered',{})],program={'repeat':True})
                    self.assertEqual(result['result']['_meta']['io.github.bkrabach.agent-runtime']['stopCause'],cause,result)
                    self.assertFalse(c.callbacks)
                    outputs=[item for item in self.items(sid) if item.get('type')=='function_call_output']
                    settled=[item for item in outputs if 'nothing was run' in item['output']]
                    self.assertEqual([item['call_id'] for item in settled],['call-0','call-1'])
                    with sqlite3.connect(self.home/'history.sqlite3') as db:
                        events=db.execute("SELECT data FROM events WHERE session=? AND kind='tool.unlisted' ORDER BY seq",(sid,)).fetchall()
                    self.assertEqual([json.loads(row[0])['call_id'] for row in events],['call-0','call-1'])
                    # When a model returns one more call than the tool budget,
                    # normal stopped-turn recovery settles its saved intent as
                    # unknown; it was not processed as another unlisted call.
                    self.assertEqual(len(outputs),3 if cause=='tool_call_budget' else 2)
                    if cause=='tool_call_budget':
                        self.assertEqual(outputs[-1]['call_id'],'call-2')
                        self.assertIn('not automatically replayed',outputs[-1]['output'])
                finally:c.close()
    def test_malformed_unlisted_calls_and_duplicate_ids_still_fail(self):
        for call,program in [({'name':'not_offered','arguments':'[]'},{}),
                             ({'name':'not_offered','arguments':'{'},{}),
                             ({'name':'','arguments':'{}'},{}),
                             ({'name':12,'arguments':'{}'},{}),
                             ({'name':'not_offered','arguments':'{}'},{'duplicate':True})]:
            with self.subTest(call=call,program=program):
                c=self.client()
                try:
                    sid=self.new(c);result=self.sequence(c,sid,[call],program=program,approve=True)
                    self.assertIn('error',result);self.assertFalse(c.callbacks)
                    self.assertFalse(any(item.get('type')=='function_call_output' for item in self.items(sid)))
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
