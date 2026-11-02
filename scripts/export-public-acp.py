#!/usr/bin/env python3
"""Explicit owner export through standard ACP load replay, without model turns.

Input is a private JSON command record (command,args,cwd,env). Output contains
public text only, with omitted media described. Never reads provider databases.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


class Client:
    def __init__(self, launch):
        self.p = subprocess.Popen([launch['command'], *launch.get('args', [])], cwd=launch['cwd'],
                                  env=os.environ | launch.get('env', {}), stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.q = queue.Queue()
        self.seq = 0
        def read():
            for line in self.p.stdout:
                try: self.q.put(json.loads(line))
                except ValueError: pass
            self.q.put(None)
        threading.Thread(target=read, daemon=True).start()

    def send(self, message):
        self.p.stdin.write(json.dumps(message) + '\n')
        self.p.stdin.flush()

    def call(self, method, params):
        self.seq += 1
        number = self.seq
        self.send({'jsonrpc':'2.0','id':number,'method':method,'params':params})
        updates = []
        deadline = time.monotonic() + 120
        while True:
            value = self.q.get(timeout=max(.01, deadline-time.monotonic()))
            if value is None:
                raise RuntimeError('ACP process stopped')
            if value.get('id') == number and 'method' not in value:
                if 'error' in value:
                    raise RuntimeError('ACP '+method+' failed with code '+str(value['error'].get('code')))
                return value['result'], updates
            if 'method' in value and 'id' in value:
                # An export cannot approve an operation or read client files.
                self.send({'jsonrpc':'2.0','id':value['id'],'error':{'code':-32601,'message':'Public-history export has no client tools'}})
            elif value.get('method') == 'session/update':
                updates.append(value.get('params', {}).get('update', {}))
                if len(updates) > 100000:
                    raise RuntimeError('Replay exceeds export limit')

    def close(self):
        self.p.terminate()
        try: self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.p.kill(); self.p.wait()


def messages(updates):
    out = []
    prior = None
    for u in updates:
        kind = u.get('sessionUpdate')
        if kind not in ['user_message_chunk', 'agent_message_chunk']:
            prior = None
            continue
        role = 'user' if kind == 'user_message_chunk' else 'assistant'
        c = u.get('content', {})
        text = c.get('text') if c.get('type') == 'text' else '[Original '+str(c.get('type','media'))+' content retained in the archived source; not copied into this text migration.]'
        if not text: continue
        key = (role, u.get('messageId'))
        if out and key == prior:
            out[-1]['content'][0]['text'] += text
        else:
            out.append({'role':role,'content':[{'type':'text','text':text}]})
        prior = key
    clean = []
    for m in out:
        text = m['content'][0]['text']
        opening = '[Configured agent instance context]'
        closing = '[/Configured agent instance context]'
        if m['role']=='user' and text.startswith(opening):
            if closing not in text:
                raise ValueError('Incomplete injected context envelope; export cannot safely classify it')
            text = text.split(closing,1)[1].lstrip()
        if text:
            m['content'][0]['text'] = text
            clean.append(m)
    return clean


def main():
    os.umask(0o077)
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--launch',required=True,type=Path)
    p.add_argument('--output',required=True,type=Path)
    p.add_argument('--source',default='ACP public replay')
    a=p.parse_args()
    launch=json.loads(a.launch.read_text())
    c=Client(launch)
    try:
        init,_=c.call('initialize',{'protocolVersion':1,'clientInfo':{'name':'constellation-history-migration','version':'1'},'clientCapabilities':{}})
        if not init.get('agentCapabilities',{}).get('loadSession'):
            raise ValueError('Source does not offer standard ACP history replay')
        sessions=[]; cursor=None; seen=set()
        for _ in range(100):
            params={} if cursor is None else {'cursor':cursor}
            listed,_=c.call('session/list',params)
            for row in listed.get('sessions',[]):
                sid=row['sessionId']
                if sid not in seen: sessions.append(row);seen.add(sid)
            cursor=listed.get('nextCursor')
            if not cursor:break
        else: raise ValueError('Session listing did not finish')
        result=[]
        for row in sessions:
            cwd=row.get('cwd') or launch['cwd']
            _,updates=c.call('session/load',{'sessionId':row['sessionId'],'cwd':cwd,'mcpServers':[]})
            content=messages(updates)
            title=row.get('title') or next((m['content'][0]['text'][:100] for m in content if m['role']=='user'),'Saved conversation')
            if title.startswith('[Configured agent instance context]'):title='Saved conversation'
            result.append({'session_id':row['sessionId'],'cwd':cwd,'title':title[:1024],'messages':content})
        packet={'schema':'constellation.public-history/1','source':a.source,'sessions':result}
        with a.output.open('x') as out: json.dump(packet,out)
        print(json.dumps({'sessions':len(result),'public_messages':sum(len(s['messages']) for s in result),'output':str(a.output),'model_calls':0}))
    finally:c.close()


if __name__=='__main__':main()
