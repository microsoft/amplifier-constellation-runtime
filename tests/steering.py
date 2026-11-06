from contextlib import closing
"""Scripted real WebSocket + ACP tests. No model account, no sleeps for ordering."""
import base64, hashlib, json, os, queue, socket, sqlite3, subprocess, tempfile, threading, time, unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import protocol
from responses_ws import ResponsesWebSocketMixin

class Wire(ResponsesWebSocketMixin,BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        self.protocol_version="HTTP/1.1"
        self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade')
        self.send_header('Sec-WebSocket-Accept',base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode());self.end_headers()
        self.close_connection=True;self.server.connection.put(self)
        try:
            while (v:=self._ws_read()) is not None:self.server.frames.put(v)
        except (OSError,ValueError):pass

class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'state';self.work=Path(self.tmp.name)/'work'
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Wire);self.server.frames=queue.Queue();self.server.connection=queue.Queue();threading.Thread(target=self.server.serve_forever,daemon=True).start()
        subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--model','gpt-6.1-sol'],check=True,capture_output=True)
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['provider'].update(endpoint=f'http://127.0.0.1:{self.server.server_port}/responses',api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None);(self.home/'runtime.json').write_text(json.dumps(cfg))
        self.c=protocol.Client(self.home);self.assertTrue(self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})['result']['_meta']['steering']['supported'])
        self.sid=self.c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.wire=None
    def tearDown(self):
        self.c.close();self.server.shutdown();self.server.server_close();self.tmp.cleanup()
    def start(self):
        pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':'Do the original work'}]})
        self.wire=self.server.connection.get(timeout=10);body=self.frame();self.assertEqual(body['type'],'response.create');self.assertNotIn('stream',body)
        self.created('r1');return pid
    def frame(self):return self.server.frames.get(timeout=10)
    def event(self,type,**args):self.wire._ws_send({'type':type,**args})
    def created(self,id,parent=None):self.event('response.created',response={'id':id,**({'previous_response_id':parent} if parent else {})})
    def completed(self,id,output=None,steered=False):
        self.event('response.incomplete' if steered else 'response.completed',response={'id':id,'status':'incomplete' if steered else 'completed','incomplete_details':{'reason':'steered'} if steered else None,'output':output or [{'type':'message','role':'assistant','content':[{'type':'output_text','text':'answer'}]}],'usage':{'input_tokens':1,'output_tokens':1}})
    def steer(self,text,id):return self.c.call('_session/steering',{'sessionId':self.sid,'prompt':[{'type':'text','text':text}],'_meta':{'steering':{'idleBehavior':'promptRequired','inputId':id}}})
    def accepted(self,id,parent):self.event('response.steer.accepted',steer={'id':id,'previous_response_id':parent})
    def receipts(self):
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:return db.execute('SELECT input_id,state,recorded FROM turn_inputs ORDER BY sequence').fetchall()
    def reconfigure(self, **limits):
        self.c.close();p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['limits'].update(limits);cfg['context']['auto_compact']=False;p.write_text(json.dumps(cfg))
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
    def test_revised_answers_have_distinct_live_and_replayed_message_ids(self):
        pid=self.start()
        old='{"decision":"speak","text":"Old answer","reason":"before update"}'
        new='{"decision":"pass","reason":"peer finished it"}'
        self.event('response.output_item.added',item={'id':'old-item','type':'message','role':'assistant','phase':'final_answer'})
        self.event('response.output_text.delta',item_id='old-item',delta=old)
        self.steer('Peer has finished it','peer-finished');self.frame();self.accepted('s1','r1')
        self.completed('r1',[{'id':'old-item','type':'message','role':'assistant','phase':'final_answer','content':[{'type':'output_text','text':old}]}],steered=True)
        self.created('r2','r1')
        self.event('response.output_item.added',item={'id':'new-item','type':'message','role':'assistant','phase':'final_answer'})
        self.event('response.output_text.delta',item_id='new-item',delta=new)
        self.completed('r2',[{'id':'new-item','type':'message','role':'assistant','phase':'final_answer','content':[{'type':'output_text','text':new}]}])
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        chunks=[u['params']['update'] for u in self.c.updates if u.get('params',{}).get('update',{}).get('sessionUpdate')=='agent_message_chunk' and u['params']['update'].get('content',{}).get('text')]
        self.assertEqual([(u['messageId'],u['content']['text']) for u in chunks],[('old-item',old),('new-item',new)])
        self.c.updates=[]
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        replay=[u['params']['update'] for u in self.c.updates if u.get('params',{}).get('update',{}).get('sessionUpdate')=='agent_message_chunk']
        self.assertEqual([(u['messageId'],u['content']['text']) for u in replay],[('old-item',old),('new-item',new)])

    def test_duplicate_response_identity_does_not_consume_budget(self):
        self.reconfigure(max_model_calls=1)
        pid=self.start();self.created('r1');self.completed('r1')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
    def test_quiet_healthy_response_survives_transport_idle_with_pongs(self):
        self.reconfigure(turn_seconds=0,provider_idle_seconds=1)
        pid=self.start();time.sleep(2.2)
        self.completed('r1')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        self.assertTrue(self.server.frames.empty(),'heartbeats must not issue another model request')
    def test_dead_websocket_stops_without_replaying_prompt(self):
        self.server.ignore_pings=True
        self.reconfigure(turn_seconds=0,provider_idle_seconds=1)
        pid=self.start();reply=self.c.finish(pid)
        self.assertIn('stopped answering',reply['error']['message'])
        self.assertTrue(self.server.frames.empty())
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            events=db.execute("SELECT data FROM events WHERE kind='model.failed'").fetchall()
        self.assertTrue(any(json.loads(e[0]).get('code')=='transport_idle' for e in events))
    def test_explicit_whole_turn_deadline_is_distinct_from_request_budget(self):
        self.reconfigure(turn_seconds=1,provider_idle_seconds=10)
        pid=self.start();reply=self.c.finish(pid)['result']
        self.assertEqual(reply['stopReason'],'max_turn_requests')
        self.assertEqual(reply['_meta']['io.github.bkrabach.agent-runtime']['stopCause'],'turn_deadline')
    def test_input_before_first_create_fits_original_call_budget(self):
        self.reconfigure(max_model_calls=1)
        entered=threading.Event();release=threading.Event();original=Wire.do_GET
        def delayed(handler):entered.set();release.wait(10);original(handler)
        Wire.do_GET=delayed
        try:
            pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':'Original'}]})
            self.assertTrue(entered.wait(5));self.steer('Include this before starting','early');release.set()
            self.wire=self.server.connection.get(timeout=10);request=self.frame();self.assertIn('Include this before starting',json.dumps(request))
            self.created('r1');self.completed('r1');self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
            self.assertEqual(self.receipts(),[('early','incorporated',1)])
        finally:release.set();Wire.do_GET=original
    def test_native_steering_respects_context_capacity(self):
        self.reconfigure(max_context_bytes=24000)
        pid=self.start();self.steer('x'*30000,'too-large')
        while True:
            v=self.c.q.get(timeout=10)
            if v.get('params',{}).get('update',{}).get('_meta',{}).get('io.github.bkrabach.agent-runtime',{}).get('inputReceipt',{}).get('state')=='notAccepted':break
        self.assertTrue(self.server.frames.empty());self.completed('r1')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn');self.assertEqual(self.receipts()[0][1],'notAccepted')
    def test_successor_disconnect_quarantines_old_input_without_blocking_new_work(self):
        pid=self.start();self.steer('Important correction','correction');self.frame();self.accepted('s1','r1');self.completed('r1',steered=True);self.created('r2','r1')
        while True:
            v=self.c.q.get(timeout=10)
            if v.get('params',{}).get('update',{}).get('_meta',{}).get('io.github.bkrabach.agent-runtime',{}).get('inputReceipt',{}).get('state')=='incorporated':break
        self.wire.connection.shutdown(socket.SHUT_RDWR);self.wire.connection.close();self.assertIn('error',self.c.finish(pid))
        self.assertEqual(self.receipts(),[('correction','incorporated',0)])
        next_prompt=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[{'type':'text','text':'A new independent question'}]})
        self.wire=self.server.connection.get(timeout=10);request=self.frame()
        self.assertNotIn('Important correction',json.dumps(request['input']))
        self.assertIn('Do the original work',json.dumps(request['input']))
        self.assertIn('quarantined',request['instructions']);self.assertIn('correction',request['instructions'])
        self.created('fresh');self.completed('fresh');self.assertEqual(self.c.finish(next_prompt)['result']['stopReason'],'end_turn')
        receipts=self.c.call('_session/input_receipts',{'sessionId':self.sid})['result']['inputReceipts'];self.assertEqual(receipts[0]['prompt'][0]['text'],'Important correction')
        self.assertFalse(receipts[0]['reconciled']);self.assertEqual(receipts[0]['state'],'incorporated')
        other=self.c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
        bad=self.c.call('_session/acknowledge_inputs',{'sessionId':other,'inputIds':['correction'],'disposition':'proceed_without_replay'});self.assertIn('error',bad)
        ack=self.c.call('_session/acknowledge_inputs',{'sessionId':self.sid,'inputIds':['correction'],'disposition':'proceed_without_replay'})
        self.assertTrue(ack['result']['inputReceipts'][0]['reconciled']);self.assertEqual(ack['result']['inputReceipts'][0]['state'],'incorporated')
        self.assertTrue(self.server.connection.empty())
        new=self.start();self.completed('r1');self.assertEqual(self.c.finish(new)['result']['stopReason'],'end_turn')
    def test_pipelined_inputs_follow_wire_order(self):
        pid=self.start();ids=[]
        for i in range(32):
            key=f'm{i:02d}'
            ids.append(self.c.request('_session/steering',{'sessionId':self.sid,'prompt':[{'type':'text','text':key}],'_meta':{'steering':{'inputId':key,'idleBehavior':'promptRequired'}}}))
        for id in ids:self.assertEqual(self.c.finish(id)['result']['outcome'],'injected')
        self.assertEqual([r[0] for r in self.receipts()],[f'm{i:02d}' for i in range(32)])
        self.c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':self.sid}});self.assertEqual(self.c.finish(pid)['result']['stopReason'],'cancelled')
    def test_idle_does_not_start_work(self):
        self.assertEqual(self.steer('late','a')['result']['outcome'],'promptRequired');self.assertTrue(self.server.connection.empty())
    def test_repeated_bursts_follow_successors_and_deduplicate(self):
        pid=self.start();self.assertEqual(self.steer('A','a')['result']['outcome'],'injected');first=self.frame();self.assertEqual(first['previous_response_id'],'r1')
        self.steer('B','b');self.steer('C','c');self.assertTrue(self.steer('A','a')['result']['_meta']['steering']['duplicate'])
        self.accepted('s1','r1');self.completed('r1',steered=True);self.created('r2','r1')
        second=self.frame();self.assertEqual(second['type'],'response.steer');self.assertEqual(second['previous_response_id'],'r2');self.assertEqual([m['content'] for m in second['input']],['B','C'])
        self.accepted('s2','r2');self.completed('r2',steered=True);self.created('r3','r2');self.completed('r3')
        result=self.c.finish(pid);self.assertEqual(result['result']['stopReason'],'end_turn');self.assertEqual(self.receipts(),[('a','incorporated',1),('b','incorporated',1),('c','incorporated',1)])
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:items=json.loads(db.execute('SELECT items FROM sessions').fetchone()[0])
        self.assertEqual([i['content'] for i in items if i.get('role')=='user'],['Do the original work','A','B','C'])
    def test_input_during_approval_does_not_cancel_or_repeat_effect(self):
        pid=self.start();call={'type':'function_call','call_id':'write-once','name':'workspace_write','arguments':json.dumps({'path':'proof.txt','content':'one effect'})};self.completed('r1',[call])
        while True:
            msg=self.c.q.get(timeout=10)
            if msg.get('method')=='session/request_permission':break
            self.c.updates.append(msg)
        self.steer('After writing, explain the result','followup');steer=self.frame();self.assertEqual(steer['type'],'response.steer');self.accepted('s1','r1')
        self.event('response.steer.pending',steer={'id':'s1','previous_response_id':'r1'},reason='waiting_for_required_input',required_input=[{'type':'function_call_output','call_id':'write-once'}])
        self.c.send({'jsonrpc':'2.0','id':msg['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once'}}})
        next=self.frame();self.assertEqual(next['type'],'response.create');self.assertEqual(next['previous_response_id'],'r1');self.assertEqual([i['type'] for i in next['input']],['function_call_output']);self.assertEqual(next['input'][0]['call_id'],'write-once')
        self.created('r2','r1');self.completed('r2');result=self.c.finish(pid)
        self.assertEqual(result['result']['stopReason'],'end_turn');self.assertEqual((self.work/'proof.txt').read_text(),'one effect');self.assertFalse(any(u.get('method')=='$/cancel_request' for u in self.c.updates))
    def test_refreshed_context_during_pending_steering_keeps_tool_result_chain(self):
        def snapshot(text):
            return {'type':'text','text':text,'_meta':{'com.microsoft.amplifier.context-snapshot':{'version':1,'key':'room','retainedText':'Earlier room observation'}}}
        pid=self.c.request('session/prompt',{'sessionId':self.sid,'prompt':[snapshot('Original room; do the requested work')]})
        self.wire=self.server.connection.get(timeout=10);self.frame();self.created('r1')
        self.c.call('_session/steering',{'sessionId':self.sid,'prompt':[snapshot('Updated room; keep doing the requested work')],'_meta':{'steering':{'inputId':'updated-room'}}})
        self.frame();self.accepted('s1','r1');self.completed('r1',steered=True);self.created('r2','r1')
        call={'type':'function_call','call_id':'write-once','name':'workspace_write','arguments':json.dumps({'path':'proof.txt','content':'one effect'})}
        self.completed('r2',[call])
        while True:
            msg=self.c.q.get(timeout=10)
            if msg.get('method')=='session/request_permission':break
            self.c.updates.append(msg)
        self.steer('Reaction awareness is additional evidence, not a new task','reaction')
        self.frame();self.accepted('s2','r2')
        self.event('response.steer.pending',steer={'id':'s2','previous_response_id':'r2'},reason='waiting_for_required_input',required_input=[{'type':'function_call_output','call_id':'write-once'}])
        self.c.send({'jsonrpc':'2.0','id':msg['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once'}}})
        try: continuation=self.server.frames.get(timeout=2)
        except queue.Empty: self.fail(f'No continuation: {self.c.finish(pid)}')
        self.assertEqual(continuation['type'],'response.create')
        self.assertEqual(continuation['previous_response_id'],'r2')
        self.assertEqual([i['type'] for i in continuation['input']],['function_call_output'])
        self.assertEqual(continuation['input'][0]['call_id'],'write-once')
        self.created('r3','r2');self.completed('r3')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        self.assertEqual((self.work/'proof.txt').read_text(),'one effect')
        self.assertEqual(self.receipts(),[('updated-room','incorporated',1),('reaction','incorporated',1)])
    def test_server_compaction_and_pending_steering_keep_exactly_one_tool_effect(self):
        pid=self.start()
        self.completed('r1',[{'type':'compaction','encrypted_content':'provider-private-window'}, {'type':'function_call','call_id':'write-once','name':'workspace_write','arguments':json.dumps({'path':'native-proof.txt','content':'one effect'})}])
        while True:
            msg=self.c.q.get(timeout=10)
            if msg.get('method')=='session/request_permission':break
            self.c.updates.append(msg)
        self.steer('Fresh public evidence','fresh-source');self.frame();self.accepted('s1','r1')
        self.event('response.steer.pending',steer={'id':'s1','previous_response_id':'r1'},reason='waiting_for_required_input',required_input=[{'type':'function_call_output','call_id':'write-once'}])
        self.c.send({'jsonrpc':'2.0','id':msg['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once'}}})
        continuation=self.frame();self.assertEqual(continuation['previous_response_id'],'r1')
        self.assertEqual([v['type'] for v in continuation['input']],['function_call_output'])
        self.created('r2','r1');self.completed('r2')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        self.assertEqual((self.work/'native-proof.txt').read_text(),'one effect')
        self.assertEqual(self.receipts(),[('fresh-source','incorporated',1)])
        self.assertNotIn('provider-private-window',json.dumps(self.c.updates))

    def test_compaction_during_pending_steering_preserves_the_same_tool_result_chain(self):
        self.c.close()
        cfg=json.loads((self.home/'runtime.json').read_text());cfg['limits']['max_context_bytes']=32000;cfg['context']['retain_recent_turns']=1;cfg['context']['provider_compaction']=False
        (self.home/'runtime.json').write_text(json.dumps(cfg))
        items=[]
        for i in range(20):
            items.extend([{'role':'user','content':f'Historical request {i}: '+('old evidence '*800)}, {'role':'assistant','content':f'Historical result {i}'}])
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            db.execute('UPDATE sessions SET items=? WHERE id=?',(json.dumps(items),self.sid));db.commit()
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        self.test_refreshed_context_during_pending_steering_keeps_tool_result_chain()
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            checkpoints=[json.loads(row[0]) for row in db.execute("SELECT data FROM events WHERE kind='context.compacted'")]
        self.assertGreaterEqual(len({v['prefix_items'] for v in checkpoints}),2)
    def test_input_during_explicit_create_waits_for_new_response_identity(self):
        pid=self.start()
        call={'type':'function_call','call_id':'write-once','name':'workspace_write','arguments':json.dumps({'path':'proof.txt','content':'one effect'})}
        self.completed('r1',[call])
        while True:
            msg=self.c.q.get(timeout=10)
            if msg.get('method')=='session/request_permission':break
            self.c.updates.append(msg)
        self.c.send({'jsonrpc':'2.0','id':msg['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once'}}})
        explicit=self.frame();self.assertEqual(explicit['type'],'response.create')
        self.assertEqual(explicit['previous_response_id'],'r1')
        self.assertEqual([i['type'] for i in explicit['input']],['function_call_output'])
        self.steer('Please incorporate the latest detail','new-detail')
        # This is an absence assertion across a real network boundary: the
        # create is on wire, but its response ID is deliberately unavailable.
        with self.assertRaises(queue.Empty):self.server.frames.get(timeout=0.1)
        self.created('r2','r1')
        update=self.frame();self.assertEqual(update['type'],'response.steer')
        self.assertEqual(update['previous_response_id'],'r2')
        self.accepted('s1','r2');self.completed('r2',steered=True)
        self.created('r3','r2');self.completed('r3')
        self.assertEqual(self.c.finish(pid)['result']['stopReason'],'end_turn')
        self.assertEqual(self.receipts(),[('new-detail','incorporated',1)])
        self.assertEqual((self.work/'proof.txt').read_text(),'one effect')
        self.assertTrue(self.server.frames.empty())
    def test_restart_preserves_unknown_input_without_replay(self):
        self.start();self.steer('retain this input','crash-input');self.frame();self.accepted('s1','r1')
        # Wait for the durable acceptance notification before simulating process death.
        while True:
            v=self.c.q.get(timeout=10)
            if v.get('params',{}).get('update',{}).get('_meta',{}).get('io.github.bkrabach.agent-runtime',{}).get('inputReceipt',{}).get('state')=='remoteQueued':break
        old=self.c;old.proc.kill();old.proc.wait(timeout=5)
        for pipe in [old.proc.stdin,old.proc.stdout,old.proc.stderr]:pipe.close()
        self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}})
        loaded=self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        receipt=loaded['result']['_meta']['io.github.bkrabach.agent-runtime']['inputReceipts'][0]
        self.assertEqual(receipt['state'],'unknown');self.assertEqual(receipt['steeringId'],'s1');self.assertEqual(receipt['responseId'],'r1')
        self.assertTrue(self.server.connection.empty());self.assertTrue(self.server.frames.empty())
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            self.assertIn('retain this input',db.execute('SELECT item FROM turn_inputs').fetchone()[0])
        self.c.call('session/delete',{'sessionId':self.sid})
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:self.assertEqual(db.execute('SELECT count(*) FROM turn_inputs').fetchone()[0],0)
    def test_rejection_is_visible_and_input_is_retained(self):
        pid=self.start();self.steer('change','x');self.frame();self.accepted('s1','r1');self.event('response.steer.failed',steer={'id':'s1','previous_response_id':'r1','input':'change'},error={'code':'steering_not_supported','message':'PRIVATE-error-body'})
        result=self.c.finish(pid);self.assertIn('error',result);self.assertNotIn('PRIVATE-error-body',json.dumps(result));self.assertEqual(self.receipts()[0][1],'notAccepted')
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
            failures=[json.loads(row[0]) for row in db.execute("SELECT data FROM events WHERE kind='model.failed'")]
        self.assertEqual(len(failures),1);self.assertEqual(failures[0]['stage'],'event');self.assertEqual(failures[0]['category'],'provider')
        self.assertNotIn('PRIVATE-error-body',json.dumps(failures));self.assertNotIn('change',json.dumps(failures))
    def test_lost_ack_is_unknown_without_replay(self):
        pid=self.start();self.steer('change','x');self.frame();self.wire.connection.shutdown(socket.SHUT_RDWR);self.wire.connection.close()
        result=self.c.finish(pid);self.assertIn('error',result);self.assertEqual(self.receipts()[0][1],'unknown');self.assertTrue(self.server.frames.empty())
    def test_final_model_call_does_not_gain_budget_from_input(self):
        # Budget is captured by a newly launched ACP process.
        self.c.close();cfg=json.loads((self.home/'runtime.json').read_text());cfg['limits']['max_model_calls']=1;(self.home/'runtime.json').write_text(json.dumps(cfg));self.c=protocol.Client(self.home);self.c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});self.c.call('session/load',{'sessionId':self.sid,'cwd':str(self.work),'mcpServers':[]})
        pid=self.start();self.steer('late','x');self.completed('r1');result=self.c.finish(pid)
        self.assertEqual(result['result']['stopReason'],'max_turn_requests');self.assertEqual(self.receipts()[0][1],'notAccepted');self.assertTrue(self.server.frames.empty())

if __name__=='__main__':unittest.main()
