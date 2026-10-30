// Windows integration: temporary Chromium profile/extension and native-host registration.
// Only synthetic local fixtures are used. Pairing material stays in process memory.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import https from 'node:https';
import { readFile, writeFile, mkdir, cp, copyFile, rm } from 'node:fs/promises';
import { join, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync, spawn } from 'node:child_process';
const repo = resolve(import.meta.dirname, '../../..');
const work = join(repo, 'target', 'login-assist-e2e');
const profile = join(work,`profile-${Date.now()}`);
const playwrightPath = process.env.PLAYWRIGHT_MODULE;
if (!playwrightPath || !process.env.CHROMIUM_EXECUTABLE) throw new Error('Set PLAYWRIGHT_MODULE and CHROMIUM_EXECUTABLE');
const { chromium } = await import(pathToFileURL(playwrightPath));
const regKey = 'HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\dev.peercarry.login_assist';
const chromiumRegKey = 'HKCU\\Software\\Chromium\\NativeMessagingHosts\\dev.peercarry.login_assist';
for (const key of [regKey, chromiumRegKey]) if (spawnSync('reg.exe', ['query', key], { stdio:'ignore' }).status === 0) throw new Error('Existing native registration preserved; use a clean Windows test account');
await mkdir(work, { recursive: true });
const extension = join(work, 'extension');
await cp(join(repo,'apps/login-assist-browser/extension'), extension, { recursive:true });
const manifest = JSON.parse(await readFile(join(extension,'manifest.json'),'utf8'));
// Headless tests cannot click the toolbar action to grant activeTab. This grant
// exists only in the disposable test copy; production uses the user's action.
manifest.host_permissions = ['<all_urls>'];
await writeFile(join(extension,'manifest.json'), JSON.stringify(manifest));
const binary = join(work,'peercarry-login-host.exe');
await copyFile(process.env.HOST_BINARY || join(repo,'target/debug/peercarry-login-host.exe'), binary);
const fixture = '<!doctype html><meta charset="utf-8"><title>Login integration fixture</title><style>body{font:18px system-ui;margin:40px;background:#f3f5f7}form{width:360px;padding:20px;background:white;border:2px solid #13745e}label{display:block;margin:16px 0}input,button{font:inherit;padding:8px}#outside{background:#ff00ff;width:100px;height:100px}</style><h1>PeerCarry synthetic login</h1><form onsubmit="return false"><label>Account<input id="account" autocomplete="username"></label><label>Password<input id="password" type="password"></label><label>Code<input id="otp" autocomplete="one-time-code"></label><button id="login" type="button">Log in</button></form><div id="outside">Outside crop</div>';
const fixtureServer = createServer((req,res) => { res.writeHead(200,{'content-type':'text/html'}); res.end(fixture); });
await new Promise((resolve,reject) => { fixtureServer.once('error',reject); fixtureServer.listen(5188,'127.0.0.1',resolve); });
let context; let registered = false;
try {
  context = await chromium.launchPersistentContext(profile, {
    executablePath: process.env.CHROMIUM_EXECUTABLE, headless:true,
    viewport:null,
    args:['--window-size=1100,900',`--disable-extensions-except=${extension}`,`--load-extension=${extension}`],
  });
  const worker = context.serviceWorkers()[0] || await context.waitForEvent('serviceworker');
  const id = new URL(worker.url()).host;
  await writeFile(join(work,'peercarry-login-host.json'), JSON.stringify({allowed_origin:`chrome-extension://${id}/`}));
  const hostManifest = join(work,'host-manifest.json');
  await writeFile(hostManifest,JSON.stringify({name:'dev.peercarry.login_assist',description:'Temporary synthetic integration test',path:binary,type:'stdio',allowed_origins:[`chrome-extension://${id}/`]}));
  assert.equal(spawnSync('reg.exe',['add',regKey,'/ve','/t','REG_SZ','/d',hostManifest,'/f'],{stdio:'ignore'}).status,0,'temporary registration');
  registered = true;
  assert.equal(spawnSync('reg.exe',['add',chromiumRegKey,'/ve','/t','REG_SZ','/d',hostManifest,'/f'],{stdio:'ignore'}).status,0,'temporary Chromium registration');
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:5188');
  const popup = await context.newPage();
  popup.on('pageerror', error => console.error('Popup script error:', error.message));
  await popup.goto(`chrome-extension://${id}/popup.html`);
  await page.bringToFront();
  await popup.evaluate(() => { document.getElementById('bind').value = '127.0.0.1:0'; document.getElementById('confirm').click(); });
  await popup.waitForFunction(() => !document.getElementById('start').disabled);
  await popup.evaluate(() => document.getElementById('start').onclick());
  try { await popup.waitForFunction(() => document.getElementById('ticket').value.length > 0, null, {timeout:15000}); }
  catch {
    const status = await popup.evaluate(async () => {
      const s = await chrome.runtime.sendMessage({type:'status'});
      return {connected:s?.connected,error:s?.error,hasOrigin:!!s?.origin};
    });
    throw new Error('Startup: ' + JSON.stringify(status));
  }
  const ticket = JSON.parse(await popup.locator('#ticket').inputValue());
  const pem = `-----BEGIN CERTIFICATE-----\n${ticket.certificate.match(/.{1,64}/g).join('\n')}\n-----END CERTIFICATE-----\n`;
  const agent = new https.Agent({ ca:pem, keepAlive:true });
  let token;
  const post = (path, body, bearer = token) => new Promise((resolve,reject) => {
    const request = https.request(new URL(path,ticket.url), {method:'POST',agent,headers:{'content-type':'application/json',...(bearer ? {authorization:`Bearer ${bearer}`} : {})}}, response => {
      let body = '';
      response.on('data', chunk => { body += chunk; });
      response.on('end', () => { try { resolve({status:response.statusCode,data:JSON.parse(body)}); } catch { reject(new Error('Invalid HTTP response')); } });
    });
    request.on('error', () => reject(new Error('TLS request failed')));
    request.setTimeout(10000, () => request.destroy());
    request.end(JSON.stringify(body));
  });
  assert.equal((await post('/v1/pair',{secret:'wrong'})).status,401);
  token = (await post('/v1/pair',{secret:ticket.secret})).data.token;
  assert.equal(typeof token,'string');
  const command = async body => (await post('/v1/command',body)).data;
  let state = await command({op:'state'});
  assert.equal(state.status.phase,'AwaitingUser');
  assert.equal(state.target.origin,'http://127.0.0.1:5188');
  const epoch = state.status.epoch;
  assert.equal((await command({op:'frame',epoch})).error,'INVALID_STATE');
  state = await command({op:'approve',epoch});
  assert.equal(state.status.phase,'Controlling');
  const field = state.fields.find(f => f.kind === 'password').id;
  const fill = {op:'fill',epoch,command:1,field,value:'synthetic-only'};
  assert.equal((await command(fill)).status.phase,'Controlling');
  assert.equal(await page.locator('#password').inputValue(),'synthetic-only');
  assert.equal(await page.locator('#account').inputValue(),'');
  assert.equal((await command(fill)).error,'REPLAY');
  const capture = await command({op:'frame',epoch});
  assert.ok(capture.data?.png,'real browser capture: ' + (capture.error || 'missing'));
  const decoded = await page.evaluate(async png => {
    const image = new Image(); image.src = 'data:image/png;base64,' + png; await image.decode();
    const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
    const ctx = canvas.getContext('2d'); ctx.drawImage(image,0,0);
    const pixels = ctx.getImageData(0,0,canvas.width,canvas.height).data;
    let outside = false;
    for(let i=0;i<pixels.length;i+=4) if(pixels[i]===255 && pixels[i+1]===0 && pixels[i+2]===255) outside = true;
    return {width:image.width,height:image.height,outside};
  },capture.data.png);
  await writeFile(join(work,'cropped-frame.png'), Buffer.from(capture.data.png,'base64'));
  await page.screenshot({path:join(work,'browser-desktop.png')});
  console.log('Crop geometry', JSON.stringify({decoded,view:capture.data.view,viewport:capture.viewport}));
  assert.equal(decoded.width,capture.data.view.width); assert.equal(decoded.height,capture.data.view.height); assert.equal(decoded.outside,false);
  await page.screenshot({path:join(work,'browser-desktop.png')});
  // A real DOM change must suspend and require another explicit approval.
  await page.locator('#password').evaluate(el => { el.style.width = '180px'; });
  await new Promise(resolve => setTimeout(resolve,300));
  state = await command({op:'state'}); assert.equal(state.status.phase,'Suspended');
  assert.equal((await command({...fill,command:2})).error,'TARGET_CHANGED');
  state = await command({op:'refresh'}); assert.equal(state.status.phase,'AwaitingUser');
  state = await command({op:'approve',epoch:state.status.epoch});
  // Capture revision and operation scope remain independent after expansion.
  state = await command({op:'expand',epoch:state.status.epoch});
  assert.equal((await command({op:'click',epoch:state.status.epoch,command:3,x:900,y:700,revision:state.revision})).error,'OUT_OF_SCOPE');
  const button = await page.locator('#login').boundingBox();
  state = await command({op:'click',epoch:state.status.epoch,command:4,x:Math.floor(button.x+button.width/2),y:Math.floor(button.y+button.height/2),revision:state.revision});
  assert.equal(state.status.phase,'Suspended');
  // A successful website login may replace the entire document. Hand-back must
  // still revoke the old binding and succeed, without granting another input.
  await page.goto('http://127.0.0.1:5188/after-login');
  state = await command({op:'state'});
  assert.equal(state.status.phase,'Suspended');
  state = await command({op:'hand_back',epoch:state.status.epoch});
  assert.equal(state.status.phase,'Completed'); assert.equal(state.status.ai_resumed,false);
  assert.equal((await command({op:'frame',epoch:state.status.epoch})).error,'INVALID_STATE');
  await popup.evaluate(() => {
    document.getElementById('ticket').value = '';
    document.getElementById('qr').removeAttribute('src');
    document.getElementById('qr').hidden = true;
  });
  await popup.screenshot({path:join(work,'extension-popup.png')});
  agent.destroy();
  // A separate native process covers the actual Dart HTTPS client, without saving tickets.
  if (process.env.DART_EXECUTABLE) {
    await popup.evaluate(() => document.getElementById('stop').click());
    await popup.waitForFunction(() => !document.getElementById('start').disabled);
    await page.bringToFront();
    await popup.evaluate(() => document.getElementById('start').click());
    await popup.waitForFunction(() => document.getElementById('ticket').value.length > 0);
    const mobileTicket = await popup.locator('#ticket').inputValue();
    const dart = spawn(process.env.DART_EXECUTABLE, ['run','test/login_assist_live.dart'], {cwd:join(repo,'apps/mobile'),stdio:['pipe','pipe','pipe']});
    dart.stdin.end(mobileTicket + '\n');
    let output = ''; let diagnostics = ''; dart.stdout.on('data', chunk => output += chunk); dart.stderr.on('data', chunk => diagnostics += chunk);
    const code = await new Promise(resolve => dart.on('exit',resolve));
    if (code !== 0) console.error('Dart diagnostic:', diagnostics.replaceAll(mobileTicket, '[redacted]'));
    assert.equal(code,0,'Dart pinned HTTPS integration'); assert.ok(output.includes('PASS'));
    await popup.waitForFunction(async () => {
      const state = await chrome.runtime.sendMessage({type:'status'});
      return !state.origin && !state.ticket;
    });
  }
  console.log('PASS: Chromium extension + Native Messaging + pinned TLS + real DOM fill/crop + revocation; synthetic inputs only.');
} finally {
  await context?.close();
  await new Promise(resolve => fixtureServer.close(resolve));
  if (registered) for (const key of [regKey, chromiumRegKey]) spawnSync('reg.exe',['delete',key,'/f'],{stdio:'ignore'});
  if (!resolve(profile).startsWith(resolve(work) + sep)) throw new Error('Invalid test profile path');
  await rm(profile, {recursive:true,force:true,maxRetries:3});
}
