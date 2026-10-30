import { access, cp, copyFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { execFileSync } from 'node:child_process';

if (process.platform !== 'win32') throw new Error('Windows packaging only');
const repo = resolve(import.meta.dirname, '../..');
const output = join(repo, 'target', 'login-assist-windows');
const binary = join(repo, 'target', 'release', 'peercarry-login-host.exe');
await access(binary);
await access(join(import.meta.dirname, 'extension', 'vendor', 'qrcode.mjs'));
await mkdir(output, { recursive: true });
await copyFile(binary, join(output, 'peercarry-login-host.exe'));
await copyFile(join(import.meta.dirname, 'install-host.mjs'), join(output, 'install-host.mjs'));
await copyFile(join(repo, 'docs', 'login-assist', 'windows-setup.md'), join(output, 'windows-setup.md'));
await cp(join(import.meta.dirname, 'extension'), join(output, 'extension'), { recursive: true });
// Use literal PowerShell paths and a fixed artifact location inside this workspace.
const quoted = value => `'${value.replaceAll("'", "''")}'`;
const inputs = ['peercarry-login-host.exe', 'install-host.mjs', 'windows-setup.md', 'extension']
  .map(name => quoted(join(output, name))).join(',');
execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
  `Compress-Archive -LiteralPath ${inputs} -DestinationPath ${quoted(join(repo, 'target', 'login-assist-windows.zip'))} -Force`],
{ stdio: 'inherit', windowsHide: true });
console.log('Created target/login-assist-windows.zip (manual preview, no persistent registration).');
