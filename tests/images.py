"""Real ACP/MCP roundtrip: observe a tool image, explicitly attach it, then reload."""
import base64,json,subprocess,tempfile,threading,unittest
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from protocol import Client,BINARY
PNG=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jC2cAAAAASUVORK5CYII=')
IMAGE={'type':'image','mimeType':'image/png','data':base64.b64encode(PNG).decode()}
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_POST(self):
  b=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
  if self.path=='/mcp':
   m=b.get('method');self.server.mcp_methods.append(m)
   if m=='notifications/initialized':self.send_response(202);self.end_headers();return
   r={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'camera','version':'1'}} if m=='initialize' else {'tools':[{'name':'picture','description':'Observe an image','inputSchema':{'type':'object','properties':{}},'annotations':{'readOnlyHint':True}}]} if m=='tools/list' else {'content':[{'type':'text','text':'Image saved at view.png'},IMAGE],'_acp_content':[IMAGE]}
   self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps({'jsonrpc':'2.0','id':b['id'],'result':r}).encode());return
  self.server.requests.append(b);done=[x for x in b['input'] if x.get('type')=='function_call_output']
  if not done:out=[{'type':'function_call','call_id':'capture','name':next(t['name'] for t in b['tools'] if t['name'].startswith('mcp_')),'arguments':'{}'}]
  elif len(done)==1:out=[{'type':'function_call','call_id':'attach','name':'workspace_attach_image','arguments':json.dumps({'path':'view.png'})}]
  else:out=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Finished.'}]}]
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers();self.wfile.write(('data: '+json.dumps({'type':'response.completed','response':{'status':'completed','output':out,'usage':{'input_tokens':1,'output_tokens':1}}})+'\n\n').encode())
class Tests(unittest.TestCase):
 def test_observation_attachment_permissions_and_restart(self):
  for mode in ['workspace','read_only']:
   with self.subTest(mode=mode),tempfile.TemporaryDirectory() as d:
    srv=ThreadingHTTPServer(('127.0.0.1',0),Handler);srv.requests=[];srv.mcp_methods=[];threading.Thread(target=srv.serve_forever,daemon=True).start()
    home=Path(d)/'state';work=Path(d)/'work';subprocess.run([str(BINARY),'--home',str(home),'init','--workspace',str(work),'--model','fixture'],check=True,stdout=subprocess.DEVNULL)
    (work/'view.png').write_bytes(PNG);url=f'http://127.0.0.1:{srv.server_port}'
    config=json.loads((home/'runtime.json').read_text());config['mode']=mode;config['trusted_mcp_readonly_urls']=[url+'/mcp'];config['provider'].update(endpoint=url+'/responses',api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None);(home/'runtime.json').write_text(json.dumps(config))
    mcp=[{'type':'http','name':'camera','url':url+'/mcp','headers':[]}]
    c=Client(home)
    try:
     c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});sid=c.call('session/new',{'cwd':str(work),'mcpServers':mcp})['result']['sessionId']
     result=c.call('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'Show this saved view.'}]});self.assertNotIn('error',result)
     updates=[u['params']['update'] for u in c.updates];sent=[u for u in updates if u.get('sessionUpdate')=='agent_message_chunk' and u.get('content',{}).get('type')=='image']
     self.assertEqual(len(sent),1 if mode=='workspace' else 0)
     if sent:self.assertEqual(base64.b64decode(sent[0]['content']['data']),PNG)
     second=srv.requests[1]['input'];self.assertTrue(any(p.get('type')=='input_image' for v in second for p in v.get('content',[]) if isinstance(p,dict)))
     self.assertNotIn(IMAGE['data'],next(v['output'] for v in second if v.get('type')=='function_call_output'))
     self.assertEqual(srv.mcp_methods.count('tools/call'),1);count=len(srv.requests)
    finally:c.close()
    c=Client(home)
    try:
     c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});c.call('session/load',{'sessionId':sid,'cwd':str(work),'mcpServers':mcp})
     updates=[u['params']['update'] for u in c.updates];self.assertFalse(any(u.get('sessionUpdate')=='agent_message_chunk' and u.get('content',{}).get('type')=='image' for u in updates));self.assertTrue(any(u.get('sessionUpdate')=='tool_call_update' and any(v.get('content',{}).get('type')=='image' for v in u.get('content',[])) for u in updates));self.assertEqual(count,len(srv.requests));self.assertEqual(srv.mcp_methods.count('tools/call'),1)
    finally:c.close();srv.shutdown();srv.server_close()
if __name__=='__main__':unittest.main()
