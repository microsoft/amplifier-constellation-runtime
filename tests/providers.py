"""Provider wire contracts exercised through real ACP subprocesses and tools.

Local servers implement each vendor's distinct streaming shape. No account,
hosted model, local model installation, or inherited model key is used.
"""
import json, os, subprocess, tempfile, threading, time, unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse
import protocol

KEY = 'fixture-provider-credential'
os.environ['PROVIDER_TEST_KEY'] = KEY + '\n'
KINDS = ['openai','azure-openai','vllm','anthropic','gemini','ollama','chat-completions','litellm']
PRIVATE = 'PRIVATE-REASONING-DO-NOT-RENDER'
ARGS = {'path':'provider-proof.txt','content':'verified café\n'}

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        case=self.server.case; kind=case['kind']; case['requests'].append((dict(self.headers),body,self.path))
        if case.get('stall'):
            case['started'].set();case['release'].wait(10)
        turn=len(case['requests']); tool=turn==1
        if case.get('http_error'):
            self.send_response(429);self.end_headers();self.wfile.write(b'fixture body includes private credential, never display');return
        events=[]; reason='length' if case.get('length') else None
        if kind in ['openai','azure-openai','vllm']:
            out=[{'type':'function_call','call_id':'call-1','name':'workspace_write','arguments':json.dumps(ARGS)}] if tool else [{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Verified café'}]}]
            if not tool:events.append({'type':'response.output_text.delta','delta':'Verified café'})
            events.append({'type':'response.incomplete' if reason else 'response.completed','response':{'status':'incomplete' if reason else 'completed','output':out,'usage':{'input_tokens':20,'output_tokens':10}}})
        elif kind in ['chat-completions','litellm']:
            if tool:
                args=json.dumps(ARGS)
                events=[{'choices':[{'delta':{'reasoning_content':PRIVATE,'tool_calls':[{'index':0,'id':'call-1','type':'function','function':{'name':'workspace_write','arguments':args[:12]}}]},'finish_reason':None}]},
                        {'choices':[{'delta':{'tool_calls':[{'index':0,'function':{'arguments':args[12:]}}]},'finish_reason':reason or 'tool_calls'}]}]
            else:events=[{'choices':[{'delta':{'content':'Verified café'},'finish_reason':'stop'}]}]
            events.append({'choices':[],'usage':{'prompt_tokens':20,'completion_tokens':10}})
        elif kind=='anthropic':
            events=[{'type':'message_start','message':{'usage':{'input_tokens':20,'output_tokens':0}}}]
            if tool:
                events += [{'type':'content_block_start','index':0,'content_block':{'type':'thinking','thinking':'','signature':''}},
                           {'type':'content_block_delta','index':0,'delta':{'type':'thinking_delta','thinking':PRIVATE}},
                           {'type':'content_block_delta','index':0,'delta':{'type':'signature_delta','signature':'opaque-signature'}},
                           {'type':'content_block_stop','index':0},
                           {'type':'content_block_start','index':1,'content_block':{'type':'tool_use','id':'call-1','name':'workspace_write','input':{}}},
                           {'type':'content_block_delta','index':1,'delta':{'type':'input_json_delta','partial_json':json.dumps(ARGS)}},
                           {'type':'content_block_stop','index':1}]
            else:events += [{'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}}, {'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'Verified café'}},{'type':'content_block_stop','index':0}]
            events += [{'type':'message_delta','delta':{'stop_reason':'max_tokens' if reason else 'tool_use' if tool else 'end_turn'},'usage':{'output_tokens':10}},{'type':'message_stop'}]
        elif kind=='gemini':
            parts=[{'thought':True,'text':PRIVATE},{'functionCall':{'id':'wire-1','name':'workspace_write','args':ARGS},'thoughtSignature':'opaque-signature'}] if tool else [{'text':'Verified café','thoughtSignature':'text-signature'}]
            events=[{'candidates':[{'content':{'role':'model','parts':parts},'finishReason':'MAX_TOKENS' if reason else 'STOP'}],'usageMetadata':{'promptTokenCount':20,'candidatesTokenCount':10}}]
        elif kind=='ollama':
            message={'role':'assistant','thinking':PRIVATE,'content':'','tool_calls':[{'function':{'name':'workspace_write','arguments':ARGS}}]} if tool else {'role':'assistant','content':'Verified café'}
            events=[{'message':message,'done':False},{'message':{'role':'assistant','content':''},'done':True,'done_reason':reason or 'stop','prompt_eval_count':20,'eval_count':10}]
        if case.get('truncated'):
            if kind in ['openai','azure-openai','vllm']:events=[]
            elif kind in ['chat-completions','litellm']:events[1]['choices'][0]['finish_reason']=None
            elif kind=='anthropic':events=events[:-2]
            elif kind=='gemini':events[0]['candidates'][0].pop('finishReason')
            else:events=events[:-1]
        raw=''.join((json.dumps(e,ensure_ascii=False)+'\n') if kind=='ollama' else ('event: ignored-name\r\ndata: '+json.dumps(e,ensure_ascii=False)+'\r\n\r\n') for e in events).encode()
        if kind in ['chat-completions','litellm'] and not case.get('truncated'):raw+=b'data: [DONE]\n\n'
        self.send_response(200);self.send_header('Content-Type','application/x-ndjson' if kind=='ollama' else 'text/event-stream');self.end_headers()
        # Deliberately split framing, JSON tokens, and multi-byte text.
        try:
            for i in range(0,len(raw),7):self.wfile.write(raw[i:i+7]);self.wfile.flush()
        except (BrokenPipeError,ConnectionResetError,ConnectionAbortedError):pass

class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'state';self.work=Path(self.tmp.name)/'workspace';self.clients=[]
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=self.server.serve_forever,daemon=True).start()
    def tearDown(self):
        for c in self.clients:c.close()
        self.server.shutdown();self.server.server_close();self.tmp.cleanup()
    def setup_kind(self,kind,**case):
        self.server.case={'kind':kind,'requests':[],**case}
        if not self.home.exists():subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work)],check=True,stdout=subprocess.DEVNULL)
        path=self.home/'runtime.json';cfg=json.loads(path.read_text());cfg['mode']='ask'
        cfg['provider'].update(kind=kind,endpoint=f'http://127.0.0.1:{self.server.server_port}/{kind}',model='fixture',api_key_env='PROVIDER_TEST_KEY',reasoning_effort=None)
        path.write_text(json.dumps(cfg))
    def client(self):
        c=protocol.Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});return c
    def close(self,c):c.close();self.clients.remove(c)
    def prompt(self,c,s,text='write',approve=True):return c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':text}]},approve=approve)
    def check_wire(self,kind):
        reqs=self.server.case['requests'];headers,first,path=reqs[0];h={k.lower():v for k,v in headers.items()}
        expected='api-key' if kind=='azure-openai' else 'x-api-key' if kind=='anthropic' else 'x-goog-api-key' if kind=='gemini' else 'authorization'
        self.assertEqual(h[expected],KEY if expected!='authorization' else 'Bearer '+KEY)
        self.assertNotIn(KEY,path);self.assertNotIn(KEY,json.dumps(first))
        if expected!='authorization':self.assertNotIn('authorization',h)
        second=reqs[1][1]
        if kind=='anthropic':
            self.assertEqual(h['anthropic-version'],'2023-06-01');self.assertIn('input_schema',first['tools'][0])
            self.assertEqual(second['messages'][1]['content'][0],{'type':'thinking','thinking':PRIVATE,'signature':'opaque-signature'})
            self.assertEqual(second['messages'][2]['content'][0]['tool_use_id'],'call-1')
        elif kind=='gemini':
            self.assertTrue(path.endswith('/models/fixture:streamGenerateContent?alt=sse'),path)
            self.assertIn('parametersJsonSchema',first['tools'][0]['functionDeclarations'][0])
            self.assertEqual(second['contents'][1]['parts'][1]['thoughtSignature'],'opaque-signature')
            self.assertEqual(second['contents'][2]['parts'][0]['functionResponse']['id'],'wire-1')
        elif kind=='ollama':
            self.assertEqual(first['options']['num_predict'],8192);self.assertEqual(second['messages'][2]['thinking'],PRIVATE)
            self.assertEqual(second['messages'][3]['tool_name'],'workspace_write')
        elif kind in ['chat-completions','litellm']:
            self.assertEqual(first['stream_options'],{'include_usage':True})
            self.assertIn('function',first['tools'][0]);self.assertEqual(second['messages'][2]['reasoning_content'],PRIVATE)
            self.assertEqual(second['messages'][3]['tool_call_id'],'call-1')
        elif kind=='vllm':self.assertNotIn('include',first)
        else:self.assertEqual(first['include'],['reasoning.encrypted_content'])
    def test_each_provider_tools_streaming_resume_and_private_state(self):
        for kind in KINDS:
            with self.subTest(kind=kind):
                self.setup_kind(kind);c=self.client();sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
                r=self.prompt(c,sid);self.assertEqual(r.get('result',{}).get('stopReason'),'end_turn',r)
                self.assertEqual((self.work/'provider-proof.txt').read_text(encoding='utf-8'),'verified café\n');self.assertEqual(len(c.callbacks),1)
                self.assertNotIn(PRIVATE,json.dumps(c.updates));self.assertIn('Verified caf',json.dumps(c.updates));self.check_wire(kind)
                self.close(c);c=self.client();c.call('session/load',{'sessionId':sid,'cwd':str(self.work),'mcpServers':[]})
                self.assertNotIn(PRIVATE,json.dumps(c.updates));self.assertTrue(any(x.get('params',{}).get('update',{}).get('sessionUpdate')=='tool_call' for x in c.updates))
                self.assertEqual(self.prompt(c,sid,'recall')['result']['stopReason'],'end_turn');self.assertEqual(len(self.server.case['requests']),3)
                if kind in ['anthropic','gemini','ollama','chat-completions','litellm']:self.assertIn(PRIVATE,json.dumps(self.server.case['requests'][2][1]))
                self.close(c);(self.work/'provider-proof.txt').unlink()
    def test_truncated_streams_never_execute_tools(self):
        for kind in KINDS:
            with self.subTest(kind=kind):
                self.setup_kind(kind,truncated=True);c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
                self.assertIn('error',self.prompt(c,s));self.assertFalse((self.work/'provider-proof.txt').exists());self.assertEqual(c.callbacks,[]);self.close(c)
    def test_incomplete_response_never_executes_a_tool(self):
        for kind in KINDS:
            with self.subTest(kind=kind):
                self.setup_kind(kind,length=True);c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
                self.assertEqual(self.prompt(c,s)['result']['stopReason'],'max_tokens');self.assertFalse((self.work/'provider-proof.txt').exists());self.close(c)
    def test_cancellation_ends_each_provider_request_without_effects(self):
        for kind in KINDS:
            with self.subTest(kind=kind):
                started=threading.Event();release=threading.Event()
                self.setup_kind(kind,stall=True,started=started,release=release)
                c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
                request=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'write'}]})
                try:
                    self.assertTrue(started.wait(5));begin=time.monotonic()
                    c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':s}})
                    self.assertEqual(c.finish(request)['result']['stopReason'],'cancelled')
                    self.assertLess(time.monotonic()-begin,2)
                    self.assertFalse((self.work/'provider-proof.txt').exists());self.assertEqual(c.callbacks,[])
                finally:release.set();self.close(c)
    def test_model_or_endpoint_change_cannot_reuse_an_old_session(self):
        self.setup_kind('anthropic');c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.prompt(c,s);self.close(c)
        p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['provider']['model']='different-model';p.write_text(json.dumps(cfg));c=self.client();c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]})
        r=self.prompt(c,s,'new prompt');self.assertIn('different provider',r['error']['message']);self.assertEqual(len(self.server.case['requests']),2)
    def test_permission_denial_is_not_bypassed_by_a_new_provider(self):
        self.setup_kind('gemini');c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.assertEqual(self.prompt(c,s,approve=False)['result']['stopReason'],'end_turn');self.assertFalse((self.work/'provider-proof.txt').exists())
        self.assertIn('Permission declined',json.dumps(self.server.case['requests'][1][1]))
    def test_azure_bearer_and_api_version_are_explicit(self):
        self.setup_kind('azure-openai');p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['provider'].update(auth='bearer',api_version='fixture-version');p.write_text(json.dumps(cfg));c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.prompt(c,s)
        h,b,path=self.server.case['requests'][0];h={k.lower():v for k,v in h.items()};self.assertEqual(h['authorization'],'Bearer '+KEY);self.assertNotIn('api-key',h);self.assertTrue(path.endswith('?api-version=fixture-version'))
    def test_no_auth_local_server_does_not_receive_an_inherited_key(self):
        self.setup_kind('ollama');p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['provider']['auth']='none';p.write_text(json.dumps(cfg));c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.prompt(c,s)
        h,_,_=self.server.case['requests'][0];self.assertNotIn('authorization',{k.lower():v for k,v in h.items()})
    def test_http_error_does_not_retry_or_expose_response_body(self):
        self.setup_kind('anthropic',http_error=True);c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];r=self.prompt(c,s);self.assertIn('HTTP 429',r['error']['message']);self.assertNotIn('private credential',str(r));self.assertEqual(len(self.server.case['requests']),1)
    def test_provider_cli_and_mock_need_no_credentials_or_network(self):
        rows=json.loads(subprocess.check_output([str(protocol.BINARY),'providers'],text=True));self.assertEqual(len(rows),10)
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--provider','mock'],check=True,stdout=subprocess.DEVNULL)
        c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.assertEqual(self.prompt(c,s)['result']['stopReason'],'end_turn');self.assertIn('deterministic test provider',json.dumps(c.updates))
    def test_cli_provider_change_preserves_old_history_and_starts_new_context(self):
        self.setup_kind('openai');c=self.client();s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.prompt(c,s);self.close(c)
        import sqlite3
        # Simulate a 0.1 transcript that did not yet have a provider binding.
        db=sqlite3.connect(self.home/'history.sqlite3')
        try:db.execute('DELETE FROM session_providers');db.commit()
        finally:db.close()
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'configure-provider','--provider','mock'],check=True,stdout=subprocess.DEVNULL)
        c=self.client();self.assertIn('result',c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]}))
        self.assertIn('different provider',self.prompt(c,s)['error']['message'])
        fresh=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
        self.assertEqual(self.prompt(c,fresh)['result']['stopReason'],'end_turn')
        self.assertEqual(len(self.server.case['requests']),2)

if __name__=='__main__':unittest.main(verbosity=2)
