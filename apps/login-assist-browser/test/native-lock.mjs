// Windows-only regression coverage for the native host's adjacent lock file.
// All files and processes are confined to a disposable target subdirectory.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { copyFile, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve, sep } from 'node:path';

if (process.platform !== 'win32') throw new Error('This regression script requires Windows');

const repo = resolve(import.meta.dirname, '../../..');
const work = resolve(repo, 'target', `login-assist-lock-test-${process.pid}-${Date.now()}`);
const binary = join(work, 'peercarry-login-host.exe');
const lockPath = join(work, 'peercarry-login-host.lock');
const origin = 'chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/';
const children = new Set();

function launch(args = []) {
  const child = spawn(binary, [...args, origin], { cwd: work, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  children.add(child);
  child.completion = new Promise((resolveExit, reject) => {
    child.once('error', reject);
    child.once('exit', code => { children.delete(child); resolveExit(code); });
  });
  child.completion.catch(() => {});
  child.stdout.resume();
  child.stderr.resume();
  return child;
}

async function exitCode(child) {
  let timer;
  try {
    return await Promise.race([child.completion, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('Native process did not exit')), 5000);
    })]);
  } finally { clearTimeout(timer); }
}

async function waitFor(predicate, description) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise(resolveWait => setTimeout(resolveWait, 25));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function lockExists() {
  try { await stat(lockPath); return true; }
  catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}

function recover() {
  return spawnSync(binary, ['--recover-lock'], { cwd: work, encoding: 'utf8', windowsHide: true });
}

let crashedHost;
try {
  await mkdir(work, { recursive: true });
  await copyFile(join(repo, 'target', 'debug', 'peercarry-login-host.exe'), binary);
  await writeFile(binary.replace(/\.exe$/i, '.json'), JSON.stringify({ allowed_origin: origin }));
  const config = JSON.parse(await readFile(binary.replace(/\.exe$/i, '.json'), 'utf8'));
  assert.equal(config.allowed_origin, origin, 'synthetic origin configuration');

  const activeHost = launch();
  await waitFor(lockExists, 'active host lock');
  assert.equal(recover().status, 1, 'recovery must refuse an active host lock');
  assert.equal((await lockExists()), true, 'active lock remains after refused recovery');

  const concurrentHost = launch();
  assert.notEqual(await exitCode(concurrentHost), null, 'concurrent host must exit after lock creation fails');
  assert.equal(activeHost.exitCode, null, 'original active host remains running');
  assert.equal((await lockExists()), true, 'concurrent host cannot remove the active lock');

  activeHost.stdin.end();
  assert.equal(await exitCode(activeHost), 0, 'normal EOF shutdown');
  await waitFor(async () => !(await lockExists()), 'normal lock cleanup');

  crashedHost = launch();
  await waitFor(lockExists, 'crash-simulation host lock');
  crashedHost.kill();
  await exitCode(crashedHost);
  crashedHost = undefined;
  assert.equal(await lockExists(), true, 'forced termination leaves the lock for recovery');

  const recovered = recover();
  assert.equal(recovered.status, 0, 'recovery removes stale lock');
  assert.equal(await lockExists(), false, 'recovery clears the stale lock');

  const restartedHost = launch();
  await waitFor(lockExists, 'host startup after recovery');
  restartedHost.stdin.end();
  assert.equal(await exitCode(restartedHost), 0, 'restarted host normal shutdown');
  await waitFor(async () => !(await lockExists()), 'restarted host lock cleanup');
  console.log('PASS: active recovery rejected; concurrent startup rejected; normal shutdown cleans lock; own-process crash recovered; restart succeeds.');
} finally {
  if (crashedHost && crashedHost.exitCode === null) crashedHost.kill();
  for (const child of children) {
    if (child.exitCode === null) child.kill();
  }
  await Promise.allSettled([...children].map(exitCode));
  const root = resolve(repo, 'target');
  if (!work.startsWith(root + sep)) throw new Error('Refusing cleanup outside the workspace target directory');
  await rm(work, { recursive: true, force: true, maxRetries: 3 });
}
