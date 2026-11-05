"""Native computer protocol + real ACP and MCP, no model account or desktop."""
import base64,json,struct,tempfile,threading,unittest,zlib,subprocess
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from protocol import BINARY,Client

def png(width=64,height=64):
 def chunk(t,data):return struct.pack('>I',len(data))+t+data+struct.pack('>I',zlib.crc32(t+data)&0xffffffff)
 return base64.b64encode(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',width,height,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\xff\xff\xff'*width)*height))+chunk(b'IEND',b'')).decode()
from responses_ws import ResponsesWebSocketMixin

class Handler(ResponsesWebSocketMixin, BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_POST(self):
  r=json.loads(self.rfile.read(int(self.headers['Content-Length'])));self.server.requests.append(r)
  if self.path=='/mcp':
   method=r.get('method')
   if method=='notifications/initialized':self.send_response(202);self.end_headers();return
   if method=='initialize':result={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'computer-fixture','version':'1'}}
   elif method=='tools/list':result={'tools':[{'name':'computer','description':'Fixture screen','inputSchema':{'type':'object','properties':{}},'annotations':{'readOnlyHint':True}}]}
   elif method=='tools/call':
    self.server.effects+=1;result={'isError':self.server.fail,'content':[{'type':'image','mimeType':'image/png','data':png()}]}
   else:result={}
   self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}).encode());return
  # The environment must not also be exposed as an ordinary function alias.
  assert {'type':'computer'} in r['tools'],r
  assert not any(t.get('name','').endswith('_computer') for t in r['tools']),r
  history=r['input'];last=max(i for i,v in enumerate(history) if v.get('role')=='user');tail=history[last+1:]
  self.server.inputs.append(history)
  if any(v.get('type')=='computer_call_output' for v in tail) or 'after' in str(history[last]):
   output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Verified fixture screen.'}]}]
  else:output=[{'type':'computer_call','call_id':'screen-'+str(len(self.server.inputs)),'actions':[{'type':'click','button':'left','keys':None,'x':999 if self.server.invalid else 10,'y':10}],'pending_safety_checks':[{'id':'check-1','code':'test','message':'Review the local fixture action'}],'status':'completed'}]
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers();self.wfile.write(('data: '+json.dumps({'type':'response.completed','response':{'status':'completed','output':output}})+'\n\n').encode())
class Tests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'agent';self.work=Path(self.tmp.name)/'work';self.clients=[]
  self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);self.server.requests=[];self.server.inputs=[];self.server.effects=0;self.server.fail=False;self.server.invalid=False;threading.Thread(target=self.server.serve_forever,daemon=True).start();self.url=f'http://127.0.0.1:{self.server.server_port}'
  subprocess.run([str(BINARY),'--home',str(self.home),'init','--workspace',str(self.work),'--endpoint',self.url+'/responses','--key-env','RUNTIME_FIXTURE_KEY'],check=True,capture_output=True)
  cfg=json.loads((self.home/'runtime.json').read_text());cfg['mode']='workspace';cfg['provider']['native']={'computer':{'mcp_server':'browser','mcp_tool':'computer','display_width':64,'display_height':64}};(self.home/'runtime.json').write_text(json.dumps(cfg))
 def tearDown(self):
  for c in self.clients:c.close()
  self.server.shutdown();self.server.server_close();self.tmp.cleanup()
 def client(self,sid=None):
  c=Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});p={'cwd':str(self.work),'mcpServers':[{'name':'browser','type':'http','url':self.url+'/mcp','headers':[]}]}
  if sid:p['sessionId']=sid
  r=c.call('session/load' if sid else 'session/new',p);return c,r['result'].get('sessionId',sid)
 def prompt(self,c,sid,text='operate',approve=False):return c.call('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':text}]},approve=approve)
 def test_workspace_mode_requires_explicit_computer_approval_and_safety_ack(self):
  c,sid=self.client();r=self.prompt(c,sid,approve=True);self.assertIn('result',r);self.assertEqual(self.server.effects,1);self.assertEqual(len(c.callbacks),1);self.assertIn('pending_safety_checks',str(c.callbacks))
  result=next(v for v in self.server.inputs[-1] if v.get('type')=='computer_call_output');self.assertEqual(result['acknowledged_safety_checks'][0]['id'],'check-1');self.assertEqual(result['output']['type'],'computer_screenshot')
  c.close();self.clients.remove(c);c,_=self.client(sid)
  replay=[u['params']['update'] for u in c.updates if u.get('method')=='session/update']
  self.assertEqual(len([u for u in replay if u['sessionUpdate']=='tool_call']),1)
  results=[u for u in replay if u['sessionUpdate']=='tool_call_update'];self.assertEqual(len(results),1);self.assertEqual(results[0]['status'],'completed');self.assertEqual(results[0]['content'][0]['content']['data'],png());self.assertEqual(c.callbacks,[]);self.assertEqual(self.server.effects,1)
  self.prompt(c,sid,'after restart');self.assertEqual(self.server.effects,1)
 def test_denial_and_failed_screenshot_are_not_replayed(self):
  for authorized in [False,True]:
   self.server.fail=authorized;c,sid=self.client();before=self.server.effects;r=self.prompt(c,sid,approve=authorized);self.assertIn('result',r);self.assertEqual(self.server.effects-before,int(authorized))
   c.close();self.clients.remove(c);c,_=self.client(sid)
   replay=[u['params']['update'] for u in c.updates if u.get('method')=='session/update'];cards=[u for u in replay if u['sessionUpdate']=='tool_call'];self.assertEqual(len(cards),1);self.assertEqual(cards[0]['status'],'failed');self.assertEqual(c.callbacks,[]);self.assertEqual(self.server.effects-before,int(authorized))
   self.prompt(c,sid,'after stop',approve=True);self.assertEqual(self.server.effects-before,int(authorized));self.assertFalse(any(v.get('type')=='computer_call' for v in self.server.inputs[-1]))
 def test_bad_coordinate_never_reaches_environment(self):
  self.server.invalid=True;c,sid=self.client();self.assertIn('error',self.prompt(c,sid,approve=True));self.assertEqual(self.server.effects,0);self.assertEqual(c.callbacks,[]);self.prompt(c,sid,'after invalid call');self.assertEqual(self.server.effects,0)
 def test_read_only_mode_never_calls_computer_even_with_auto_approval_client(self):
  cfg=json.loads((self.home/'runtime.json').read_text());cfg['mode']='read_only';(self.home/'runtime.json').write_text(json.dumps(cfg));c,sid=self.client();self.prompt(c,sid,approve=True);self.assertEqual(self.server.effects,0);self.assertEqual(c.callbacks,[])
 def test_unconnected_environment_fails_before_model_call(self):
  c=Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});sid=c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId'];self.assertIn('error',self.prompt(c,sid));self.assertEqual(self.server.inputs,[])
if __name__=='__main__':unittest.main()
