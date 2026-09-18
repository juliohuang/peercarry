"""Exercise the production helper with isolated native fixture processes.

Usage: python scripts/tests/update-helper-e2e.py target/debug/peercarry-tray.exe
Requires rustc. Does not start a real clipboard service or touch its data.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import uuid

CREATE_FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0
ROOT = Path('target') / ('update-e2e-' + uuid.uuid4().hex)
ROOT.mkdir(parents=True)
ROOT = ROOT.resolve()
SOURCE = r'''
use std::{io::{Read,Write}, net::TcpListener, time::{Duration,Instant}};
fn main() {
    let version = if cfg!(new_version) { "0.2.0" } else { "0.1.0" };
    let port = std::env::var("FIXTURE_PORT").unwrap();
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::fs::write(std::env::var("FIXTURE_PID_FILE").unwrap(),std::process::id().to_string()).unwrap();
    let end = Instant::now() + Duration::from_secs(if std::env::var_os("FIXTURE_EXIT").is_some() { 2 } else { 30 });
    while Instant::now() < end {
        if let Ok((mut stream,_)) = listener.accept() {
            stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
            let mut buf=[0u8;2048]; let _=stream.read(&mut buf);
            let body=format!("{{\"version\":\"{version}\"}}");
            let reply=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            let _=stream.write_all(reply.as_bytes());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
'''
(ROOT / 'fixture.rs').write_text(SOURCE)
extension = '.exe' if os.name == 'nt' else ''
for name, flags in [('old', []), ('new', ['--cfg', 'new_version'])]:
    subprocess.run(['rustc', str(ROOT / 'fixture.rs'), '-o', str(ROOT / (name + extension)), *flags], check=True, creationflags=CREATE_FLAGS)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

for scenario, outcome in [('ok', 'ok'), ('invalid_binary', 'rolled_back'), ('wrong_version', 'rolled_back')]:
    case = ROOT / scenario
    case.mkdir()
    plan_dir = case / '.peercarry-update-fixture'
    plan_dir.mkdir()
    target = case / ('app' + extension)
    shutil.copy2(ROOT / ('old' + extension), target)
    stage = plan_dir / 'newbinary'
    if outcome == 'ok':
        shutil.copy2(ROOT / ('new' + extension), stage)
    elif scenario == 'wrong_version':
        shutil.copy2(ROOT / ('old' + extension), stage)
    else:
        stage.write_bytes(b'not an executable')
    helper = plan_dir / ('helper' + extension)
    shutil.copy2(Path(sys.argv[1]).resolve(), helper)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = {**os.environ, 'FIXTURE_PORT': str(port), 'FIXTURE_PID_FILE': str(case / 'pid')}
    env.pop('FIXTURE_EXIT', None)
    old = subprocess.Popen([str(target)], env={**env, 'FIXTURE_EXIT': '1'}, creationflags=CREATE_FLAGS)
    plan = {'oldpid': old.pid, 'target': str(target), 'staged': str(stage),
            'expectedsha256': hashlib.sha256(stage.read_bytes()).hexdigest(), 'newversion': '0.2.0', 'port': port}
    plan_path = plan_dir / 'plan.json'
    plan_path.write_text(json.dumps(plan), encoding='utf-8')
    try:
        result = subprocess.run([str(helper), '--apply-staged-update', str(plan_path)], env=env, timeout=25, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=CREATE_FLAGS)
        status = json.loads((plan_dir / 'status.json').read_text())['status']
        assert status == outcome, (outcome, status, result.returncode)
        assert (result.returncode == 0) == (outcome == 'ok')
        expected = '0.2.0' if outcome == 'ok' else '0.1.0'
        deadline = time.monotonic() + 5
        while True:
            try:
                with opener.open(f'http://127.0.0.1:{port}/v1/hello', timeout=1) as response:
                    assert json.load(response)['version'] == expected
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(.1)
        print(f'{scenario}: PASS ({outcome}; helper exit, file replacement, live version health)', flush=True)
    finally:
        old.wait(timeout=5)
        if (case / 'pid').exists():
            pid = int((case / 'pid').read_text())
            if pid != old.pid:
                # Only terminate the fixture child whose pid was written in this isolated test directory.
                try:
                    os.kill(pid, 15)
                except OSError:
                    pass
print('Evidence directory:', ROOT)
