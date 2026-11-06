"""Exercise native commands against a disposable administrator container. No model."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]

def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)

def main():
    name = 'constellation-command-test-' + uuid.uuid4().hex[:12]
    with tempfile.TemporaryDirectory(prefix=name) as temporary:
        stage = Path(temporary)
        build = subprocess.run(['cargo', 'test', '--locked', '--test', 'container_commands', '--no-run', '--message-format=json'], cwd=ROOT, check=True, capture_output=True, text=True)
        artifacts = [Path(v['executable']) for line in build.stdout.splitlines() if (v := json.loads(line)).get('reason') == 'compiler-artifact' and v.get('target', {}).get('name') == 'container_commands' and v.get('executable')]
        if len(artifacts) != 1:
            raise RuntimeError('Build exactly one container_commands test binary before this check')
        shutil.copy2(artifacts[0], stage/'check')
        (stage/'marker.json').write_text(json.dumps({'container_admin': True, 'command_boundary': 'container'}))
        (stage/'Dockerfile').write_text('''FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends sudo && rm -rf /var/lib/apt/lists/* \\
 && usermod -l agent -d /home/agent ubuntu \\
 && printf 'Defaults:agent !use_pty\\nagent ALL=(ALL:ALL) NOPASSWD: ALL\\n' > /etc/sudoers.d/agent \\
 && chmod 0440 /etc/sudoers.d/agent && visudo -cf /etc/sudoers.d/agent
COPY check /check
USER 1000:1000
ENTRYPOINT ["/check", "--nocapture"]
''')
        try:
            run('docker', 'build', '-t', name, str(stage))
            args = ['docker', 'run', '--rm', '--name', name, '--network', 'none', '--cap-drop', 'ALL', '--pids-limit', '128', '--memory', '512m']
            for cap in ['CHOWN', 'DAC_OVERRIDE', 'FOWNER', 'FSETID', 'KILL', 'SETGID', 'SETUID', 'SETFCAP', 'SYS_CHROOT']:
                args += ['--cap-add', cap]
            mount = f'type=bind,src={stage}/marker.json,dst=/etc/constellation-container.json'
            # A writable marker must not enable container commands.
            run(*args, '--mount', mount, name)
            run(*args, '--mount', mount+',readonly', '-e', 'CONTAINER_COMMAND_TEST=enabled', '-e', 'COMMAND_TEST_SECRET=not-inherited', name)
        finally:
            subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            subprocess.run(['docker', 'image', 'rm', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

if __name__ == '__main__':
    main()
