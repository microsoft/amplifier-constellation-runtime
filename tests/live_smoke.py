"""Opt-in paid model smoke check. Uses a new private home; never runs in CI."""
import argparse, importlib.util, json, os, queue, secrets, subprocess, sys, time
from pathlib import Path

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',required=True,type=Path)
p.add_argument('--home',required=True,type=Path)
p.add_argument('--model',default='gpt-6.1-sol')
p.add_argument('--provider',default='openai')
p.add_argument('--key-env')
p.add_argument('--endpoint')
p.add_argument('--copilot-python',type=Path)
p.add_argument('--copilot-bridge',type=Path)
p.add_argument('--auth',choices=['auto','bearer','api-key','none'],default='auto')
p.add_argument('--thinking-budget-tokens',type=int)
p.add_argument('--reasoning-effort')
p.add_argument('--api-key-stdin',action='store_true')
a=p.parse_args()
assert a.home.is_absolute() and not a.home.exists(), 'Choose a new absolute private folder'
presets=json.loads(subprocess.check_output([str(a.binary),'providers'],text=True))
key_env=a.key_env or next(x['key_env'] for x in presets if x['provider']==a.provider)
if a.api_key_stdin:os.environ[key_env]=sys.stdin.readline().strip()
assert a.auth=='none' or os.environ.get(key_env), 'Supply a model key privately'
spec=importlib.util.spec_from_file_location('protocol',Path(__file__).with_name('protocol.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);m.BINARY=a.binary

class LiveClient(m.Client):
    def finish(self,id,approve=False):
        # The runtime's 120s turn budget is still authoritative. This total
        # deadline leaves room for transport/cleanup, including quiet models.
        deadline=time.monotonic()+150
        while True:
            remaining=deadline-time.monotonic()
            if remaining<=0:raise TimeoutError('Live ACP request exceeded its 150s deadline')
            try:v=self.q.get(timeout=remaining)
            except queue.Empty:raise TimeoutError('Live ACP request exceeded its 150s deadline') from None
            if v.get('method')=='session/request_permission':
                self.callbacks.append(v)
                self.send({'jsonrpc':'2.0','id':v['id'],'result':{'outcome':{'outcome':'selected','optionId':'allow_once' if approve else 'reject_once'}}})
            elif 'method' in v:self.updates.append(v)
            elif v.get('id')==id:return v
            else:raise AssertionError(v)

home=a.home/'state';work=a.home/'workspace'
init=[str(a.binary),'--home',str(home),'init','--workspace',str(work),'--name','Native provider check','--provider',a.provider,'--model',a.model,'--key-env',key_env,'--auth',a.auth]
if a.copilot_python:init.extend(['--copilot-python',str(a.copilot_python),'--copilot-bridge',str(a.copilot_bridge)])
if a.endpoint:init.extend(['--endpoint',a.endpoint])
if a.thinking_budget_tokens is not None:init.extend(['--thinking-budget-tokens',str(a.thinking_budget_tokens)])
if a.reasoning_effort:init.extend(['--reasoning-effort',a.reasoning_effort])
subprocess.run(init,check=True,stdout=subprocess.DEVNULL)
cfg=json.loads((home/'runtime.json').read_text());cfg['mode']='workspace';cfg['limits']['turn_seconds']=120
(home/'runtime.json').write_text(json.dumps(cfg))
marker='platform-'+secrets.token_hex(8)
def client():
    c=LiveClient(home);c.call('initialize',{'protocolVersion':1,'clientCapabilities':{}});return c
c=client()
try:
    sid=c.call('session/new',{'cwd':str(work),'mcpServers':[]})['result']['sessionId']
    r=c.call('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':f'Use the workspace write tool to create platform-proof.txt containing exactly {marker} and one trailing newline. Read it back with your read tool. No other tools or work. Briefly report what you actually verified.'}]})
    assert r.get('result',{}).get('stopReason')=='end_turn',r
    assert (work/'platform-proof.txt').read_text()==marker+'\n'
    tools=[x['params']['update'].get('title') for x in c.updates if x.get('method')=='session/update' and x['params']['update'].get('sessionUpdate')=='tool_call']
finally:c.close()
c=client()
try:
    c.call('session/load',{'sessionId':sid,'cwd':str(work),'mcpServers':[]});c.updates=[]
    r=c.call('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':'Without tools, what exact platform marker did you write in the previous turn? Reply with just the marker.'}]})
    assert r.get('result',{}).get('stopReason')=='end_turn',r
    reply=''.join(x['params']['update'].get('content',{}).get('text','') for x in c.updates if x.get('method')=='session/update' and x['params']['update'].get('sessionUpdate')=='agent_message_chunk')
    assert marker in reply
finally:c.close()
result={'success':True,'provider':a.provider,'model':a.model,'platform':sys.platform,'session_id':sid,'file_bytes_verified':True,'process_restart_recall':True,'tools':tools}
(a.home/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
