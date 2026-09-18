"""Local process acceptance; isolated daemon and metadata-only hook payloads."""
import json, os, pathlib, socket, subprocess, sys, tempfile, time, urllib.request

cli = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='peercarry-ai-e2e-'))
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
(root / 'config.toml').write_text(f'[network]\nbind = "127.0.0.1"\nport = {port}\n', encoding='utf-8')
env = {**os.environ, 'PEERCARRY_DATA_DIR': str(root), 'PEERCARRY_TAILSCALE': str(root/'no-tailscale')}
flags = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path):
    with http.open(f'http://127.0.0.1:{port}{path}', timeout=2) as response:
        return json.load(response)
def hook(tool, event):
    payload = {'session_id': f'{tool}-fixture', 'cwd': 'C:/private/project-fixture', 'hook_event_name': event, 'prompt':'SECRET_MUST_NOT_BE_SAVED'}
    result = subprocess.run([str(cli), 'ai', 'hook', '--tool', tool], input=json.dumps(payload), text=True, capture_output=True, env=env, timeout=5, creationflags=flags)
    assert result.returncode == 0, result.stderr
    if tool == 'codex': assert json.loads(result.stdout) == {}

# Source can emit while the daemon is offline; startup must replay the outbox.
hook('codex', 'UserPromptSubmit')
hook('codex', 'Stop')
process = subprocess.Popen([str(cli), 'serve'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=flags)
try:
    deadline = time.monotonic()+20
    while True:
        try:
            events = get('/v1/ai/events')
            if len(events) == 2: break
        except OSError: pass
        assert process.poll() is None, 'isolated daemon exited'
        assert time.monotonic()<deadline, 'outbox replay timed out'
        time.sleep(.2)
    hook('zcode', 'PermissionRequest')
    deadline=time.monotonic()+20
    while len(get('/v1/ai/events')) != 3:
        assert time.monotonic()<deadline
        time.sleep(.2)
    events=get('/v1/ai/events')
    assert {e['tool'] for e in events} == {'codex','zcode'}
    assert all(e['project']=='project-fixture' for e in events)
    assert 'SECRET_MUST_NOT_BE_SAVED' not in json.dumps(events)
    assert 'C:/private' not in json.dumps(events)
    assert len({e['id'] for e in events})==3
    request=urllib.request.Request(f'http://127.0.0.1:{port}/v1/ai/receiver', data=b'{"enabled":true}', headers={'Content-Type':'application/json'})
    with http.open(request) as response: assert response.status==204
    assert get('/v1/ai/overview')['receiver_enabled'] is True
    scan=get('/v1/ai/tools')
    assert 'codex' in scan and 'zcode' in scan
    assert get('/v1/ai/events') == events, 'reads must not duplicate events'
    print('PASS: offline outbox replay, Codex/ZCode metadata, receiver setting, tool scan, dedup')
    print('Evidence:',root)
finally:
    process.terminate()
    process.wait(timeout=8)
