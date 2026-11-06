"""Real ACP/provider boundaries: canonical windows, signed state and history."""
import json, sqlite3, subprocess, tempfile, threading, unittest
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from contextlib import closing
import protocol
from responses_ws import ResponsesWebSocketMixin

class Handler(ResponsesWebSocketMixin,BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value,status=200):
        self.send_response(status);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps(value).encode())
    def do_GET(self):
        if self.path.startswith('/models/'):
            self.server.discovered.append(self.headers.get('anthropic-beta'))
            return self.reply({'capabilities':{'compaction':{'summarize':{'supported':True}}}})
        return super().do_GET()
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.server.requests.append((self.path,body))
        if self.path.endswith('/compact'):
            if self.server.compact_error:
                return self.reply({'error':{'code':'context_length_exceeded','message':'PRIVATE_ERROR_CANARY','param':'PRIVATE_PARAM_CANARY'}},400)
            if len(json.dumps(body['input'])) > 100000:
                return self.reply({'error':{'code':'context_length_exceeded'}},400)
            retained=body['input'][0]['content'] if self.server.retain_large_input else 'canonical retained item'
            return self.reply({'output':[{'role':'user','content':retained},{'type':'compaction','encrypted_content':'PRIVATE_OPAQUE'}],'usage':{'input_tokens':14000,'output_tokens':200}})
        if body.get('compaction'):
            if 'output_config' in body and not isinstance(body['output_config'],dict):
                return self.reply({'error':{'type':'invalid_request_error','message':'output_config must be an object'}},400)
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            events=[{'type':'message_start','message':{'role':'assistant','usage':{'input_tokens':0,'output_tokens':0}}},{'type':'ping'}, {'type':'content_block_start','index':0,'content_block':{'type':'compaction','content':'PRIVATE_SUMMARY','signature':'PRIVATE_SIGNATURE'}}, {'type':'content_block_stop','index':0}, {'type':'message_delta','delta':{'stop_reason':'max_tokens' if self.server.bad_compaction else 'compaction'},'usage':{'iterations':[{'type':'compaction','input_tokens':12000,'output_tokens':200}]}}, {'type':'message_stop'}]
            if self.server.truncate:events=events[:-1]
            for event in events:self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
            self.wfile.flush();return
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        if self.path=='/responses':
            events=[{'type':'response.completed','response':{'status':'completed','output':[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Finished'}]}],'usage':{'input_tokens':400,'output_tokens':20,'input_tokens_details':{'cached_tokens':256}}}}]
        else:
            events=[{'type':'message_start','message':{'usage':{'input_tokens':400,'output_tokens':0,'cache_read_input_tokens':256}}},{'type':'content_block_start','index':0,'content_block':{'type':'text','text':'Finished'}},{'type':'content_block_stop','index':0},{'type':'message_delta','delta':{'stop_reason':'end_turn'},'usage':{'output_tokens':20}},{'type':'message_stop'}]
        for event in events:self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
        self.wfile.flush()

class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'home';self.work=Path(self.tmp.name)/'work';self.c=None
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);self.server.requests=[];self.server.discovered=[];self.server.bad_compaction=False;self.server.truncate=False;self.server.retain_large_input=False;self.server.compact_error=False
        threading.Thread(target=self.server.serve_forever,daemon=True).start()
    def tearDown(self):
        if self.c:self.c.close()
        self.server.shutdown();self.server.server_close();self.tmp.cleanup()
    def start(self,kind):
        model='gpt-6.1-sol' if kind=='openai' else 'claude-opus-5-5'
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--model',model],capture_output=True,check=True)
        p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['provider'].update(kind=kind,model=model,endpoint=f'http://127.0.0.1:{self.server.server_port}/'+('responses' if kind=='openai' else 'messages'),api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None)
        cfg['limits']['max_context_bytes']=64000;p.write_text(json.dumps(cfg))
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.sid=self.c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
    def prompt(self,text):return self.c.call('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':text}]})
    def test_existing_oversized_transcript_is_bounded_before_native_compaction(self):
        self.start('openai');self.c.close();self.c=None
        original=[]
        for i in range(120):
            original.extend([{'role':'user','content':f'historical request {i}: '+('x'*10000)},
                             {'type':'message','role':'assistant','content':[{'type':'output_text','text':'Recorded historical result'}]}])
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            db.execute('UPDATE sessions SET items=? WHERE id=?',(json.dumps(original),self.sid));db.commit()
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        self.assertEqual(self.prompt('A fresh question, without repeating earlier actions')['result']['stopReason'],'end_turn')
        sent=[b for p,b in self.server.requests if p=='/responses'][-1]
        self.assertLess(len(json.dumps(sent['input'])),64000)
        self.assertIn('session_history',json.dumps(sent['input']))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            saved=json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0])
            checkpoints=[json.loads(r[0]) for r in db.execute('SELECT data FROM context_checkpoints')]
        self.assertEqual(saved[:len(original)],original)
        self.assertTrue(any(c['kind']=='extractive' for c in checkpoints))
        self.assertFalse(self.c.callbacks)

    def test_native_path_replaces_only_declared_old_snapshots(self):
        self.start('openai')
        def marked(text,retained):
            return self.c.call('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':text,'_meta':{'com.microsoft.amplifier.context-snapshot':{'version':1,'key':'room','retainedText':retained}}}]})
        self.assertEqual(marked('OLD_SNAPSHOT '+('a'*10000),'original request')['result']['stopReason'],'end_turn')
        self.assertEqual(marked('NEW_SNAPSHOT','new request')['result']['stopReason'],'end_turn')
        sent=[b for p,b in self.server.requests if p=='/responses'][-1]
        self.assertNotIn('OLD_SNAPSHOT',json.dumps(sent['input']))
        self.assertIn('original request',json.dumps(sent['input']))
        self.assertIn('NEW_SNAPSHOT',json.dumps(sent['input']))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            self.assertIn('OLD_SNAPSHOT',db.execute('SELECT items FROM sessions').fetchone()[0])

    def test_compaction_rejection_records_safe_reason_not_provider_payload(self):
        self.start('openai');self.server.compact_error=True
        self.assertIn('error',self.prompt('Original '+('x'*50000)))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            data=[json.loads(r[0]) for r in db.execute("SELECT data FROM events WHERE kind='context.compaction.failed'")]
            events=' '.join(r[0] for r in db.execute('SELECT data FROM events'))
        self.assertEqual(data[-1]['reason'],'context_length_exceeded')
        self.assertEqual(data[-1]['http_status'],400)
        self.assertNotIn('PRIVATE_ERROR_CANARY',events)
        self.assertNotIn('PRIVATE_PARAM_CANARY',events)
        self.assertFalse(any(p=='/responses' for p,b in self.server.requests))

    def test_openai_canonical_window_survives_restart_without_pruning_or_exporting_opaque_state(self):
        self.start('openai');original='original evidence '+('x'*50000)
        self.assertEqual(self.prompt(original)['result']['stopReason'],'end_turn')
        sent=[b for p,b in self.server.requests if p=='/responses'][-1]
        self.assertEqual(sent['input'][0]['content'],'canonical retained item')
        self.assertEqual(sent['input'][1]['encrypted_content'],'PRIVATE_OPAQUE')
        self.assertNotIn('context_management',sent,'Automatic compaction rejects native steering')
        self.c.close();self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        self.assertNotIn('PRIVATE_OPAQUE',json.dumps(self.c.updates))
        self.assertEqual(self.prompt('Continue independently')['result']['stopReason'],'end_turn')
        sent=[b for p,b in self.server.requests if p=='/responses'][-1]
        self.assertEqual(sent['input'][0]['content'],'canonical retained item')
        self.assertEqual(sent['input'][1]['encrypted_content'],'PRIVATE_OPAQUE')
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            saved=json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0])
            events=' '.join(r[0] for r in db.execute('SELECT data FROM events'))
        self.assertEqual(saved[0]['content'],original)
        self.assertNotIn('PRIVATE_OPAQUE',events)
        self.assertEqual(len([1 for p,b in self.server.requests if p.endswith('/compact')]),1)
    def test_retained_large_user_input_does_not_compact_again_without_material_growth(self):
        self.server.retain_large_input=True;self.start('openai')
        self.assertEqual(self.prompt('Original '+('x'*50000))['result']['stopReason'],'end_turn')
        self.c.close();self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        self.assertEqual(self.prompt('Small continuation')['result']['stopReason'],'end_turn')
        self.assertEqual(len([1 for p,b in self.server.requests if p.endswith('/compact')]),1)

    def test_anthropic_negotiates_and_preserves_exact_signed_block_and_stable_prefix(self):
        self.start('anthropic')
        self.assertEqual(self.prompt('Original '+('x'*50000))['result']['stopReason'],'end_turn')
        compact=next(b for p,b in self.server.requests if b.get('compaction'))
        sent=[b for p,b in self.server.requests if p=='/messages' and not b.get('compaction')][-1]
        self.assertEqual(sent['messages'][0],{'role':'assistant','content':[{'type':'compaction','content':'PRIVATE_SUMMARY','signature':'PRIVATE_SIGNATURE'}]})
        self.assertEqual(sent['system'],compact['system']);self.assertEqual(sent['tools'],compact['tools'])
        self.assertEqual(sent['system'][0]['cache_control'],{'type':'ephemeral'})
        self.assertIn('compact-2026-09-04',self.server.discovered[0])
        self.assertNotIn('PRIVATE_SIGNATURE',json.dumps(self.c.updates))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            events=[json.loads(r[0]) for r in db.execute("SELECT data FROM events WHERE kind='context.compacted'")]
        self.assertEqual(events[0]['usage']['iterations'][0]['input_tokens'],12000)
    def test_signed_block_without_terminal_stream_settlement_is_not_applied(self):
        self.start('anthropic');self.server.truncate=True
        self.assertIn('error',self.prompt('Original '+('x'*50000)))
        self.assertFalse(any(p=='/messages' and not b.get('compaction') for p,b in self.server.requests))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM context_checkpoints').fetchone()[0],0)

    def test_failed_compaction_never_replaces_history_or_starts_followup_inference(self):
        self.start('anthropic');self.server.bad_compaction=True
        self.assertIn('error',self.prompt('Original '+('x'*50000)))
        self.assertFalse(any(p=='/messages' and not b.get('compaction') for p,b in self.server.requests))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            self.assertEqual(db.execute('SELECT count(*) FROM context_checkpoints').fetchone()[0],0)
            self.assertIn('Original',db.execute('SELECT items FROM sessions').fetchone()[0])

if __name__=='__main__':unittest.main()
