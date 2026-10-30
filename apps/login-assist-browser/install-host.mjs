import { copyFile, mkdir, writeFile, access, chmod } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { homedir, platform } from 'node:os';
import { execFileSync } from 'node:child_process';
const [id, binary] = process.argv.slice(2);
if (!/^[a-p]{32}$/.test(id || '') || !binary) throw new Error('Usage: node install-host.mjs EXTENSION_ID HOST_BINARY');
const os = platform();
if (!['win32','darwin'].includes(os)) throw new Error('Windows and macOS only');
const dir = os === 'win32' ? join(process.env.LOCALAPPDATA, 'PeerCarry', 'LoginAssist') : join(homedir(), 'Library', 'Application Support', 'PeerCarry', 'LoginAssist');
await mkdir(dir, { recursive: true, mode: 0o700 });
const destination = join(dir, os === 'win32' ? 'peercarry-login-host.exe' : 'peercarry-login-host');
try { await access(destination.replace(/\.exe$/, '') + '.lock'); throw new Error('Host lock exists; close Chrome login assist and reconcile a crashed session before reinstalling'); }
catch (error) { if (error.code !== 'ENOENT') throw error; }
await copyFile(resolve(binary), destination);
await chmod(destination, 0o700);
await writeFile(destination.replace(/\.exe$/, '') + '.json', JSON.stringify({ allowed_origin: `chrome-extension://${id}/` }), { mode: 0o600 });
const manifestDir = os === 'win32' ? dir : join(homedir(), 'Library', 'Application Support', 'Google', 'Chrome', 'NativeMessagingHosts');
await mkdir(manifestDir, { recursive: true });
const manifest = join(manifestDir, 'dev.peercarry.login_assist.json');
await writeFile(manifest, JSON.stringify({ name:'dev.peercarry.login_assist', description:'PeerCarry private login host', path:destination, type:'stdio', allowed_origins:[`chrome-extension://${id}/`] }, null, 2), { mode: 0o600 });
if (os === 'win32') execFileSync('reg.exe', ['add', 'HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\dev.peercarry.login_assist', '/ve', '/t', 'REG_SZ', '/d', manifest, '/f'], { stdio:'ignore' });
console.log('Native host registered for the specified extension. No network listener starts until local approval.');
