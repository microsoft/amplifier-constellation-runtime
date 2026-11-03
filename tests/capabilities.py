"""Typed decision and native media/tool contracts through real ACP processes."""
import base64,json,os,subprocess,tempfile,threading,time,unittest
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import protocol
PNG='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII='
QUESTIONS=[{'type':'predicate','name':'relevant','instructions':'Is this relevant?'},{'type':'choice','name':'route','instructions':'Who?','choices':[{'value':'lead','description':'coordination'},{'value':'builder','description':'implementation'}]},{'type':'score','name':'priority','instructions':'Priority?','levels':['low','medium','high']}]
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*_):pass
 def do_GET(self):
  self.server.case.setdefault('downloads',[]).append((self.path,dict(self.headers)))
  data=base64.b64decode(PNG);self.send_response(200);self.send_header('Content-Type','image/png');self.end_headers();self.wfile.write(data)
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers['Content-Length'])));case=self.server.case;case['requests'].append((self.path,dict(self.headers),body))
  if self.path=='/decide':
   case['decided'].set()
   if case.get('stall'):time.sleep(3)
   if case.get('status'):
    self.send_response(case['status']);self.end_headers();self.wfile.write(b'PRIVATE API KEY AND EVIDENCE');return
   answers=[{'type':'predicate','name':'relevant','probability':.9},{'type':'choice','name':'route','choice':'lead','confidence':.8,'probabilities':[{'value':'lead','probability':.8},{'value':'builder','probability':.2}]},{'type':'score','name':'priority','score':1.5,'confidence':.6,'probabilities':[{'value':0,'label':'low','probability':.1},{'value':1,'label':'medium','probability':.3},{'value':2,'label':'high','probability':.6}]}]
   if case['kind']!='openai':
    for a in answers:
     if a['type']=='predicate':a['type']='noul';a['noul']=a.pop('probability')
     else:a['probabilities']={str(p['value']):p['probability'] for p in a['probabilities']}
   if case.get('bad'):answers[1]['choice']='unknown'
   if case.get('missing'):answers.pop()
   if case.get('refusal'):answers[0]={'name':'relevant','type':'refusal'}
   payload={'model':'resolved-fixture','answers':answers if case['kind']=='openai' else {a['name']:a for a in answers},'usage':{'input_tokens':12}}
   self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers()
   try:self.wfile.write(json.dumps(payload).encode())
   except (BrokenPipeError,ConnectionResetError):pass
   return
  n=sum(1 for p,_,_ in case['requests'] if p!='/decide');kind=case.get('chat','openai');events=[]
  if case.get('vision_probe'):
   if kind in ['openai','azure-openai','vllm']:
    events=[{'type':'response.completed','response':{'status':'completed','output':[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'{"seen":true}'}]}]}}]
   elif kind=='anthropic':
    events=[{'type':'message_start','message':{'usage':{}}},{'type':'content_block_start','index':0,'content_block':{'type':'text','text':'{"seen":true}'}},{'type':'message_delta','delta':{'stop_reason':'end_turn'}},{'type':'message_stop'}]
   elif kind=='gemini':
    events=[{'candidates':[{'content':{'role':'model','parts':[{'text':'{"seen":true}'}]},'finishReason':'STOP'}]}]
   elif kind=='ollama':
    self.send_response(200);self.send_header('Content-Type','application/x-ndjson');self.end_headers();self.wfile.write((json.dumps({'message':{'role':'assistant','content':'{"seen":true}'},'done':True,'done_reason':'stop'})+'\n').encode());return
   else:events=[{'choices':[{'delta':{'content':'{"seen":true}'},'finish_reason':'stop'}]}]
  elif case.get('file_auto'):
   events=[{'type':'response.completed','response':{'status':'completed','output':[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Generated file','annotations':[{'type':'container_file_citation','file_id':'file-1','container_id':'container-1','filename':'image.png'}]}]}]}}]
  elif case.get('file_tool'):
   if n==1:
    output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'File ready','annotations':[{'type':'container_file_citation','file_id':'file-1','container_id':'container-1','filename':'image.png'}]}]},{'type':'function_call','call_id':'download','name':'provider_file','arguments':json.dumps({'file_id':'file-other' if case.get('foreign_file') else 'file-1','container_id':'container-1'})}]
   else:output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Done'}]}]
   events=[{'type':'response.completed','response':{'status':'completed','output':output}}]
  elif case.get('decision_tool'):
   output=[{'type':'function_call','call_id':'dec','name':'decision_evaluate','arguments':json.dumps({'backend':'routing','state':{'message':'test'},'questions':QUESTIONS})}] if n==1 else [{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Decided.'}]}]
   events=[{'type':'response.completed','response':{'status':'completed','output':output}}]
  elif kind in ['openai','azure-openai']:
   if n==1:
    output=[{'type':'image_generation_call','id':'ig-fixture','status':'completed','result':PNG,'output_format':'png'}, {'type':'web_search_call','id':'web-fixture','status':'completed','action':{'type':'search','query':'test'}},{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Here is the image.','annotations':[{'type':'url_citation','url':'https://example.com/source','title':'Source'}]}]}]
    events=[{'type':'response.output_text.delta','delta':'Here is the image.'}]
   elif n==2:
    assert body['input'][1]['content'][1]['type']=='input_image'
    output=[{'type':'function_call','call_id':'save-art','name':'workspace_save_artifact','arguments':'{"artifact":"latest","path":"image.png"}'}]
   else:output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'Saved.'}]}];events=[{'type':'response.output_text.delta','delta':'Saved.'}]
   events.append({'type':'response.completed','response':{'status':'completed','output':output}})
  elif kind=='anthropic':
   blocks=[{'type':'thinking','thinking':'PRIVATE THOUGHT','signature':'signed'},{'type':'server_tool_use','id':'srv-1','name':'web_search','input':{'query':'test'}}] if n==1 else [{'type':'web_search_tool_result','tool_use_id':'srv-1','content':[{'type':'web_search_result','url':'https://example.com','title':'Source','encrypted_content':'PRIVATE DATA'}]},{'type':'text','text':'Found it.','citations':[{'type':'web_search_result_location','url':'https://example.com','title':'Source'}]}]
   events=[{'type':'message_start','message':{'container':{'id':'container-fixture'},'usage':{'input_tokens':10}}}]+[{'type':'content_block_start','index':i,'content_block':b} for i,b in enumerate(blocks)]+[{'type':'message_delta','delta':{'stop_reason':'pause_turn' if n==1 else 'end_turn'}},{'type':'message_stop'}]
  elif kind=='gemini':
   parts=[{'thought':True,'text':'PRIVATE THOUGHT'},{'inlineData':{'mimeType':'image/png','data':PNG},'thoughtSignature':'opaque'},{'text':'Created.'},{'executableCode':{'language':'PYTHON','code':'print(2+2)'}},{'codeExecutionResult':{'outcome':'OUTCOME_OK','output':'4'}}]
   events=[{'candidates':[{'content':{'role':'model','parts':parts},'groundingMetadata':{'groundingChunks':[{'web':{'uri':'https://example.com','title':'Source'}}]},'finishReason':'STOP'}]}]
  else:
   events=[{'choices':[{'delta':{'content':'Seen.'},'finish_reason':'stop'}]}]
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
  raw=''.join('data: '+json.dumps(e)+'\n\n' for e in events)
  if kind in ['litellm','chat-completions']:raw+='data: [DONE]\n\n'
  try:
   for i in range(0,len(raw),19):self.wfile.write(raw[i:i+19].encode())
  except (BrokenPipeError,ConnectionResetError):pass
class Tests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.home=Path(self.tmp.name)/'state';self.work=Path(self.tmp.name)/'work';self.clients=[]
  self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler);self.server.case={'kind':'openai','requests':[],'decided':threading.Event()};threading.Thread(target=self.server.serve_forever,daemon=True).start()
  self.url=f'http://127.0.0.1:{self.server.server_port}'
  subprocess.run([str(protocol.BINARY),'--home',str(self.home),'init','--workspace',str(self.work)],check=True,stdout=subprocess.DEVNULL)
  self.cfg=json.loads((self.home/'runtime.json').read_text());self.cfg['provider'].update(endpoint=self.url+'/responses',reasoning_effort=None,api_key_env='RUNTIME_FIXTURE_KEY')
  self.req=Path(self.tmp.name)/'request.json';self.req.write_text(json.dumps({'state':{'message':'test'},'questions':QUESTIONS}))
 def tearDown(self):
  for c in self.clients:c.close()
  self.server.shutdown();self.server.server_close();self.tmp.cleanup()
 def save(self): (self.home/'runtime.json').write_text(json.dumps(self.cfg))
 def backend(self,kind):
  self.server.case['kind']=kind
  self.cfg['decisions']=[{'name':'routing','provider':kind,'endpoint':self.url+'/decide','model':'fixture','api_key_env':'RUNTIME_FIXTURE_KEY','expose_to_agent':True}];self.save()
 def cli(self,*args):return subprocess.run([str(protocol.BINARY),'--home',str(self.home),*args],env={**os.environ,'RUNTIME_FIXTURE_KEY':'fixture'},capture_output=True,text=True,encoding='utf-8',timeout=15)
 def client(self):
  self.save();c=protocol.Client(self.home);self.clients.append(c);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});return c
 def new(self,c):return c.call('session/new',{'cwd':str(self.work),'mcpServers':[]})['result']['sessionId']
 def prompt(self,c,s,text='Create an image',**kw):return c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':text}]},**kw)
 def test_all_decision_wires_and_typed_normalization(self):
  for kind in ['openai','jev','microsoft-decision','h2o-lightning','local-decisions']:
   with self.subTest(kind=kind):
    self.backend(kind);r=self.cli('decide','routing',str(self.req));self.assertEqual(r.returncode,0,r.stderr);out=json.loads(r.stdout);self.assertEqual(out['answers'][2]['score'],1.5)
    _,h,b=self.server.case['requests'][-1];self.assertEqual(h.get('api-key' if kind=='microsoft-decision' else 'authorization') or h.get('Authorization'),'fixture' if kind=='microsoft-decision' else 'Bearer fixture')
    if kind=='openai':self.assertEqual(b['input'][0]['content'][0]['type'],'input_json');self.assertEqual(b['questions'][2]['levels'][2]['label'],'high')
    else:self.assertEqual(b['questions']['relevant']['type'],'noul');self.assertEqual(b['questions']['route']['criteria']['lead'],'coordination')
 def test_bad_decisions_fail_without_retry_or_leaking_error_body(self):
  self.backend('openai')
  for bad in ['bad','missing','status']:
   self.server.case[bad]=429 if bad=='status' else True;before=len(self.server.case['requests']);r=self.cli('decide','routing',str(self.req));self.assertNotEqual(r.returncode,0);self.assertEqual(len(self.server.case['requests']),before+1);self.assertNotIn('PRIVATE API',r.stderr);self.server.case.pop(bad)
  self.server.case['refusal']=True;r=self.cli('decide','routing',str(self.req));self.assertEqual(json.loads(r.stdout)['answers'][0]['type'],'refusal')
 def test_decision_tool_is_explicit_and_cancelable(self):
  self.backend('jev');self.server.case.update(decision_tool=True,stall=True);c=self.client();s=self.new(c);i=c.request('session/prompt',{'sessionId':s,'prompt':[{'type':'text','text':'classify'}]});self.assertTrue(self.server.case['decided'].wait(5));start=time.time();c.send({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':s}});self.assertEqual(c.finish(i)['result']['stopReason'],'cancelled');self.assertLess(time.time()-start,1)
 def test_native_image_output_survives_restart_and_saves_with_permission(self):
  for kind in ['openai','azure-openai']:
   with self.subTest(kind=kind):
    self.server.case.update(chat=kind,requests=[]);self.cfg['provider'].update(kind=kind,native={'image_input':True,'tools':[{'type':'image_generation'},{'type':'web_search'}]});c=self.client();s=self.new(c);self.assertIn('result',self.prompt(c,s));self.assertTrue(any(u['params']['update'].get('content',{}).get('data')==PNG for u in c.updates));self.assertIn('https://example.com/source',json.dumps(c.updates))
    c.close();self.clients.remove(c);c=self.client();self.assertIn('result',c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]}));self.assertTrue(any(u['params']['update'].get('content',{}).get('data')==PNG for u in c.updates))
    self.assertIn('result',self.prompt(c,s,'Save',approve=True));self.assertEqual((self.work/'image.png').read_bytes(),base64.b64decode(PNG));self.assertEqual(len(c.callbacks),1)
 def test_anthropic_hosted_tools_pause_continue_and_citations(self):
  self.server.case['chat']='anthropic';self.cfg['provider'].update(kind='anthropic',native={'tools':[{'type':'web_search_20250305','name':'web_search','max_uses':1}]});c=self.client();s=self.new(c);self.assertIn('result',self.prompt(c,s,'search'));self.assertEqual(len(self.server.case['requests']),2)
  b=self.server.case['requests'][1][2];self.assertEqual(b['container'],'container-fixture');self.assertEqual(b['messages'][1]['content'][0]['signature'],'signed');self.assertNotIn('PRIVATE THOUGHT',json.dumps(c.updates));self.assertNotIn('PRIVATE DATA',json.dumps(c.updates));self.assertIn('https://example.com',json.dumps(c.updates))
 def test_gemini_media_signatures_grounding_and_code(self):
  self.server.case['chat']='gemini';self.cfg['provider'].update(kind='gemini',native={'image_input':True,'disable_function_tools':True,'response_modalities':['TEXT','IMAGE'],'tools':[{'codeExecution':{}}]});c=self.client();s=self.new(c)
  self.assertIn('result',c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'image','mimeType':'image/png','data':PNG},{'type':'text','text':'Edit this'}]}));self.assertEqual(self.server.case['requests'][0][2]['contents'][0]['parts'][0]['inlineData']['data'],PNG);self.assertNotIn('functionDeclarations',str(self.server.case['requests'][0][2]['tools']));self.assertIn('https://example.com',str(c.updates));self.assertNotIn('PRIVATE THOUGHT',str(c.updates));self.assertIn('result',self.prompt(c,s,'Again'));self.assertEqual(self.server.case['requests'][1][2]['contents'][1]['parts'][1]['thoughtSignature'],'opaque')
 def test_vision_and_structured_output_use_each_native_wire_format(self):
  schema={'type':'object','properties':{'seen':{'type':'boolean'}},'required':['seen'],'additionalProperties':False}
  for kind in ['openai','azure-openai','anthropic','gemini','ollama','vllm','chat-completions','litellm']:
   with self.subTest(kind=kind):
    self.server.case.update(chat=kind,vision_probe=True,requests=[]);self.cfg['provider'].update(kind=kind,native={'image_input':True,'json_schema':schema});c=self.client();s=self.new(c)
    self.assertIn('result',c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'image','mimeType':'image/png','data':PNG},{'type':'text','text':'Look'}]}));body=self.server.case['requests'][0][2]
    if kind in ['openai','azure-openai','vllm']:
     self.assertEqual(body['input'][-1]['content'][0]['image_url'],'data:image/png;base64,'+PNG);self.assertEqual(body['text']['format']['schema'],schema)
    elif kind=='anthropic':
     self.assertEqual(body['messages'][-1]['content'][0]['source']['data'],PNG);self.assertEqual(body['output_config']['format']['schema'],schema)
    elif kind=='gemini':
     self.assertEqual(body['contents'][-1]['parts'][0]['inlineData']['data'],PNG);self.assertEqual(body['generationConfig']['responseJsonSchema'],schema)
    elif kind=='ollama':
     self.assertEqual(body['messages'][-1]['images'],[PNG]);self.assertEqual(body['format'],schema)
    else:
     self.assertEqual(body['messages'][-1]['content'][0]['image_url']['url'],'data:image/png;base64,'+PNG);self.assertEqual(body['response_format']['json_schema']['schema'],schema)
    c.close();self.clients.remove(c)
 def test_pdf_native_inputs_and_replay_across_provider_protocols(self):
  pdf=base64.b64encode(b'%PDF-1.7\nfixture document\n%%EOF').decode()
  block={'type':'resource','resource':{'uri':'attachment://fixture/brief.pdf','mimeType':'application/pdf','blob':pdf}}
  for kind in ['openai','azure-openai','anthropic','gemini','chat-completions','litellm']:
   with self.subTest(kind=kind):
    self.server.case.update(chat=kind,vision_probe=True,requests=[]);self.cfg['provider'].update(kind=kind,native={'pdf_input':True});c=self.client();s=self.new(c)
    self.assertIn('result',c.call('session/prompt',{'sessionId':s,'prompt':[block,{'type':'text','text':'Summarize this file'}]}))
    body=self.server.case['requests'][0][2]
    if kind in ['openai','azure-openai']:
     part=body['input'][-1]['content'][0];self.assertEqual(part['type'],'input_file');self.assertEqual(part['filename'],'brief.pdf');self.assertEqual(part['file_data'],'data:application/pdf;base64,'+pdf)
    elif kind=='anthropic':
     part=body['messages'][-1]['content'][0];self.assertEqual(part['type'],'document');self.assertEqual(part['source'],{'type':'base64','media_type':'application/pdf','data':pdf})
    elif kind=='gemini':self.assertEqual(body['contents'][-1]['parts'][0]['inlineData'],{'mimeType':'application/pdf','data':pdf})
    else:self.assertEqual(body['messages'][-1]['content'][0],{'type':'file','file':{'filename':'brief.pdf','file_data':'data:application/pdf;base64,'+pdf}})
    c.close();self.clients.remove(c);c=self.client();self.assertIn('result',c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]}));self.assertTrue(any(u['params'].get('update',{}).get('content')==block for u in c.updates))
    self.assertIn('result',self.prompt(c,s,'What was in that document?'));self.assertIn(pdf,json.dumps(self.server.case['requests'][-1][2]));c.close();self.clients.remove(c)
 def test_pdf_configuration_validation_and_no_network_for_disabled_or_bad_files(self):
  block={'type':'resource','resource':{'uri':'attachment://fixture/brief.pdf','mimeType':'application/pdf','blob':base64.b64encode(b'%PDF-1.7\nfixture').decode()}}
  c=self.client();s=self.new(c);self.assertIn('error',c.call('session/prompt',{'sessionId':s,'prompt':[block]}));self.assertEqual(self.server.case['requests'],[])
  self.cfg['provider']['native']={'pdf_input':True};c=self.client();s=self.new(c);block['resource']['blob']=base64.b64encode(b'not a PDF').decode();self.assertIn('error',c.call('session/prompt',{'sessionId':s,'prompt':[block]}));self.assertEqual(self.server.case['requests'],[])
  for kind in ['mock','ollama','vllm','github-copilot']:
   self.cfg['provider']['kind']=kind;self.save();self.assertNotEqual(self.cli('check').returncode,0)
 def test_disabled_media_and_unsupported_tools_fail_before_network(self):
  c=self.client();s=self.new(c);r=c.call('session/prompt',{'sessionId':s,'prompt':[{'type':'image','mimeType':'image/png','data':PNG}]});self.assertIn('error',r);self.assertEqual(self.server.case['requests'],[])
  self.cfg['provider']['native']={'tools':[{'type':'computer_use_preview'}]};self.save();self.assertNotEqual(self.cli('check').returncode,0)
 def test_provider_file_is_session_scoped_and_returned_as_acp_media(self):
  self.cfg['provider']['native']={'tools':[{'type':'code_interpreter','container':{'type':'auto'}}]};self.server.case['file_tool']=True;c=self.client();s=self.new(c);self.assertIn('result',self.prompt(c,s,'Make file'));self.assertEqual(self.server.case['downloads'][0][0],'/containers/container-1/files/file-1/content');self.assertIn(PNG,str(c.updates))
  self.server.case.update(foreign_file=True,requests=[],downloads=[]);s=self.new(c);self.assertIn('result',self.prompt(c,s,'Make file'));self.assertEqual(self.server.case['downloads'],[]);self.assertIn('not returned',str(c.updates))
 def test_final_file_citation_downloads_without_another_model_turn_and_replays(self):
  self.cfg['provider']['native']={'tools':[{'type':'code_interpreter','container':{'type':'auto'}}]};self.server.case['file_auto']=True;c=self.client();s=self.new(c);self.assertIn('result',self.prompt(c,s,'Make file'));self.assertEqual(len(self.server.case['requests']),1);self.assertEqual(self.server.case['downloads'][0][0],'/containers/container-1/files/file-1/content');self.assertIn(PNG,str(c.updates))
  c.close();self.clients.remove(c);c=self.client();self.assertIn('result',c.call('session/load',{'sessionId':s,'cwd':str(self.work),'mcpServers':[]}));self.assertIn(PNG,str(c.updates));self.assertEqual(len(self.server.case['downloads']),1)
 def test_duplicate_questions_fail_before_network(self):
  self.backend('jev');self.req.write_text(json.dumps({'state':'test','questions':[QUESTIONS[0],QUESTIONS[0]]}));self.assertNotEqual(self.cli('decide','routing',str(self.req)).returncode,0);self.assertEqual(self.server.case['requests'],[])
if __name__=='__main__':unittest.main(verbosity=2)
