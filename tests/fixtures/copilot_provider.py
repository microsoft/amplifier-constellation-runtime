"""Protocol fixture only. Does not import the real SDK or contact a provider."""
import json,os,sys,time,subprocess
from pathlib import Path
request=json.load(sys.stdin)
assert set(os.environ).issubset({'HOME','PATH','LANG','LC_CTYPE','COPILOT_GITHUB_TOKEN','__CF_USER_TEXT_ENCODING'})
assert os.environ['COPILOT_GITHUB_TOKEN']=='gho_fixture'
assert Path.cwd().resolve()==Path(os.environ['HOME']).resolve()
messages=request.get('messages',[])
user=next(m for m in reversed(messages) if m.get('role')=='user')
text=user['content'] if isinstance(user['content'],str) else '\n'.join(p.get('text','') for p in user['content'])
if text.startswith('cancel:'):
    work=Path(text.split(':',1)[1])
    subprocess.Popen([sys.executable,'-c','import time,pathlib,sys; p=pathlib.Path(sys.argv[1]); (p/"started").write_text("yes");\nwhile not (p/"release").exists(): time.sleep(.02)\n(p/"escaped").write_text("escaped")',str(work)])
    time.sleep(60)
elif text=='write proof' and messages[-1].get('type')!='function_call_output':
    output=[{'type':'function_call','call_id':'write-call','name':'workspace_write','arguments':json.dumps({'path':'proof.txt','content':'verified'})}]
elif text=='unadvertised':
    output=[{'type':'function_call','call_id':'bad','name':'bash','arguments':'{}'}]
else:
    output=[{'type':'message','role':'assistant','content':[{'type':'output_text','text':'saved history includes write proof' if any('write proof' in json.dumps(m) for m in messages) else 'complete'}]}]
print(json.dumps({'protocol_version':1,'status':'completed','output':output,'usage':{},'sdk_tools_executed':False}))
