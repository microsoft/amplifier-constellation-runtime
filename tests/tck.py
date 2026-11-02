"""Run upstream ACP v1 tests against a fresh native agent and local model fixture.

Requires a separately checked-out, pinned TCK and uv; makes no paid model calls.
The trusted-local flag explicitly allows the TCK's supplied stdio MCP fixture.
Use --restricted to inspect the deliberate default stdio-MCP policy exception.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import protocol

PIN = 'b7f40fb3c003c45926547be5a1f7e2b374035815'

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tck',type=Path,required=True)
    parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--restricted',action='store_true')
    args=parser.parse_args()
    checkout=args.tck.resolve()
    actual=subprocess.run(['git','-C',str(checkout),'rev-parse','HEAD'],check=True,capture_output=True,text=True).stdout.strip()
    if actual!=PIN:parser.error('TCK checkout must be pinned to '+PIN)
    binary=protocol.BINARY.resolve()
    server=protocol.ThreadingHTTPServer(('127.0.0.1',0),protocol.Handler)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='native-acp-tck-') as temporary:
            base=Path(temporary);home=base/'engine';work=base/'work'
            subprocess.run([str(binary),'--home',str(home),'init','--workspace',str(work),'--model','fixture'],check=True)
            path=home/'runtime.json';config=json.loads(path.read_text())
            config['provider'].update(endpoint=f'http://127.0.0.1:{server.server_port}/responses',api_key_env='RUNTIME_FIXTURE_KEY',reasoning_effort=None)
            path.write_text(json.dumps(config))
            env={**os.environ,'TMPDIR':str(work),'TMP':str(work),'TEMP':str(work),'RUNTIME_FIXTURE_KEY':'fixture'}
            command=['uv','run','--project',str(checkout),'--locked','acp-tck','--timeout','8','--test-timeout','20','--cancel-prompt','slow','--report-json',str(args.report.resolve()),'--',str(binary),'--home',str(home),'acp']
            if not args.restricted:command.append('--trusted-local-client')
            result=subprocess.run(command,env=env)
            return result.returncode
    finally:
        server.shutdown();server.server_close()

if __name__=='__main__':raise SystemExit(main())
