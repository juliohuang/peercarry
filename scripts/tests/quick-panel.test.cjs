const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const html = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/quick.html'), 'utf8');
const localeScript = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/ui-locale.js'), 'utf8');
const preferencesScript = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/device-preferences.js'), 'utf8');
const { create: createPreferences, KEY, WORKBENCH_KEY } = require('../../crates/peercarry-core/device-preferences.js');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.style = {}; this.attributes = {}; this.value = ''; this.textContent = ''; this.disabled = false; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this.attributes[name] = value; }
  focus() { this.focused = true; }
  select() {}
}
async function fixture(online = false, blockedStorage = false, { native, browser = 'zh-CN', initial = {}, peers } = {}) {
  const elements = new Map(), calls = [], saved = new Map(Object.entries(initial)), events = {};
  const availablePeers = peers || [{ node_id: 'stable-device-id', host: 'target', online }];
  const entry = { id: 'history-id', kind: 'text', preview: '<img src=x onerror=alert(1)>', origin: { host: 'source' }, created_at: '2026-09-26T00:00:00Z' };
  const documentElement = new Element('html');
  const document = {
    documentElement,
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element('div')); return elements.get(id); },
    createElement(tag) { return new Element(tag); },
    addEventListener(type, callback) { events[type] = callback; },
  };
  const sandbox = {
    document, navigator: { language: browser, languages: [browser] }, __PEERCARRY_LANG__: native,
    localStorage: { getItem: k => saved.get(k) ?? null, setItem: (k, v) => { if (blockedStorage) throw Error('storage denied'); saved.set(k, v); } },
    addEventListener: (type, callback) => { events[`window:${type}`] = callback; },
    async fetch(path, options = {}) {
      calls.push({ path, options });
      const data = path === '/v1/peers' ? availablePeers
        : path.startsWith('/v1/timeline') ? [entry] : { ok: true };
      return { ok: true, status: 200, json: async () => data, text: async () => JSON.stringify(data) };
    },
    ipc: { postMessage: message => calls.push({ ipc: message }) },
  };
  sandbox.window = sandbox;
  vm.runInNewContext(localeScript, sandbox);
  vm.runInNewContext(preferencesScript, sandbox);
  vm.runInNewContext(script, sandbox);
  await new Promise(resolve => setImmediate(resolve));
  return { sandbox, elements, calls, saved, events, entry };
}
function choose(f, id = 'stable-device-id') {
  f.elements.get('device').value = id;
  f.elements.get('device').onchange();
}

test('language reload waits until an explicit send finishes and does not submit it again', async () => {
  const f = await fixture(false, false, { native: 'en' });
  choose(f);
  let release, reloads = 0;
  f.sandbox.location = { reload: () => { reloads++; } };
  f.sandbox.fetch = async (path, options = {}) => {
    f.calls.push({ path, options });
    if (path === '/v1/transfers') return new Promise(resolve => { release = () => resolve({ ok: true, json: async () => ({ ok: true }) }); });
    return { ok: true, json: async () => path === '/v1/desktop/language' ? { language: 'zh' } : [] };
  };
  const send = f.elements.get('send').onclick();
  assert.equal(await f.sandbox.PeerCarryLocale.checkForChange(), false);
  assert.equal(reloads, 0);
  release(); await send;
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(reloads, 1);
  assert.equal(f.calls.filter(call => call.path === '/v1/transfers').length, 1);
});
test('an offline target can be queued but cannot have its current clipboard fetched', async () => {
  const f = await fixture(false);
  assert.equal(f.elements.get('send').disabled, true);
  await f.elements.get('send').onclick();
  assert.equal(f.calls.some(c => c.path === '/v1/transfers'), false);
  choose(f);
  assert.equal(f.elements.get('send').disabled, false);
  assert.equal(f.elements.get('fetch').disabled, true);
  await f.elements.get('send').onclick();
  const sent = f.calls.find(c => c.path === '/v1/transfers');
  assert.deepEqual(JSON.parse(sent.options.body), { operation: 'send', node_id: 'stable-device-id', expires_in_secs: 86400 });
  assert.equal(f.calls.some(c => c.path === '/v1/actions/apply'), false);
  const preferences = JSON.parse(f.saved.get(KEY));
  assert.deepEqual(preferences, { version: 1, selected: 'stable-device-id', favorites: [], recent: ['stable-device-id'] });
});
test('fetching creates history only; applying an entry requires its own explicit click', async () => {
  const f = await fixture(true, true);
  choose(f);
  await f.elements.get('fetch').onclick();
  assert.deepEqual(JSON.parse(f.calls.find(c => c.path === '/v1/actions/fetch-clipboard').options.body), { node_id: 'stable-device-id' });
  assert.equal(f.calls.some(c => c.path === '/v1/actions/apply'), false);
  const row = f.elements.get('results').children[0];
  assert.equal(row.children[0].textContent, f.entry.preview);
  await row.children[1].children[1].onclick();
  assert.deepEqual(JSON.parse(f.calls.find(c => c.path === '/v1/actions/apply').options.body), { id: 'history-id' });
});
test('a double click does not queue the clipboard twice', async () => {
  const f = await fixture();
  choose(f);
  let release;
  f.sandbox.fetch = async (path, options) => {
    f.calls.push({ path, options });
    return new Promise(resolve => { release = () => resolve({ ok: true, status: 202, json: async () => ({}) }); });
  };
  const first = f.elements.get('send').onclick();
  await f.elements.get('send').onclick();
  assert.equal(f.calls.filter(c => c.path === '/v1/transfers').length, 1);
  release(); await first;
  assert.equal(f.elements.get('send').disabled, false);
});
test('native Chinese overrides an English browser for visible text and accessibility labels', async () => {
  const f = await fixture(false, false, { native: 'zh', browser: 'en-US' });
  assert.equal(f.sandbox.PeerCarryLocale.language, 'zh');
  assert.equal(f.sandbox.document.documentElement.lang, 'zh-CN');
  assert.equal(f.elements.get('panel-subtitle').textContent, '快捷面板');
  assert.equal(f.elements.get('device-label').textContent, '发送到');
  assert.equal(f.elements.get('device').attributes['aria-label'], '选择要发送到的设备');
  assert.equal(f.elements.get('device').title, '选择要发送到的设备');
  assert.match(f.elements.get('device').children[1].textContent, /离线，可排队/);
  assert.equal(f.elements.get('query').attributes['aria-label'], '搜索历史内容');
  assert.equal(f.elements.get('query').title, '搜索历史内容');
  assert.equal(f.elements.get('favorite').title, '收藏此设备');
  assert.equal(f.elements.get('favorite').attributes['aria-label'], '收藏此设备');
  assert.equal(f.elements.get('search').attributes['aria-label'], '搜索');
  assert.equal(f.elements.get('results').children[0].children[1].children[0].textContent, `source · ${new Date(f.entry.created_at).toLocaleString('zh-CN')}`);
  assert.equal(f.elements.get('results').children[0].children[1].children[1].attributes['aria-label'], '复制');
});
test('native English overrides a Chinese browser for visible text and accessibility labels', async () => {
  const f = await fixture(false, false, { native: 'en', browser: 'zh-CN' });
  assert.equal(f.sandbox.PeerCarryLocale.language, 'en');
  assert.equal(f.sandbox.document.documentElement.lang, 'en');
  assert.equal(f.elements.get('panel-subtitle').textContent, 'Quick panel');
  assert.equal(f.elements.get('device-label').textContent, 'Send to');
  assert.equal(f.elements.get('device').attributes['aria-label'], 'Choose a device');
  assert.equal(f.elements.get('device').title, 'Choose a device');
  assert.match(f.elements.get('device').children[1].textContent, /offline, can queue/);
  assert.equal(f.elements.get('query').attributes['aria-label'], 'Search recent content');
  assert.equal(f.elements.get('query').title, 'Search recent content');
  assert.equal(f.elements.get('favorite').title, 'Favorite this device');
  assert.equal(f.elements.get('favorite').attributes['aria-label'], 'Favorite this device');
  assert.equal(f.elements.get('search').attributes['aria-label'], 'Search');
  assert.equal(f.elements.get('results').children[0].children[1].children[0].textContent, `source · ${new Date(f.entry.created_at).toLocaleString('en')}`);
  assert.equal(f.elements.get('results').children[0].children[1].children[1].attributes['aria-label'], 'Copy');
});

test('a missing saved target does not fall back to a discovered computer', async () => {
  const f = await fixture(true, false, { initial: { [WORKBENCH_KEY]: 'removed-id' } });
  assert.equal(f.elements.get('device').value, '');
  assert.equal(f.elements.get('send').disabled, true);
  assert.match(f.elements.get('device-notice').textContent, /未发现/);
  await f.elements.get('send').onclick();
  assert.equal(f.calls.some(c => c.path === '/v1/transfers'), false);
  assert.match(f.elements.get('status').textContent, /未发现/);
});

test('another window selection and favorite are reflected without performing an action', async () => {
  const peers = [
    { node_id: 'first-id', host: 'First', online: true },
    { node_id: 'second-id', host: 'Second', online: true },
  ];
  const f = await fixture(true, false, { peers });
  const writer = createPreferences({ getItem: key => f.saved.get(key) ?? null, setItem: (key, value) => f.saved.set(key, value) });
  writer.select('second-id');
  writer.toggleFavorite('second-id');
  f.events['window:storage']({ key: KEY });
  assert.equal(f.elements.get('device').value, 'second-id');
  assert.equal(f.elements.get('favorite').textContent, '★');
  assert.equal(f.calls.some(c => c.path === '/v1/transfers' || c.path === '/v1/actions/fetch-clipboard'), false);
  await f.elements.get('send').onclick();
  assert.equal(JSON.parse(f.calls.find(c => c.path === '/v1/transfers').options.body).node_id, 'second-id');
});

test('reopening while a send is busy refreshes after the send completes', async () => {
  const f = await fixture(true);
  choose(f);
  let release;
  f.sandbox.fetch = async (path, options = {}) => {
    f.calls.push({ path, options });
    if (path === '/v1/transfers') return new Promise(resolve => { release = () => resolve({ ok: true, status: 202, json: async () => ({}) }); });
    const data = path === '/v1/peers' ? [{ node_id: 'stable-device-id', host: 'target', online: true }] : [];
    return { ok: true, status: 200, json: async () => data, text: async () => '' };
  };
  const first = f.elements.get('send').onclick();
  await f.sandbox.quickPanelRefresh();
  assert.equal(f.calls.filter(c => c.path === '/v1/transfers').length, 1);
  release(); await first;
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.calls.filter(c => c.path === '/v1/peers').length, 2);
});

test('fetch success with a failed list refresh reports both facts and never applies the clipboard', async () => {
  const f = await fixture(true);
  choose(f);
  f.sandbox.fetch = async (path, options = {}) => {
    f.calls.push({ path, options });
    if (path.startsWith('/v1/timeline')) return { ok: false, status: 503, text: async () => 'unavailable' };
    return { ok: true, status: 200, json: async () => ({ ok: true }), text: async () => '' };
  };
  await f.elements.get('fetch').onclick();
  assert.match(f.elements.get('status').textContent, /已获取到本机历史.*刷新失败/);
  assert.equal(f.calls.some(c => c.path === '/v1/actions/apply'), false);
});

test('copy failure is reported and repeated clicks do not issue duplicate writes', async () => {
  const f = await fixture(true);
  let release;
  f.sandbox.fetch = async (path, options = {}) => {
    f.calls.push({ path, options });
    return new Promise(resolve => { release = () => resolve({ ok: false, status: 500, text: async () => 'failed' }); });
  };
  const copy = f.elements.get('results').children[0].children[1].children[1];
  const first = copy.onclick();
  await copy.onclick();
  assert.equal(f.calls.filter(c => c.path === '/v1/actions/apply').length, 1);
  release(); await first;
  assert.match(f.elements.get('status').textContent, /复制失败/);
});

test('a selection changed in another window waits for an in-flight send', async () => {
  const peers = [
    { node_id: 'first-id', host: 'First', online: true },
    { node_id: 'second-id', host: 'Second', online: true },
  ];
  const f = await fixture(true, false, { peers, native: 'en' });
  choose(f, 'first-id');
  let release;
  f.sandbox.fetch = async (path, options = {}) => {
    f.calls.push({ path, options });
    return new Promise(resolve => { release = () => resolve({ ok: true, status: 202, json: async () => ({}) }); });
  };
  const first = f.elements.get('send').onclick();
  const other = createPreferences({ getItem: key => f.saved.get(key) ?? null, setItem: (key, value) => f.saved.set(key, value) });
  other.select('second-id');
  f.events['window:storage']({ key: KEY });
  assert.equal(f.elements.get('device').value, 'first-id');
  release(); await first;
  assert.equal(JSON.parse(f.calls.find(c => c.path === '/v1/transfers').options.body).node_id, 'first-id');
  assert.equal(f.elements.get('device').value, 'second-id');
  assert.match(f.elements.get('status').textContent, /Queued for First/);
});
