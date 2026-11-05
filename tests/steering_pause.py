from contextlib import closing
"""Keep Anthropic server-tool continuation intact before adding queued input."""
import json
import sqlite3
import threading
import unittest
import capabilities

class Tests(capabilities.Tests):
 def test_new_input_waits_until_server_tool_pause_finishes(self):
  entered=threading.Event();release=threading.Event();original=capabilities.Handler.do_POST
  def delayed(handler):
   if not handler.server.case['requests']:
    entered.set();release.wait(10)
   return original(handler)
  capabilities.Handler.do_POST=delayed
  try:
   self.server.case['chat']='anthropic'
   self.cfg['provider'].update(kind='anthropic',model='claude-sonnet-5-5',native={'tools':[{'type':'web_search_20250305','name':'web_search','max_uses':1}]})
   c=self.client();sid=self.new(c)
   pid=c.request('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'Look up the original question'}]})
   self.assertTrue(entered.wait(5))
   r=c.call('_session/steering',{'sessionId':sid,'prompt':[{'type':'text','text':'Then explain the new question'}],'_meta':{'steering':{'inputId':'after-pause','idleBehavior':'promptRequired'}}})
   self.assertEqual(r['result']['outcome'],'injected');release.set()
   self.assertEqual(c.finish(pid)['result']['stopReason'],'end_turn')
   bodies=[r[2] for r in self.server.case['requests']];self.assertEqual(len(bodies),3)
   self.assertNotIn('Then explain the new question',json.dumps(bodies[1]))
   self.assertEqual(bodies[1]['messages'][-1]['role'],'assistant')
   self.assertEqual(bodies[1]['messages'][-1]['content'][0]['signature'],'signed')
   self.assertIn('Then explain the new question',json.dumps(bodies[2]))
   self.assertIn('web_search_tool_result',json.dumps(bodies[2]))
   with closing(sqlite3.connect(self.home/'history.sqlite3')) as db:
    self.assertEqual(db.execute('SELECT state,recorded FROM turn_inputs').fetchall(),[('incorporated',1)])
    self.assertEqual(db.execute('SELECT COUNT(*) FROM turns').fetchone()[0],1)
  finally:
   release.set();capabilities.Handler.do_POST=original

if __name__=='__main__':
 unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite([Tests('test_new_input_waits_until_server_tool_pause_finishes')])).wasSuccessful() or exit(1)
