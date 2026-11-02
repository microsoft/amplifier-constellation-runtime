#!/usr/bin/env python3
"""Install a NEW Agent Instance with its own native runtime, key and history.

Existing installations are never switched or migrated by this command.
The host and runtime remain separate repositories and independently pinned files.
"""
import argparse, hashlib, importlib.util, json, os, platform, shutil, subprocess, sys, tempfile
from pathlib import Path

def run(*args):
    return subprocess.run([str(a) for a in args],check=True,capture_output=True,text=True)

def write(path,value):
    path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    temporary=path.with_suffix('.next')
    with temporary.open('x') as f:f.write(value)
    temporary.chmod(0o600);temporary.replace(path)

def platform_host_install(a):
    system=platform.system()
    if system=='Linux':
        run('bash',a.host_repo/'install.sh','--home',a.home,'--name',a.name,'--binary',a.host_binary,'--backend','none','--no-start','--no-browser','--skip-prerequisites','--skip-pam')
        return None
    if system not in ('Darwin','Windows'):
        raise ValueError('Supported installation platforms are Linux, macOS and Windows')
    source=a.host_repo/'scripts/native-install.py'
    run(sys.executable,source,'install','--home',a.home,'--name',a.name,'--binary',a.host_binary,'--skip-runtime','--no-service','--no-browser')
    spec=importlib.util.spec_from_file_location('agent_instance_native_install',source)
    native=importlib.util.module_from_spec(spec);spec.loader.exec_module(native)
    return native

def platform_start(a,config,native):
    if native is None:
        service={'kind':'systemd-user','name':'agent-instance-'+config['instance_id']+'.service'}
        if not a.no_start:run('systemctl','--user','start',service['name'])
        return service
    # Registration happens only after the engine and settings are complete. The
    # native host installer enforces private owner ACLs and native account auth.
    metadata=json.loads((a.home/'install.json').read_text())
    service=native.make_service(a.home,config)
    metadata['service']=service
    native.save(a.home/'install.json',metadata)
    if not a.no_start:
        native.service_command(service,'start')
        native.health(a.home,config['instance_id'],metadata['current']['version'])
    return service

def main():
    os.umask(0o077)
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--host-repo',required=True,type=Path)
    p.add_argument('--host-binary',required=True,type=Path)
    p.add_argument('--runtime-binary',required=True,type=Path)
    p.add_argument('--home',required=True,type=Path)
    p.add_argument('--name',default='Native Workshop')
    p.add_argument('--provider',default='openai')
    p.add_argument('--model')
    p.add_argument('--key-env')
    p.add_argument('--endpoint')
    p.add_argument('--auth',choices=['auto','bearer','api-key','none'],default='auto')
    p.add_argument('--copilot-python',type=Path)
    p.add_argument('--copilot-bridge',type=Path)
    p.add_argument('--reasoning-effort')
    p.add_argument('--thinking-budget-tokens',type=int)
    p.add_argument('--api-version')
    p.add_argument('--tls-ca-file',type=Path)
    p.add_argument('--allow-insecure-http',action='store_true')
    p.add_argument('--mode',choices=['ask','read_only','workspace'],default='ask')
    p.add_argument('--public-url')
    p.add_argument('--no-start',action='store_true')
    a=p.parse_args()
    assert a.home.is_absolute() and not a.home.exists(),'Choose a new absolute installation folder; existing agents are preserved'
    assert a.runtime_binary.is_file() and a.host_binary.is_file()
    runtime_version=run(a.runtime_binary,'--version').stdout.strip()
    assert runtime_version.startswith('agent-runtime '),'Expected the native runtime executable'
    presets=json.loads(run(a.runtime_binary,'providers').stdout)
    preset=next((x for x in presets if x['provider']==a.provider),None)
    assert preset,'Unknown provider; run agent-runtime providers for supported choices'
    key_env=a.key_env or preset['key_env']
    key='' if a.auth=='none' or a.provider=='mock' else os.environ.get(key_env,'').strip()
    required=a.auth!='none' and a.provider!='mock' and (a.auth!='auto' or a.provider in ['openai','azure-openai','anthropic','gemini','github-copilot'])
    assert not required or key,'Set the model credential in '+key_env
    flags=['--provider',a.provider,'--auth',a.auth]
    for name in ['model','endpoint','reasoning_effort','thinking_budget_tokens','api_version','tls_ca_file','copilot_python','copilot_bridge']:
        value=getattr(a,name)
        if value is not None:flags.extend(['--'+name.replace('_','-'),str(value)])
    if a.allow_insecure_http:flags.append('--allow-insecure-http')
    # Validate the complete provider configuration before creating a service.
    with tempfile.TemporaryDirectory() as test:
        run(a.runtime_binary,'--home',Path(test)/'state','init','--workspace',Path(test)/'workspace',*flags)
    native=platform_host_install(a)
    # The host owns the instance secret. The runtime receives only its dedicated
    # model credential, never the host control token or connector credentials.
    runtime=a.home/'engine';(runtime/'bin').mkdir(parents=True,mode=0o700)
    engine_hash=hashlib.sha256(a.runtime_binary.read_bytes()).hexdigest()
    release=runtime/'releases'/(runtime_version.split()[-1]+'-'+engine_hash[:16]);release.mkdir(parents=True,mode=0o700)
    executable=release/('agent-runtime.exe' if platform.system()=='Windows' else 'agent-runtime');shutil.copy2(a.runtime_binary,executable);executable.chmod(0o700)
    workspace=a.home/'workspace'
    run(executable,'--home',runtime,'init','--workspace',workspace,'--name',a.name,*flags)
    config=json.loads((runtime/'runtime.json').read_text());config['mode']=a.mode;config['provider']['api_key_env']='AGENT_MODEL_API_KEY'
    if a.public_url:config['trusted_mcp_readonly_urls']=[a.public_url.rstrip('/')+'/context/mcp']
    write(runtime/'runtime.json',json.dumps(config,indent=2)+'\n')
    if key:write(a.home/'secrets/native-model-key',key+'\n')
    doc=json.loads((a.home/'config.json').read_text());c=doc['config']
    c['purpose']='Independent agent using the native Rust model and tool execution loop.'
    c['setup_complete']=True;c['enabled']=True
    c['backend']={'command':'${AGENT_HOME}/'+executable.relative_to(a.home).as_posix(),'args':['--home','${AGENT_HOME}/engine','acp'],'env':{},'secret_env':{'AGENT_MODEL_API_KEY':'native-model-key'} if key else {},'working_directory':'${AGENT_HOME}/workspace','allowed_working_roots':['${AGENT_HOME}/workspace']}
    if a.public_url:c['server']['public_url']=a.public_url
    doc['revision']+=1;write(a.home/'config.json',json.dumps(doc,indent=2)+'\n')
    service=platform_start(a,c,native)
    runtime_check=json.loads(run(executable,'--home',runtime,'check').stdout)
    receipt={'instance_id':c['instance_id'],'home':str(a.home),'runtime':runtime_version,'runtime_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'provider':a.provider,'model':config['provider']['model'],'mode':a.mode,'service':service,'platform':platform.system(),'started':not a.no_start,'public_url':a.public_url,'native_history':'engine/history.sqlite3','legacy_migration':False,'local_command_execution':runtime_check.get('local_command_isolation') or 'unavailable; file tools and explicit MCP remain available'}
    write(runtime/'installation.json',json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt,indent=2))

if __name__=='__main__':main()
