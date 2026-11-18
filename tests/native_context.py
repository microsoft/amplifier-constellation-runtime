"""Real ACP/provider boundaries: canonical windows, signed state and history."""
import hashlib, json, sqlite3, subprocess, tempfile, threading, unittest, unittest.mock
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from contextlib import closing
import protocol
from responses_ws import ResponsesWebSocketMixin

def digest(value):return hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest()[:24]
def followed(body):
    """What the live service ties each reasoning block of a request to: the
    instructions, the tools and every earlier block. Reasoning blocks and cache
    markers are not part of that. Returns the first block sent after anything
    else, or after another reasoning block than the one it was produced after,
    then what a reply to the whole request follows."""
    system=body.get('system') or ''
    earlier=[system if isinstance(system,str) else ''.join(b['text'] for b in system),body.get('tools')];previous=None;refused=None
    for m,message in enumerate(body['messages']):
        for b,block in enumerate(message['content']):
            if block['type']!='thinking':
                earlier.append([message['role'],{k:v for k,v in block.items() if k!='cache_control'}]);continue
            kind,_,record=block['signature'].partition(' ')
            if kind=='BOUND':
                after,_,chained=record.partition(' ')
                if refused is None and (after!=digest(earlier) or previous not in (None,chained)):refused=(m,b)
            previous=digest(block['signature'])
    return refused,f'BOUND {digest(earlier)} {previous}'

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
            window=getattr(self.server,'mcp_window',None)
            return self.reply({'output':window if window is not None else [{'role':'user','content':retained},{'type':'compaction','encrypted_content':'PRIVATE_OPAQUE'}],'usage':{'input_tokens':14000,'output_tokens':200}})
        refused,signature=followed(body) if self.server.bound else (None,None)
        if refused:
            # The live service refuses this request shape with exactly this error.
            return self.reply({'type':'error','error':{'type':'invalid_request_error','message':'messages.%d.content.%d: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to "drop_block".'%refused}},400)
        if body.get('compaction'):
            last=body['messages'][-1]
            if last['role']=='assistant' and last['content'][-1]['type'] in ('thinking','redacted_thinking'):
                # The live service refuses this request shape with exactly this error.
                return self.reply({'type':'error','error':{'type':'invalid_request_error','message':f"messages.{len(body['messages'])-1}: The final block in an assistant message cannot be `thinking`."}},400)
            if 'output_config' in body and not isinstance(body['output_config'],dict):
                return self.reply({'error':{'type':'invalid_request_error','message':'output_config must be an object'}},400)
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            events=[{'type':'message_start','message':{'role':'assistant','usage':{'input_tokens':0,'output_tokens':0}}},{'type':'ping'}, {'type':'content_block_start','index':0,'content_block':{'type':'compaction','content':'PRIVATE_SUMMARY','signature':'PRIVATE_SIGNATURE'}}, {'type':'content_block_stop','index':0}, {'type':'message_delta','delta':{'stop_reason':'max_tokens' if self.server.bad_compaction else 'compaction'},'usage':{'iterations':[{'type':'compaction','input_tokens':12000,'output_tokens':200}]}}, {'type':'message_stop'}]
            if self.server.truncate:events=events[:-1]
            for event in events:self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
            self.wfile.flush();return
        hold,self.server.hold=self.server.hold,None
        if hold:hold[0].set();hold[1].wait(10)
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        reported=self.server.reported_tokens
        if self.path=='/responses':
            events=[{'type':'response.completed','response':{'status':'completed','output':[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Finished'}]}],'usage':{'input_tokens':reported,'output_tokens':20,'input_tokens_details':{'cached_tokens':256}}}}]
        elif self.server.bound:
            # Reasoning tied to this request, then the next scripted tool call or a final text.
            call=self.server.script.pop(0) if self.server.script else None
            block=[{'type':'content_block_start','index':0,'content_block':{'type':'thinking','thinking':'','signature':''}},{'type':'content_block_delta','index':0,'delta':{'type':'signature_delta','signature':signature}},{'type':'content_block_stop','index':0}]
            block+=[{'type':'content_block_start','index':1,'content_block':{'type':'tool_use','id':f'call-{len(self.server.requests)}','name':call,'input':{}}},{'type':'content_block_delta','index':1,'delta':{'type':'input_json_delta','partial_json':''}}] if call else [{'type':'content_block_start','index':1,'content_block':{'type':'text','text':'Finished'}}]
            events=[{'type':'message_start','message':{'usage':{'input_tokens':reported,'output_tokens':0,'cache_read_input_tokens':256}}},*block,{'type':'content_block_stop','index':1},{'type':'message_delta','delta':{'stop_reason':'tool_use' if call else 'end_turn'},'usage':{'output_tokens':20}},{'type':'message_stop'}]
        else:
            # A reply with reasoning and no text is how a model stays silent.
            block=[{'type':'content_block_start','index':0,'content_block':{'type':'thinking','thinking':'','signature':''}},{'type':'content_block_delta','index':0,'delta':{'type':'signature_delta','signature':'PRIVATE_REASONING_SIGNATURE'}}] if self.server.silent else [{'type':'content_block_start','index':0,'content_block':{'type':'text','text':'Finished'}}]
            events=[{'type':'message_start','message':{'usage':{'input_tokens':reported,'output_tokens':0,'cache_read_input_tokens':256}}},*block,{'type':'content_block_stop','index':0},{'type':'message_delta','delta':{'stop_reason':'end_turn'},'usage':{'output_tokens':20}},{'type':'message_stop'}]
        for event in events:self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
        self.wfile.flush()

class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'home';self.work=Path(self.tmp.name)/'work';self.c=None
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);self.server.requests=[];self.server.discovered=[];self.server.bad_compaction=False;self.server.truncate=False;self.server.retain_large_input=False;self.server.compact_error=False;self.server.silent=False;self.server.reported_tokens=400;self.server.hold=None;self.server.bound=False;self.server.script=[]
        threading.Thread(target=self.server.serve_forever,daemon=True).start()
    def tearDown(self):
        if self.c:self.c.close()
        self.server.shutdown();self.server.server_close();self.tmp.cleanup()
    def start(self,kind,threshold=None,**context):
        model='gpt-6.1-sol' if kind=='openai' else 'claude-opus-5-5'
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--model',model],capture_output=True,check=True)
        p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['provider'].update(kind=kind,model=model,endpoint=f'http://127.0.0.1:{self.server.server_port}/'+('responses' if kind=='openai' else 'messages'),api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None)
        cfg['limits']['max_context_bytes']=64000
        if threshold:cfg['context']['compact_threshold_tokens']=threshold
        cfg['context'].update(context)
        p.write_text(json.dumps(cfg))
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

    def snapshot(self,text,retained):
        return {'type':'text','text':text,'_meta':{'com.microsoft.amplifier.context-snapshot':{'version':1,'key':'room','retainedText':retained}}}
    def marked(self,text,retained):
        return self.c.call('session/prompt',{'sessionId':self.sid,'prompt':[self.snapshot(text,retained)]})['result']['stopReason']
    def model_requests(self):
        return [b for p,b in self.server.requests if p in ('/responses','/messages') and not b.get('compaction')]
    def after_checkpoint(self,kind):
        # The first prompt alone needs compaction, so every later prompt is
        # saved after a provider window that already exists.
        prompts=[('FIRST_SNAPSHOT '+('a'*50000),'first request'),('SECOND_SNAPSHOT '+('b'*10000),'second request'),('THIRD_SNAPSHOT','third request')]
        self.start(kind)
        for text,retained in prompts:self.assertEqual(self.marked(text,retained),'end_turn')
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            saved=[i for i in json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0]) if i.get('role')=='user']
            checkpoints=[json.loads(r[0]) for r in db.execute('SELECT data FROM context_checkpoints')]
        # Saved history and the client's own blocks are exactly what was sent.
        self.assertEqual([i['content'] for i in saved],[text for text,_ in prompts])
        self.assertEqual([i['_acp_prompt'] for i in saved],[[self.snapshot(text,retained)] for text,retained in prompts])
        self.assertEqual([c['kind'] for c in checkpoints],['provider_native'])
        self.assertEqual(len([1 for p,b in self.server.requests if p.endswith('/compact') or b.get('compaction')]),1)
        second,third=self.model_requests()[-2:]
        # Each prompt is whole while it is the newest one.
        self.assertIn('SECOND_SNAPSHOT',json.dumps(second))
        sent=json.dumps(third.get('input') or third['messages'])
        self.assertNotIn('SECOND_SNAPSHOT',sent)
        self.assertIn('second request',sent)
        self.assertIn('THIRD_SNAPSHOT',sent)
        return checkpoints[0],second,third
    def cache_marks(self,body):
        return [(m,b) for m,message in enumerate(body['messages']) for b,block in enumerate(message['content']) if block.get('cache_control')=={'type':'ephemeral'}]
    def without_marks(self,messages):
        return [{**m,'content':[{k:v for k,v in block.items() if k!='cache_control'} for block in m['content']]} for m in messages]

    def test_openai_window_keeps_replacing_declared_old_snapshots(self):
        checkpoint,_,third=self.after_checkpoint('openai')
        window=[{'role':'user','content':'canonical retained item'},{'type':'compaction','encrypted_content':'PRIVATE_OPAQUE'}]
        self.assertEqual(checkpoint['model_items'],window)
        self.assertEqual(third['input'][:2],window)

    def test_anthropic_window_keeps_replacing_declared_old_snapshots_and_keeps_its_cache(self):
        checkpoint,second,third=self.after_checkpoint('anthropic')
        signed={'type':'compaction','content':'PRIVATE_SUMMARY','signature':'PRIVATE_SIGNATURE'}
        self.assertEqual(checkpoint['model_items'],[{'type':'provider_state','provider':'anthropic','message':{'role':'assistant','content':[signed]}}])
        self.assertEqual(third['messages'][0]['content'][0],signed)
        self.assertEqual([m['role'] for m in third['messages']],['assistant','user','assistant','user'])
        # The automatic breakpoint ends each request inside the newest prompt,
        # which the next prompt replaces. The block before that prompt is the
        # last one the next turn sends unchanged, and the turn after reads it.
        self.assertEqual(self.cache_marks(second),[(0,1)])
        self.assertEqual(self.cache_marks(third),[(0,1),(2,0)])
        self.assertEqual(self.without_marks(third['messages'])[0],self.without_marks(second['messages'])[0])
        self.assertEqual(third['cache_control'],{'type':'ephemeral'})
        self.assertEqual(third['system'][0]['cache_control'],{'type':'ephemeral'})

    def test_reasoning_bound_to_a_replaced_snapshot_is_not_sent_again(self):
        # The service ties a reasoning block to the exact messages before it.
        self.server.silent=True
        _,second,third=self.after_checkpoint('anthropic')
        reasoning=lambda body:[b for m in body['messages'] for b in m['content'] if b['type']=='thinking']
        self.assertEqual(len(reasoning(second)),1)
        # The reply to the summary still follows unchanged messages. The reply
        # to the second prompt followed text that the third prompt replaced.
        self.assertEqual(len(reasoning(third)),1)
        self.assertEqual([m['role'] for m in third['messages']],['assistant','user'])
        self.assertEqual(third['messages'][0],second['messages'][0])
        self.assertEqual(self.cache_marks(third),[(1,0)])
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            saved=json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0])
        self.assertEqual([i['message']['content'] for i in saved if i.get('type')=='provider_state'],[[{'type':'thinking','thinking':'','signature':'PRIVATE_REASONING_SIGNATURE'}]]*3)

    def test_waiting_input_refreshes_the_snapshot_it_is_sent_with(self):
        self.start('anthropic')
        self.assertEqual(self.marked('FIRST_SNAPSHOT '+('a'*50000),'first request'),'end_turn')
        started=threading.Event();release=threading.Event();self.server.hold=(started,release)
        pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[self.snapshot('SECOND_SNAPSHOT','second request')]})
        self.assertTrue(started.wait(5))
        waiting=self.c.call('_session/steering',{'sessionId':self.sid,'prompt':[self.snapshot('WAITING_SNAPSHOT','waiting request')],'_meta':{'steering':{'inputId':'waiting','idleBehavior':'promptRequired'}}})
        self.assertEqual(waiting['result']['outcome'],'injected')
        release.set();self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        self.assertEqual(self.marked('THIRD_SNAPSHOT','third request'),'end_turn')
        with_waiting,later=self.model_requests()[-2:]
        sent=json.dumps(with_waiting['messages'])
        self.assertNotIn('SECOND_SNAPSHOT',sent)
        self.assertIn('second request',sent)
        self.assertEqual(with_waiting['messages'][-1]['content'],[{'type':'text','text':'WAITING_SNAPSHOT'}])
        # Once saved, the input is rebuilt in the place it was first sent, so
        # the request after it starts with the same messages.
        before=len(with_waiting['messages'])-1
        self.assertEqual(self.without_marks(later['messages'])[:before],self.without_marks(with_waiting['messages'])[:before])
        self.assertIn('waiting request',json.dumps(later['messages'][before]))
        self.assertNotIn('WAITING_SNAPSHOT',json.dumps(later['messages']))
        self.assertEqual(len([1 for p,b in self.server.requests if b.get('compaction')]),1)

    def reasoning(self,body):return [b for m in body['messages'] for b in m['content'] if b['type']=='thinking']
    def reopen(self,**changes):
        self.c.close();p=self.home/'runtime.json';p.write_text(json.dumps({**json.loads(p.read_text()),**changes}))
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
    def saved_replies(self):
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            return [i for i in json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0]) if i.get('type')=='provider_state']
    def ends(self,text):self.assertEqual(self.prompt(text),{'jsonrpc':'2.0','id':self.c.next,'result':{'stopReason':'end_turn','_meta':unittest.mock.ANY}})

    # From here the fixture answers as the service does where it enforces the
    # rule: every reply opens with reasoning tied to the instructions, the
    # tools and every earlier block, and a request that sends such a block
    # back after anything else is refused.
    def test_reasoning_is_left_out_once_the_instructions_change(self):
        self.server.bound=True;self.start('anthropic')
        self.ends('First request');self.ends('Second request');self.ends('Third request')
        # Nothing changed: every earlier reply is sent back whole.
        self.assertEqual(len(self.reasoning(self.model_requests()[-1])),2)
        self.reopen(instructions='Changed instructions')
        self.ends('Fourth request')
        fourth=self.model_requests()[-1]
        self.assertIn('Changed instructions',json.dumps(fourth['system']))
        self.assertEqual(self.reasoning(fourth),[])
        self.assertEqual([[b['type'] for b in m['content']] for m in fourth['messages']],[['text']]*7)
        # The reply produced after the change still follows what is sent.
        self.ends('Fifth request')
        fifth=self.model_requests()[-1]
        self.assertEqual([[b['type'] for b in m['content']] for m in fifth['messages']],[['text']]*7+[['thinking','text'],['text']])
        self.assertEqual(self.without_marks(fifth['messages'])[:7],self.without_marks(fourth['messages']))
        # Saved history keeps each reply as the provider returned it.
        self.assertEqual([[b['type'] for b in i['message']['content']] for i in self.saved_replies()],[['thinking','text']]*5)
        self.assertEqual({i['message']['content'][0]['signature'].split()[0] for i in self.saved_replies()},{'BOUND'})

    def test_reasoning_produced_after_a_window_is_left_out_when_the_window_cannot_be_used(self):
        # The first turn compacts between its two requests, so every later
        # reply follows the provider's window.
        self.server.bound=True;self.server.script=['clock'];self.server.reported_tokens=5000
        self.start('anthropic',threshold=1024)
        self.ends('First request');self.ends('Second request')
        second=self.model_requests()[-1]
        self.assertEqual([b['type'] for b in second['messages'][0]['content']],['compaction','thinking','text'])
        # Changed instructions leave the checkpoint unusable. The saved
        # transcript is sent in place of the window.
        self.reopen(instructions='Changed instructions')
        self.ends('Third request')
        third=self.model_requests()[-1]
        self.assertEqual([[b['type'] for b in m['content']] for m in third['messages']],[['text'],['tool_use'],['tool_result'],['text'],['text'],['text'],['text']])
        self.assertEqual(len([1 for p,b in self.server.requests if b.get('compaction')]),1)
        self.assertEqual([[b['type'] for b in i['message']['content']] for i in self.saved_replies()],[['thinking','tool_use']]+[['thinking','text']]*3)

    def test_a_summary_request_leaves_out_reasoning_bound_to_other_instructions(self):
        self.server.bound=True;self.start('anthropic')
        self.ends('First request');self.ends('Second '+('x'*40000))
        self.assertFalse(any(b.get('compaction') for p,b in self.server.requests))
        # The next prompt needs maintenance before its first model request.
        self.reopen(instructions='Changed instructions')
        self.ends('Third '+('x'*10000))
        summary=next(b for p,b in self.server.requests if b.get('compaction'))
        self.assertEqual(self.reasoning(summary),[])
        self.assertIn('Third',json.dumps(summary['messages'][-1]))
        self.assertEqual([b['type'] for b in self.model_requests()[-1]['messages'][0]['content']],['compaction'])

    def test_reasoning_is_left_out_after_turns_before_it_are_archived(self):
        self.server.bound=True;self.start('anthropic',provider_compaction=False)
        for n in ('First','Second','Third'):self.ends(n+' request '+('x'*20000))
        third=self.model_requests()[-1]
        # The first turn no longer fits: an archive note stands in for it, and
        # the second reply followed the turn itself.
        self.assertIn('Saved conversation archive',third['messages'][0]['content'][0]['text'])
        self.assertIn('Second request',third['messages'][0]['content'][1]['text'])
        self.assertEqual(self.reasoning(third),[])
        self.assertEqual([[b['type'] for b in m['content']] for m in third['messages']],[['text','text'],['text'],['text']])
        # Each new cut changes the note the kept turns follow.
        self.ends('Fourth request '+('x'*20000))
        fourth=self.model_requests()[-1]
        self.assertIn('Third request',fourth['messages'][0]['content'][1]['text'])
        self.assertEqual(self.reasoning(fourth),[])
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            self.assertEqual([json.loads(r[0])['kind'] for r in db.execute('SELECT data FROM context_checkpoints')],['extractive']*2)
        self.assertEqual([[b['type'] for b in i['message']['content']] for i in self.saved_replies()],[['thinking','text']]*4)

    def test_waiting_input_before_a_checkpoint_refreshes_the_snapshot_it_is_sent_with(self):
        self.server.bound=True;self.server.script=['clock','clock'];self.start('anthropic')
        started=threading.Event();release=threading.Event();self.server.hold=(started,release)
        pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[self.snapshot('FIRST_SNAPSHOT','first request')]})
        self.assertTrue(started.wait(5))
        waiting=self.c.call('_session/steering',{'sessionId':self.sid,'prompt':[self.snapshot('WAITING_SNAPSHOT','waiting request')],'_meta':{'steering':{'inputId':'waiting','idleBehavior':'promptRequired'}}})
        self.assertEqual(waiting['result']['outcome'],'injected')
        release.set();self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        first,with_waiting,later=self.model_requests()
        self.assertEqual(first['messages'],[{'role':'user','content':[{'type':'text','text':'FIRST_SNAPSHOT'}]}])
        # The input replaces the prompt's snapshot in the request that carries
        # it. The first reply followed the whole prompt, so only its tool call
        # is sent from here on.
        sent=json.dumps(with_waiting['messages'])
        self.assertNotIn('FIRST_SNAPSHOT',sent)
        self.assertIn('first request',sent)
        self.assertEqual([[b['type'] for b in m['content']] for m in with_waiting['messages']],[['text'],['tool_use'],['tool_result','text']])
        self.assertEqual(self.without_marks(with_waiting['messages'])[-1]['content'][-1],{'type':'text','text':'WAITING_SNAPSHOT'})
        # The second reply followed that request, and the next one sends the
        # same messages before it: its reasoning stays while its call is open.
        before=len(with_waiting['messages'])
        self.assertEqual(self.without_marks(later['messages'])[:before],self.without_marks(with_waiting['messages']))
        self.assertEqual([[b['type'] for b in m['content']] for m in later['messages'][before:]],[['thinking','tool_use'],['tool_result']])
        self.assertFalse(any(b.get('compaction') for p,b in self.server.requests))
        self.assertEqual([[b['type'] for b in i['message']['content']] for i in self.saved_replies()],[['thinking','tool_use']]*2+[['thinking','text']])

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

    def test_restored_mcp_window_budgets_unique_facts_but_keeps_canonical_checkpoint(self):
        facts={'receipt':'retained-'+('x'*36000),'submitted':True,'recipient_delivery_verified':False}
        envelope={'content':[{'type':'text','text':json.dumps(facts,separators=(',',':'))}], 'structuredContent':facts,'isError':False}
        window=[{'role':'user','content':'Retained request'},
                {'type':'function_call','call_id':'fixture-read','name':'mcp_0_receipt','arguments':'{}'},
                {'type':'function_call_output','call_id':'fixture-read','output':json.dumps(envelope,separators=(',',':'))},
                {'type':'compaction','encrypted_content':'PRIVATE_OPAQUE'}]
        self.server.mcp_window=window;self.start('openai')
        self.assertEqual(self.prompt('Original '+('x'*50000))['result']['stopReason'],'end_turn')
        self.c.close();self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        self.assertEqual(self.prompt('Clarify this established receipt')['result']['stopReason'],'end_turn')
        self.assertEqual(len([1 for p,b in self.server.requests if p.endswith('/compact')]),1)
        for _,body in [(p,b) for p,b in self.server.requests if p=='/responses']:
            result=json.loads(next(i['output'] for i in body['input'] if i.get('type')=='function_call_output'))
            self.assertEqual(result['content'],[]);self.assertEqual(result['structuredContent'],facts)
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            checkpoint=json.loads(db.execute("SELECT data FROM context_checkpoints WHERE data LIKE '%provider_native%'").fetchone()[0])
        self.assertEqual(checkpoint['model_items'],window)
        self.assertFalse(self.c.callbacks)

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

    def test_summary_request_leaves_out_reasoning_that_ends_the_window(self):
        # The model stays silent on one input while another is already waiting.
        # Maintenance then runs on a window that ends on reasoning alone.
        started=threading.Event();release=threading.Event()
        self.server.silent=True;self.server.reported_tokens=5000;self.server.hold=(started,release)
        self.start('anthropic',threshold=1024)
        pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':'Overheard: first line'}]})
        self.assertTrue(started.wait(5))
        waiting=self.c.call('_session/steering',{'sessionId':self.sid,'prompt':[{'type':'text','text':'Overheard: second line'}],'_meta':{'steering':{'inputId':'second-line','idleBehavior':'promptRequired'}}})
        self.assertEqual(waiting['result']['outcome'],'injected')
        release.set();self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        sent=[b for p,b in self.server.requests if p=='/messages']
        self.assertEqual([bool(b.get('compaction')) for b in sent],[False,True,False])
        self.assertEqual([m['role'] for m in sent[1]['messages']],['user'])
        self.assertIn('first line',json.dumps(sent[1]['messages']))
        self.assertNotIn('second line',json.dumps(sent[1]['messages']),'An input not yet accepted is not summary source')
        self.assertEqual(sent[2]['messages'][0],{'role':'assistant','content':[{'type':'compaction','content':'PRIVATE_SUMMARY','signature':'PRIVATE_SIGNATURE'}]})
        self.assertIn('second line',json.dumps(sent[2]['messages'][1]))
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            saved=json.loads(db.execute('SELECT items FROM sessions WHERE id=?',(self.sid,)).fetchone()[0])
            kinds=[r[0] for r in db.execute('SELECT kind FROM events')]
            self.assertEqual(db.execute('SELECT state,recorded FROM turn_inputs').fetchall(),[('incorporated',1)])
            self.assertEqual(db.execute('SELECT count(*) FROM context_checkpoints').fetchone()[0],1)
        # Saved history keeps the reply exactly as the provider returned it.
        self.assertEqual(saved[1]['message']['content'],[{'type':'thinking','thinking':'','signature':'PRIVATE_REASONING_SIGNATURE'}])
        self.assertNotIn('context.compaction.failed',kinds)
        self.assertNotIn('PRIVATE_REASONING_SIGNATURE',json.dumps(self.c.updates))

if __name__=='__main__':unittest.main()
