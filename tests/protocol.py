"""Real stdio ACP + local Responses/MCP fixtures; no model account required."""
import json, os, queue, subprocess, tempfile, threading, time, unittest
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('AGENT_RUNTIME_TEST_BINARY', str(ROOT / ('target/debug/agent-runtime.exe' if os.name == 'nt' else 'target/debug/agent-runtime'))))
REQUESTS = []
CLOSE_STARTED = threading.Event()
CLOSE_RELEASE = threading.Event()

from responses_ws import ResponsesWebSocketMixin

class Handler(ResponsesWebSocketMixin, BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path == '/mcp':
            method=body.get('method')
            if method=='notifications/initialized':self.send_response(202);self.end_headers();return
            result={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}} if method=='initialize' else {'tools':[{'name':'test_read','description':'Read fixture data','inputSchema':{'type':'object','properties':{},'additionalProperties':False},'annotations':{'readOnlyHint':True}}]} if method=='tools/list' else {'content':[{'type':'text','text':'scoped fixture evidence'}]}
            self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Mcp-Session-Id','fixture-session');self.end_headers();self.wfile.write(json.dumps({'jsonrpc':'2.0','id':body['id'],'result':result}).encode());return
        if self.headers.get('Authorization') != 'Bearer fixture':
            self.send_response(401);self.end_headers();return
        REQUESTS.append(body)
        messages=body['input'];last_user=max(i for i,x in enumerate(messages) if x.get('role')=='user');text=messages[last_user]['content'];tail=messages[last_user+1:]
        if text == 'hold-for-close':
            CLOSE_STARTED.set();CLOSE_RELEASE.wait(10)
        elif 'slow' in text:time.sleep(2)
        output=[]
        if any(x.get('type')=='function_call_output' for x in tail):
            text='Tool result: '+next(x['output'] for x in tail if x.get('type')=='function_call_output')
        elif 'run native windows' in text:
            output=[{'type':'function_call','call_id':'command-'+str(len(REQUESTS)),'name':'workspace_run','arguments':json.dumps({'command':'cmd.exe','args':['/d','/c','echo ACP-command-proof>command-proof.txt']})}]
        elif text.startswith('read grant: '):
            output=[{'type':'function_call','call_id':'read-'+str(len(REQUESTS)),'name':'workspace_read','arguments':json.dumps({'path':text.removeprefix('read grant: ')})}]
        elif 'ask structured input' in text:
            output=[{'type':'function_call','call_id':'ask-'+str(len(REQUESTS)),'name':'user_input','arguments':json.dumps({'message':'Pick a release direction','questions':[{'id':'direction','label':'Release direction','kind':'choice','options':['small','large']}]})}]
        elif 'write' in text:
            output=[{'type':'function_call','call_id':'write-'+str(len(REQUESTS)),'name':'workspace_write','arguments':json.dumps({'path':'proof.txt','content':'verified payload'})}]
        elif 'mcp' in text:
            name=next(x['name'] for x in body['tools'] if x['name'].startswith('mcp_'));output=[{'type':'function_call','call_id':'mcp-'+str(len(REQUESTS)),'name':name,'arguments':'{}'}]
        elif 'lookup archived' in text:
            output=[{'type':'function_call','call_id':'history-'+str(len(REQUESTS)),'name':'session_history','arguments':json.dumps({'query':'original-apricot','limit':1})}]
        elif 'loop' in text:
            output=[{'type':'function_call','call_id':'clock-'+str(len(REQUESTS)),'name':'clock','arguments':'{}'}]
        else:text='Saved context: '+' | '.join(x['content'] for x in messages if x.get('role')=='user' and isinstance(x.get('content'),str))
        if 'loop' in text:output=[{'type':'function_call','call_id':'clock-'+str(len(REQUESTS)),'name':'clock','arguments':'{}'}]
        if not output:output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':text}]}]
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        try:
            if output[0]['type']=='message':self.wfile.write(('data: '+json.dumps({'type':'response.output_text.delta','delta':text})+'\n\n').encode())
            self.wfile.write(('data: '+json.dumps({'type':'response.completed','response':{'status':'completed','output':output,'usage':{'input_tokens':10,'output_tokens':20}}})+'\n\n').encode());self.wfile.flush()
        except (BrokenPipeError,ConnectionResetError,ConnectionAbortedError):pass

class Client:
    def __init__(self,home):
        self.proc=subprocess.Popen([str(BINARY),'--home',str(home),'acp'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8',env={**os.environ,'RUNTIME_FIXTURE_KEY':'fixture\n'})
        self.q=queue.Queue();self.next=0;self.updates=[];self.callbacks=[];self.form_reply={'action':'accept','content':{'direction':'small'}}
        threading.Thread(target=lambda:[self.q.put(json.loads(line)) for line in self.proc.stdout],daemon=True).start()
    def send(self,v):self.proc.stdin.write(json.dumps(v)+'\n');self.proc.stdin.flush()
    def request(self,method,params):self.next+=1;self.send({'jsonrpc':'2.0','id':self.next,'method':method,'params':params});return self.next
    def finish(self,id,approve=False):
        while True:
            v=self.q.get(timeout=12)
            if v.get('method')=='session/request_permission':
                self.callbacks.append(v)
                self.send({'jsonrpc':'2.0','id':v['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once' if approve else 'reject_once'}}})
            elif v.get('method')=='elicitation/create':
                self.callbacks.append(v)
                self.send({'jsonrpc':'2.0','id':v['id'],'result':self.form_reply})
            elif 'method' in v:self.updates.append(v)
            elif v.get('id')==id:return v
            else:raise AssertionError(v)
    def call(self,method,p,**kw):return self.finish(self.request(method,p),**kw)
    def close(self):self.proc.stdin.close();self.proc.wait(timeout=5);self.proc.stdout.close();self.proc.stderr.close()

class Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=cls.server.serve_forever,daemon=True).start()
    @classmethod
    def tearDownClass(cls):cls.server.shutdown();cls.server.server_close()
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'state';self.work=Path(self.tmp.name)/'work';self.clients=[]
        subprocess.run([str(BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--model','fixture'],check=True,stdout=subprocess.DEVNULL)
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['provider'].update(endpoint=f'http://127.0.0.1:{self.server.server_port}/responses',api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None);(self.home/'runtime.json').write_text(json.dumps(cfg))
    def tearDown(self):
        for c in self.clients:c.close()
        self.tmp.cleanup()
    def client(self):
        c=Client(self.home);self.clients.append(c);result=c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});self.assertTrue(result['result']['agentCapabilities']['loadSession']);return c
    def new(self,c,mcp=None):return c.call('session/new',{'cwd':str(self.work),'mcpServers':mcp or []})['result']['sessionId']
    def prompt(self,c,s,text,**kw):return c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':text}]},**kw)
    def test_refreshed_context_survives_resume_and_original_history_stays_available(self):
        c=self.client();s=self.new(c)
        def prompt(text,retained):return {'sessionId':s,'prompt':[{'type':'text','text':text,'_meta':{'com.microsoft.amplifier.context-snapshot':{'version':1,'key':'fixture:audience-a','retainedText':retained}}}]}
        c.call('session/prompt',prompt('original-apricot old snapshot','First actual request'))
        c.call('session/close',{'sessionId':s});c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]})
        c.call('session/prompt',prompt('new snapshot','Second actual request'))
        users=[x['content'] for x in REQUESTS[-1]['input'] if x.get('role')=='user']
        self.assertIn('First actual request',users[0]);self.assertNotIn('original-apricot',users[0]);self.assertEqual(users[-1],'new snapshot')
        self.prompt(c,s,'lookup archived');self.assertTrue(any('original-apricot' in json.dumps(u) for u in c.updates))
    def test_permission_denial_and_grant_are_actual_effects(self):
        c=self.client();s=self.new(c);self.prompt(c,s,'write it');self.assertFalse((self.work/'proof.txt').exists());self.assertEqual(len(c.callbacks),1)
        self.prompt(c,s,'write it again',approve=True);self.assertEqual((self.work/'proof.txt').read_text(),'verified payload');self.assertEqual(len(c.callbacks),2)
    def test_additional_directories_are_reissued_on_reconnect(self):
        shared=Path(self.tmp.name)/'shared';shared.mkdir();(shared/'data').write_text('GRANTED-ROOT-MARKER')
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['additional_workspaces']=[str(shared)];(self.home/'runtime.json').write_text(json.dumps(cfg))
        c=self.client();s=c.call('session/new',{'cwd':str(self.work),'additionalDirectories':[str(shared)],'mcpServers':[]})['result']['sessionId']
        self.prompt(c,s,'read grant: '+str(shared/'data'));self.assertTrue(any('GRANTED-ROOT-MARKER' in json.dumps(u) for u in c.updates))
        other=self.new(c);c.updates=[];self.prompt(c,other,'read grant: '+str(shared/'data'));self.assertFalse(any('GRANTED-ROOT-MARKER' in json.dumps(u) for u in c.updates))
        for roots in [[str(self.home)],['relative'],[str(shared.parent)]]:
            self.assertEqual(c.call('session/new',{'cwd':str(self.work),'additionalDirectories':roots,'mcpServers':[]})['error']['code'],-32602)
        c.call('session/close',{'sessionId':s});c.call('session/resume',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]});c.updates=[]
        self.prompt(c,s,'read grant: '+str(shared/'data'));self.assertFalse(any('GRANTED-ROOT-MARKER' in json.dumps(u) for u in c.updates))
    def test_system_prompt_is_owner_opt_in_and_connection_local(self):
        prompt={'mode':'append','content':[{'type':'text','text':'SYSTEM-APPEND-MARKER'}]}
        c=self.client();self.assertEqual(c.call('session/new',{'cwd':str(self.work),'mcpServers':[],'systemPrompt':prompt})['error']['code'],-32602)
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['allow_client_system_prompt']=True;(self.home/'runtime.json').write_text(json.dumps(cfg))
        c=Client(self.home);self.clients.append(c);init=c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.assertEqual(init['result']['agentCapabilities']['sessionCapabilities']['systemPrompt'],{})
        for invalid in [{'mode':'override','content':prompt['content']},{'mode':'append','content':[{'type':'image','data':'abc','mimeType':'image/png'}]}]:
            self.assertEqual(c.call('session/new',{'cwd':str(self.work),'mcpServers':[],'systemPrompt':invalid})['error']['code'],-32602)
        s=c.call('session/new',{'cwd':str(self.work),'mcpServers':[],'systemPrompt':prompt})['result']['sessionId']
        self.prompt(c,s,'hello');self.assertIn('SYSTEM-APPEND-MARKER',REQUESTS[-1]['instructions'])
        c.call('session/close',{'sessionId':s});c.call('session/resume',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]});self.prompt(c,s,'hello again')
        self.assertNotIn('SYSTEM-APPEND-MARKER',REQUESTS[-1]['instructions'])
    def test_form_elicitation_requires_explicit_mode_and_valid_answers(self):
        for capabilities in [{},{'elicitation':{}},{'elicitation':{'form':None}},{'elicitation':{'url':{}}}]:
            c=Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':capabilities});s=self.new(c)
            self.prompt(c,s,'ask structured input');self.assertEqual(c.callbacks,[])
            self.assertFalse(any(t.get('name')=='user_input' for t in REQUESTS[-1]['tools']))
        c=Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{'elicitation':{'form':{}}}});s=self.new(c)
        self.prompt(c,s,'ask structured input');self.assertEqual(c.callbacks[-1]['method'],'elicitation/create');self.assertEqual(c.callbacks[-1]['params']['sessionId'],s)
        self.assertEqual(c.callbacks[-1]['params']['requestedSchema']['properties']['direction']['enum'],['small','large'])
        self.assertTrue(any('small' in json.dumps(u) for u in c.updates))
        c.form_reply={'action':'decline','content':{'direction':'DECLINED-SECRET'}};c.updates=[];self.prompt(c,s,'ask structured input again')
        self.assertFalse(any('DECLINED-SECRET' in json.dumps(u) for u in c.updates))
        c.form_reply={'action':'accept','content':{'direction':'not-an-option'}};c.updates=[];self.prompt(c,s,'ask structured input again')
        self.assertTrue(any('not an offered choice' in json.dumps(u) for u in c.updates))
    def test_cancelling_form_notifies_client_and_ignores_late_answer(self):
        c=Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{'elicitation':{'form':{}}}});s=self.new(c)
        request=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'ask structured input'}]})
        while True:
            frame=c.q.get(timeout=12)
            if frame.get('method')=='elicitation/create':break
            self.assertIn('method',frame)
        c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':s}})
        result=c.finish(request);self.assertEqual(result['result']['stopReason'],'cancelled')
        self.assertTrue(any(u.get('method')=='$/cancel_request' and u['params']['requestId']==frame['id'] for u in c.updates))
        c.send({'jsonrpc':'2.0','id':frame['id'],'result':{'action':'accept','content':{'direction':'LATE-ANSWER'}}})
        c.updates=[];self.assertEqual(self.prompt(c,s,'hello after cancellation')['result']['stopReason'],'end_turn')
        self.assertFalse(any('LATE-ANSWER' in json.dumps(u) for u in c.updates))
    def test_v2_initialization_negotiates_only_supported_v1(self):
        c=Client(self.home);self.clients.append(c)
        result=c.call('initialize',{'protocolVersion':2,'info':{'name':'newer-client','version':'1'},'capabilities':{}})['result']
        self.assertEqual(result['protocolVersion'],1);self.assertIn('agentCapabilities',result);self.assertNotIn('capabilities',result)
        s=self.new(c);self.assertEqual(self.prompt(c,s,'hello')['result']['stopReason'],'end_turn')
    def test_resume_replays_and_preserves_model_context_across_processes(self):
        c=self.client();s=self.new(c);self.prompt(c,s,'unique apricot 123');c.close();self.clients.remove(c)
        c=self.client();r=c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]});self.assertIn('result',r);self.assertTrue(any('unique apricot' in json.dumps(x) for x in c.updates));self.prompt(c,s,'recall');self.assertTrue(any('unique apricot' in json.dumps(x) for x in c.updates));self.assertEqual(c.call('session/list',{})['result']['sessions'][0]['sessionId'],s)
    def test_resource_content_replays_original_blocks_without_automatic_file_access(self):
        c=self.client();s=self.new(c)
        private=Path(self.tmp.name)/'private.txt';private.write_text('NEVER_READ_CANARY')
        parts=[{'type':'resource_link','uri':private.as_uri(),'name':'private.txt'}, {'type':'resource','resource':{'uri':'file:///client/project/spec.md','mimeType':'text/markdown','text':'embedded-blueprint'}}]
        r=c.call('session/prompt',{'sessionId':s,'prompt':parts});self.assertIn('result',r)
        self.assertIn('embedded-blueprint',json.dumps(REQUESTS[-1]));self.assertNotIn('NEVER_READ_CANARY',json.dumps(REQUESTS[-1]));self.assertNotIn('_acp_prompt',json.dumps(REQUESTS[-1]))
        c.updates=[];c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]})
        replay=[u['params']['update']['content'] for u in c.updates if u['params']['update']['sessionUpdate']=='user_message_chunk']
        self.assertEqual(replay,parts)
        self.assertIn('error',c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'resource_link','name':'missing URI'}]}))
    def test_resume_close_delete_and_config_options(self):
        c=self.client();s=self.new(c);self.prompt(c,s,'retained conversation')
        r=c.call('session/set_config_option',{'sessionId':s,'configId':'mode','value':'read_only'})
        self.assertEqual(r['result']['configOptions'][0]['currentValue'],'read_only')
        self.assertIn('error',c.call('session/set_config_option',{'sessionId':s,'configId':'unknown','value':'ask'}))
        self.assertEqual(c.call('session/close',{'sessionId':s})['result'],{})
        self.assertEqual(c.call('session/close',{'sessionId':s})['result'],{})
        self.assertIn('error',self.prompt(c,s,'must load first'))
        c.updates=[];r=c.call('session/resume',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]})
        self.assertEqual(r['result']['configOptions'][0]['currentValue'],'read_only');self.assertEqual(c.updates,[])
        self.prompt(c,s,'recall');self.assertTrue(any('retained conversation' in json.dumps(u) for u in c.updates))
        self.assertEqual(c.call('session/delete',{'sessionId':s})['result'],{});self.assertEqual(c.call('session/delete',{'sessionId':s})['result'],{})
        self.assertEqual(c.call('session/delete',{'sessionId':'unknown-opaque-id'})['result'],{})
        self.assertEqual(c.call('session/list',{})['result']['sessions'],[])
        self.assertIn('error',c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]}))
    def test_close_cancels_active_turn_and_delete_cannot_race_another_process(self):
        c=self.client();s=self.new(c);other=self.client()
        CLOSE_STARTED.clear();CLOSE_RELEASE.clear()
        p=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'hold-for-close'}]})
        try:
            # Wait for actual model entry (and its held session lock), not a
            # scheduling guess. Slow Windows runners can exceed 150 ms here.
            self.assertTrue(CLOSE_STARTED.wait(5),'Prompt did not reach the held model fixture')
            self.assertIn('error',other.call('session/delete',{'sessionId':s}))
            close=c.request('session/close',{'sessionId':s});responses={}
            while len(responses)<2:
                msg=c.q.get(timeout=12)
                if 'method' in msg:c.updates.append(msg)
                else:responses[msg['id']]=msg
            self.assertEqual(responses[p]['result']['stopReason'],'cancelled');self.assertEqual(responses[close]['result'],{})
        finally:CLOSE_RELEASE.set()
        self.assertIn('error',self.prompt(c,s,'closed'))
    def test_native_history_is_public_bounded_and_store_bound(self):
        c=self.client();s=self.new(c)
        private={'type':'text','text':'[Configured agent instance context] PROFILE_CANARY [/Configured agent instance context]','_meta':{'io.github.bkrabach.agent-instance':{'context':True}}}
        literal='[Configured agent instance context] human literal example [/Configured agent instance context] preserve me'
        c.call('session/prompt',{'sessionId':s,'prompt':[private,{'type':'text','text':'human-first'},{'type':'text','text':literal}]})
        self.prompt(c,s,'human-second')
        # A provider reply can repeat source text. Use explicit public replies to
        # distinguish filtering of injected context from censorship of answers.
        import sqlite3
        db=sqlite3.connect(self.home/'history.sqlite3');items=json.loads(db.execute('select items from sessions where id=?',(s,)).fetchone()[0])
        for item in items:
            if item.get('role')=='assistant':item['content']=[{'type':'output_text','text':'Public response'}]
        items += [{'type':'reasoning','summary':[{'text':'PRIVATE_REASONING_CANARY'}]},{'type':'function_call_output','call_id':'test','output':'PRIVATE_TOOL_CANARY'}]
        db.execute('update sessions set items=? where id=?',(json.dumps(items),s));db.commit();db.close()
        path=Path(self.tmp.name)/'history-query.json'
        def history(query,success=True):
            path.write_text(json.dumps(query));r=subprocess.run([str(BINARY),'--home',str(self.home),'history-query',str(path)],capture_output=True,text=True,check=success)
            return json.loads(r.stdout) if success else r
        identity=history({'operation':'describe'})['store_id']
        page=history({'operation':'read','store_id':identity,'session_id':s,'limit':1})
        self.assertEqual(len(page['messages']),1);self.assertFalse(page['complete'])
        older=history({'operation':'read','store_id':identity,'session_id':s,'before':page['next_before'],'limit':40})
        public=json.dumps(older)+json.dumps(page)
        for secret in ['PROFILE_CANARY','PRIVATE_REASONING_CANARY','PRIVATE_TOOL_CANARY']:self.assertNotIn(secret,public)
        self.assertIn('human-first',public);self.assertIn('human-second',public);self.assertIn(literal,public)
        listing=history({'operation':'list','store_id':identity,'session_ids':[s]});self.assertEqual(listing['items'][0]['label'],'human-first')
        self.assertEqual(history({'operation':'list','store_id':identity,'session_ids':[]})['items'],[])
        self.assertNotEqual(history({'operation':'read','store_id':'wrong-store','session_id':s},False).returncode,0)
    def test_mcp_annotations_cannot_grant_permission(self):
        c=self.client();s=self.new(c,[{'type':'http','name':'fixture','url':f'http://127.0.0.1:{self.server.server_port}/mcp','headers':[]}])
        c.call('session/set_mode',{'sessionId':s,'modeId':'read_only'});self.prompt(c,s,'use mcp')
        self.assertFalse(any('scoped fixture evidence' in json.dumps(x) for x in c.updates));self.assertEqual(c.callbacks,[])
        c.call('session/set_mode',{'sessionId':s,'modeId':'ask'});self.prompt(c,s,'use mcp now',approve=True)
        self.assertTrue(any('scoped fixture evidence' in json.dumps(x) for x in c.updates));self.assertEqual(len(c.callbacks),1)
        raw=c.callbacks[0]['params']['toolCall']['rawInput'];self.assertEqual(raw['server'],'fixture');self.assertEqual(raw['tool'],'test_read');self.assertEqual(raw['arguments'],{})
    def test_trusted_readonly_mcp_endpoint_can_read_without_approval(self):
        url=f'http://127.0.0.1:{self.server.server_port}/mcp'
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['trusted_mcp_readonly_urls']=[url];(self.home/'runtime.json').write_text(json.dumps(cfg))
        c=self.client();s=self.new(c,[{'type':'http','name':'fixture','url':url,'headers':[]}]);self.prompt(c,s,'use mcp');self.assertTrue(any('scoped fixture evidence' in json.dumps(x) for x in c.updates));self.assertEqual(c.callbacks,[])
    def test_cancel_returns_without_waiting_for_model_and_allows_next_turn(self):
        c=self.client();s=self.new(c);id=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'slow'}]});time.sleep(.2);start=time.time();c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':s}});self.assertEqual(c.finish(id)['result']['stopReason'],'cancelled');self.assertLess(time.time()-start,1);self.assertEqual(self.prompt(c,s,'next')['result']['stopReason'],'end_turn')
    def test_unknown_methods_and_outside_workspaces_fail(self):
        c=self.client();self.assertIn('error',c.call('session/new',{'cwd':str(self.home),'mcpServers':[]}));self.assertEqual(c.call('made_up',{})['error']['code'],-32601)
    def test_client_cannot_launch_local_mcp_programs_by_default(self):
        import sys
        outside=Path(self.tmp.name)/'unauthorized.txt';c=self.client()
        r=c.call('session/new',{'cwd':str(self.work),'mcpServers':[{'name':'untrusted','command':sys.executable,'args':['-c',f'from pathlib import Path;Path({str(outside)!r}).write_text("bad")'],'env':[]}]})
        self.assertIn('error',r);self.assertIn('disabled',r['error']['message']);self.assertFalse(outside.exists())
    def test_parallel_connections_cannot_modify_one_running_session(self):
        a=self.client();s=self.new(a);b=self.client();b.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]});id=a.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'slow'}]});time.sleep(.2);self.assertIn('error',self.prompt(b,s,'collision'));self.assertIn('result',a.finish(id))
    def test_fragmented_frame_survives_other_request_completion(self):
        c=self.client();s=self.new(c)
        prompt_id=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'slow'}]})
        c.next+=1;list_id=c.next
        line=json.dumps({'jsonrpc':'2.0','id':list_id,'method':'session/list','params':{}})+'\n'
        c.proc.stdin.write(line[:16]);c.proc.stdin.flush()
        # Complete another request while this input frame is still partial.
        # Waiting on the actual reply avoids assuming a machine's scheduling
        # latency, and JSON-RPC does not promise response ordering.
        self.assertIn('result',c.finish(prompt_id))
        c.proc.stdin.write(line[16:]);c.proc.stdin.flush()
        self.assertEqual(c.finish(list_id)['result']['sessions'][0]['sessionId'],s)
    def test_read_only_mode_cannot_write(self):
        c=self.client();s=self.new(c);c.call('session/set_mode',{'sessionId':s,'modeId':'read_only'});self.prompt(c,s,'write');self.assertFalse((self.work/'proof.txt').exists());self.assertEqual(c.callbacks,[])
    def test_compaction_retains_full_replay_and_original_history_lookup_after_restart(self):
        import sqlite3
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['limits']['max_context_bytes']=24000
        (self.home/'runtime.json').write_text(json.dumps(cfg))
        c=self.client();s=self.new(c)
        self.assertIn('result',self.prompt(c,s,'original-apricot '+('archive detail '*250)))
        for i in range(8):self.assertIn('result',self.prompt(c,s,f'followup {i}: '+('current detail '*250)))
        db=sqlite3.connect(self.home/'history.sqlite3')
        try:
            saved=json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(s,)).fetchone()[0])
            checkpoints=db.execute('SELECT count(*) FROM context_checkpoints WHERE session=?',(s,)).fetchone()[0]
        finally:db.close()
        self.assertEqual(sum(x.get('role')=='user' for x in saved),9)
        self.assertTrue(saved[0]['content'].startswith('original-apricot'));self.assertGreater(checkpoints,0)
        self.assertTrue(any('Saved conversation archive' in json.dumps(r) for r in REQUESTS))
        c.close();self.clients.remove(c);c=self.client();c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]})
        self.assertTrue(any('original-apricot' in json.dumps(u) for u in c.updates))
        c.updates=[];self.assertEqual(self.prompt(c,s,'lookup archived')['result']['stopReason'],'end_turn')
        self.assertTrue(any('original-apricot' in json.dumps(u) for u in c.updates));self.assertEqual(c.callbacks,[])

if __name__=='__main__':unittest.main(verbosity=2)
