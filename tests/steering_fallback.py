from contextlib import closing
"""New inputs are ordered between HTTP calls, after real tool results, without cancelling work."""
import json, socket, sqlite3, threading, unittest
import providers

class Tests(providers.Tests):
    def test_context_rejection_never_claims_model_incorporation(self):
        started=threading.Event();release=threading.Event();self.setup_kind('anthropic',stall=True,started=started,release=release)
        p=self.home/'runtime.json';cfg=json.loads(p.read_text());cfg['limits']['max_context_bytes']=24000;cfg['context']['auto_compact']=False;p.write_text(json.dumps(cfg))
        c=self.client();sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
        pid=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'Original work'}]});self.assertTrue(started.wait(5))
        c.call('_session/steering',{'sessionId':sid,'prompt':[{'type':'text','text':'x'*30000}],'_meta':{'steering':{'inputId':'oversized','idleBehavior':'promptRequired'}}})
        self.server.case['stall']=False;release.set();self.assertIn('error',c.finish(pid,approve=True));self.assertEqual(len(self.server.case['requests']),1)
        with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:self.assertEqual(db.execute("SELECT state FROM turn_inputs WHERE input_id='oversized'").fetchone()[0],'notAccepted')
        # A refused candidate stays in its receipt, not in the next request's
        # active context. It must neither poison the session nor become a retry.
        self.assertEqual(self.prompt(c,sid,'What is 2 plus 2?')['result']['stopReason'],'end_turn')
        self.assertEqual(len(self.server.case['requests']),2)
        self.assertNotIn('x'*30000,json.dumps(self.server.case['requests'][1][1]))
        receipt=c.call('_session/input_receipts',{'sessionId':sid})['result']['inputReceipts'][0]
        self.assertEqual(receipt['state'],'notAccepted');self.assertEqual(receipt['prompt'][0]['text'],'x'*30000)
    def test_unknown_input_acknowledgment_does_not_replay_candidate(self):
        original=providers.Handler.do_POST
        def drop_second(handler):
            case=handler.server.case
            if len(case['requests'])==1:
                body=json.loads(handler.rfile.read(int(handler.headers['Content-Length'])))
                case['requests'].append((dict(handler.headers),body,handler.path))
                handler.connection.shutdown(socket.SHUT_RDWR);handler.connection.close()
            else:
                original(handler)
        providers.Handler.do_POST=drop_second
        release=threading.Event()
        try:
            started=threading.Event();self.setup_kind('anthropic',stall=True,started=started,release=release)
            c=self.client();sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
            pid=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'Original work'}]})
            self.assertTrue(started.wait(5));marker='Uncertain correction: create another purchase order'
            c.call('_session/steering',{'sessionId':sid,'prompt':[{'type':'text','text':marker}],'_meta':{'steering':{'inputId':'uncertain','idleBehavior':'promptRequired'}}})
            self.server.case['stall']=False;release.set();self.assertIn('error',c.finish(pid,approve=True))
            self.assertEqual(len(self.server.case['requests']),2)
            self.assertIn(marker,json.dumps(self.server.case['requests'][1][1]))
            self.assertEqual(self.prompt(c,sid,'Unrelated new question')['result']['stopReason'],'end_turn')
            self.assertEqual(len(self.server.case['requests']),3)
            self.assertNotIn(marker,json.dumps(self.server.case['requests'][2][1]))
            self.assertIn('quarantined',self.server.case['requests'][2][1]['system'])
            self.assertIn('uncertain',self.server.case['requests'][2][1]['system'])
            ack=c.call('_session/acknowledge_inputs',{'sessionId':sid,'inputIds':['uncertain'],'disposition':'proceed_without_replay'})
            receipt=ack['result']['inputReceipts'][0]
            self.assertTrue(receipt['reconciled']);self.assertEqual(receipt['state'],'unknown')
            self.assertEqual(receipt['prompt'][0]['text'],marker)
            # Reconnect to prove the exclusion survives durable recovery.
            self.close(c);c=self.client();c.call('session/load',{'sessionId':sid,'cwd':str(self.work),'mcpServers':[]})
            self.assertEqual(self.prompt(c,sid,'What is 2 plus 2?')['result']['stopReason'],'end_turn')
            self.assertEqual(len(self.server.case['requests']),4)
            self.assertNotIn(marker,json.dumps(self.server.case['requests'][3][1]))
            self.assertEqual((self.work/'provider-proof.txt').read_text(encoding='utf-8'),providers.ARGS['content'])
            with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
                self.assertEqual(db.execute("SELECT state,recorded,reconciled FROM turn_inputs WHERE input_id='uncertain'").fetchone(),('unknown',0,1))
        finally:
            release.set();providers.Handler.do_POST=original
    # Reuse setup/cleanup, not the whole provider suite.
    def test_ordered_input_preserves_original_call_and_tool_effect(self):
        for kind in providers.KINDS[1:]:
            with self.subTest(kind=kind):
                started=threading.Event();release=threading.Event()
                self.setup_kind(kind,stall=True,started=started,release=release)
                c=self.client();sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
                pid=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'write original work'}]})
                self.assertTrue(started.wait(5))
                for key in ['first-update','second-update']:
                    response=c.call('_session/steering',{'sessionId':sid,'prompt':[{'type':'text','text':key}],'_meta':{'steering':{'inputId':key,'idleBehavior':'promptRequired'}}})
                    self.assertEqual(response['result']['outcome'],'injected')
                self.assertEqual(len(self.server.case['requests']),1)
                self.server.case['stall']=False;release.set()
                self.assertEqual(c.finish(pid,approve=True)['result']['stopReason'],'end_turn')
                requests=self.server.case['requests'];self.assertEqual(len(requests),2)
                self.assertEqual((self.work/'provider-proof.txt').read_text(encoding='utf-8'),providers.ARGS['content'])
                wire=json.dumps(requests[1][1]);self.assertLess(wire.index('first-update'),wire.index('second-update'))
                if kind=='anthropic':
                    blocks=requests[1][1]['messages'][2]['content'];self.assertEqual(blocks[0]['type'],'tool_result')
                    self.assertEqual([b['text'] for b in blocks[1:]],['first-update','second-update'])
                with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
                    rows=db.execute('SELECT state,recorded FROM turn_inputs WHERE session=? ORDER BY sequence',(sid,)).fetchall()
                    self.assertEqual(rows,[('incorporated',1),('incorporated',1)])
                    self.assertEqual(db.execute('SELECT count(*) FROM turns WHERE session=?',(sid,)).fetchone()[0],1)
                self.assertEqual(len(c.callbacks),1)
                self.close(c)

# The parent class is a fixture utility here; run only the new contract test.
if __name__=='__main__':
    unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite([Tests('test_ordered_input_preserves_original_call_and_tool_effect'),Tests('test_context_rejection_never_claims_model_incorporation'),Tests('test_unknown_input_acknowledgment_does_not_replay_candidate')])).wasSuccessful() or exit(1)
