const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/timeline.html'), 'utf8');
const preferencesScript = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/device-preferences.js'), 'utf8');
const { create: createPreferences, KEY, WORKBENCH_KEY } = require('../../crates/peercarry-core/device-preferences.js');
const workbench = html.match(/\/\* ------------------------------------------------------------ workbench \*\/([\s\S]*?)\/\* ------------------------------------------------------------ native files \*\//)?.[1];
assert.ok(workbench, 'workbench script markers should exist');

class Element {
  constructor(tag = 'div') {
    this.tagName = tag.toUpperCase(); this.children = []; this.style = {}; this.dataset = {};
    this.value = ''; this.textContent = ''; this.disabled = false; this.hidden = false;
    this.attributes = {}; this.className = ''; this.onclick = null; this.onchange = null;
    this.listeners = {};
    this.classList = { toggle: (name, on) => { this.attributes[`class:${name}`] = !!on; } };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = [...children]; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  addEventListener(type, callback) { this.listeners[type] = callback; }
  querySelector(selector) {
    if (selector === '.device-check') {
      if (!this._check) this._check = new Element('span');
      return this._check;
    }
    return null;
  }
  querySelectorAll(selector) {
    const found = [];
    const visit = node => { for (const child of node.children) { if (selector === 'button' && child.tagName === 'BUTTON') found.push(child); visit(child); } };
    visit(this); return found;
  }
  focus() { this.focused = true; }
  scrollIntoView() {}
}

async function fixture({ peers = [], blockedStorage = false, savedTarget = '' } = {}) {
  const elements = new Map(), calls = [], events = {}, stored = new Map();
  if (savedTarget) stored.set(WORKBENCH_KEY, savedTarget);
  let currentPeers = peers;
  const document = {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element('div')); return elements.get(id); },
    createElement(tag) { return new Element(tag); },
    addEventListener(type, callback) { events[type] = callback; },
  };
  const response = (data, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => data, text: async () => JSON.stringify(data) });
  const sandbox = {
    document, $: id => document.getElementById(id), q: document.getElementById('q'), zh: false, icon: () => '', showTab: name => calls.push({ tab: name }),
    show: (...args) => calls.push({ toast: args }), refresh: async () => { calls.push({ refresh: true }); },
    addEventListener: (type, callback) => { events[type] = callback; },
    localStorage: {
      getItem: key => stored.get(key) ?? null,
      setItem: (key, value) => { if (blockedStorage) throw Error('storage denied'); stored.set(key, String(value)); },
    },
    async fetch(url, options = {}) {
      calls.push({ url, options });
      if (url === '/v1/peers') return response(currentPeers);
      if (url === '/v1/transfers') return response({ id: 'transfer-1' }, 202);
      if (url === '/v1/actions/fetch-clipboard') return response({ ok: true });
      return response([]);
    },
  };
  sandbox.window = sandbox;
  vm.runInNewContext(preferencesScript, sandbox, { filename: 'device-preferences.js' });
  vm.runInNewContext(workbench, sandbox, { filename: 'timeline.html#workbench' });
  await sandbox.loadPeers();
  return { sandbox, elements, calls, events, stored, setPeers: value => { currentPeers = value; } };
}

const peers = [
  { node_id: 'stable-peer-id-1', host: 'Workstation', dns_name: 'workstation.tailnet', addresses: ['100.64.0.10'], online: true },
  { node_id: 'stable-peer-id-2', host: 'Travel Laptop', dns_name: 'laptop.tailnet', addresses: ['100.64.0.11'], online: false },
];
const posts = (calls, url) => calls.filter(call => call.url === url && call.options.method === 'POST');

test('choosing a device only updates the selection and stores its preference id', async () => {
  const f = await fixture({ peers });
  const card = f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id);
  await card.onclick();
  assert.equal(posts(f.calls, '/v1/transfers').length, 0);
  assert.equal(posts(f.calls, '/v1/actions/fetch-clipboard').length, 0);
  assert.equal(f.elements.get('remote-peer').value, peers[0].node_id);
  assert.equal(f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).focused, true);
  assert.equal(JSON.parse(f.stored.get(KEY)).selected, peers[0].node_id);
  const visible = [];
  const visit = element => { visible.push(element); for (const child of element.children) visit(child); };
  for (const root of f.elements.values()) visit(root);
  const visibleText = visible.map(element => element.textContent).join(' ');
  assert.equal(visibleText.includes(peers[0].node_id), false);
  assert.equal(visibleText.includes(peers[0].addresses[0]), false);
  assert.match(visibleText, /Workstation/);
  assert.deepEqual(JSON.parse(f.stored.get(KEY)), { version: 1, selected: peers[0].node_id, favorites: [], recent: [peers[0].node_id] });
});

test('sending without a selected target does not issue a request', async () => {
  const f = await fixture({ peers });
  await f.elements.get('remote-send').onclick();
  assert.equal(posts(f.calls, '/v1/transfers').length, 0);
  assert.equal(f.elements.get('remote-send').disabled, true);
});

test('offline peers can queue a history entry with the default TTL without recapturing', async () => {
  const f = await fixture({ peers });
  const card = f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[1].node_id);
  card.onclick();
  assert.equal(f.elements.get('remote-fetch').disabled, true);
  await f.elements.get('remote-fetch').onclick();
  assert.equal(posts(f.calls, '/v1/actions/fetch-clipboard').length, 0);
  await f.sandbox.sendEntry({ id: 'history-entry-42', preview: 'private clipboard text' });
  const request = posts(f.calls, '/v1/transfers').at(-1);
  assert.deepEqual(JSON.parse(request.options.body), { operation: 'send', node_id: peers[1].node_id, expires_in_secs: 86400, entry_id: 'history-entry-42' });
  assert.equal(posts(f.calls, '/v1/actions/fetch-clipboard').length, 0);
  assert.equal(JSON.stringify(f.calls).includes('private clipboard text'), false);
});

test('a fetch adds the result to history and never applies it to the clipboard', async () => {
  const f = await fixture({ peers });
  f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).onclick();
  f.calls.length = 0;
  await f.elements.get('remote-fetch').onclick();
  assert.deepEqual(JSON.parse(posts(f.calls, '/v1/actions/fetch-clipboard')[0].options.body), { node_id: peers[0].node_id });
  assert.equal(f.calls.some(call => call.url === '/v1/actions/apply'), false);
  assert.equal(f.calls.filter(call => call.refresh).length, 1);
});

test('a rapid double click queues only one transfer', async () => {
  const f = await fixture({ peers });
  f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).onclick();
  let release;
  f.sandbox.fetch = async (url, options) => {
    f.calls.push({ url, options });
    if (url === '/v1/transfers') return new Promise(resolve => { release = () => resolve({ ok: true, status: 202, json: async () => ({}) }); });
    return { ok: true, status: 200, json: async () => [] };
  };
  const first = f.elements.get('remote-send').onclick();
  await f.elements.get('remote-send').onclick();
  assert.equal(posts(f.calls, '/v1/transfers').length, 1);
  release(); await first;
  assert.equal(f.elements.get('remote-send').disabled, false);
});

test('a refresh does not keep a remembered target that disappeared', async () => {
  const f = await fixture({ peers: [peers[0]], savedTarget: peers[1].node_id });
  assert.equal(f.elements.get('remote-peer').value, '');
  assert.equal(f.elements.get('remote-send').disabled, true);
  await f.elements.get('remote-send').onclick();
  assert.equal(posts(f.calls, '/v1/transfers').length, 0);
  assert.equal(f.stored.get(WORKBENCH_KEY), peers[1].node_id);
  assert.match(f.elements.get('remote-target-notice').textContent, /unavailable/);
});

test('favorites and another window selection update the target without sending or fetching', async () => {
  const f = await fixture({ peers });
  f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).onclick();
  f.elements.get('remote-favorite').onclick();
  assert.deepEqual(JSON.parse(f.stored.get(KEY)).favorites, [peers[0].node_id]);
  assert.equal(posts(f.calls, '/v1/transfers').length, 0);
  assert.equal(posts(f.calls, '/v1/actions/fetch-clipboard').length, 0);

  const other = createPreferences({ getItem: key => f.stored.get(key) ?? null, setItem: (key, value) => f.stored.set(key, value) });
  other.select(peers[1].node_id);
  f.events.storage({ key: KEY });
  assert.equal(f.elements.get('remote-peer').value, peers[1].node_id);
  assert.equal(f.elements.get('remote-fetch').disabled, true);
  await f.elements.get('remote-send').onclick();
  assert.equal(JSON.parse(posts(f.calls, '/v1/transfers').at(-1).options.body).node_id, peers[1].node_id);
});

test('a removed selected target is cleared and a busy refresh runs after the action', async () => {
  const f = await fixture({ peers });
  f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).onclick();
  let release;
  f.sandbox.fetch = async (url, options = {}) => {
    f.calls.push({ url, options });
    if (url === '/v1/transfers') return new Promise(resolve => { release = () => resolve({ ok: true, status: 202, json: async () => ({}) }); });
    return { ok: true, status: 200, json: async () => [peers[1]] };
  };
  const first = f.elements.get('remote-send').onclick();
  await f.sandbox.loadPeers();
  release(); await first;
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.elements.get('remote-peer').value, '');
  assert.equal(f.elements.get('remote-send').disabled, true);
  assert.match(f.elements.get('remote-target-notice').textContent, /unavailable/);
  assert.match(f.elements.get('remote-capture-status').textContent, /Queued for Workstation/);
  assert.equal(posts(f.calls, '/v1/transfers').length, 1);
});

test('storage failure keeps an explicit target in memory and fetch denial stays visible', async () => {
  const f = await fixture({ peers, blockedStorage: true });
  f.elements.get('device-cards').children.find(item => item.dataset.nodeId === peers[0].node_id).onclick();
  await f.sandbox.loadPeers();
  assert.equal(f.elements.get('remote-peer').value, peers[0].node_id);
  f.sandbox.fetch = async (url, options = {}) => {
    f.calls.push({ url, options });
    return { ok: false, status: 403, text: async () => 'denied' };
  };
  await f.elements.get('remote-fetch').onclick();
  assert.match(f.elements.get('remote-capture-status').textContent, /other computer’s settings/);
  assert.equal(f.calls.some(call => call.url === '/v1/actions/apply'), false);
});

test('timeline keyboard handling refreshes search Enter, applies page Enter, and ignores IME Enter', () => {
  const start = html.indexOf("document.addEventListener('keydown'");
  const end = html.indexOf('/* ------------------------------------------------------------ AI task alerts */', start);
  assert.ok(start >= 0 && end > start, 'keyboard handler markers should exist');
  const elements = new Map();
  const document = {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element(id)); return elements.get(id); },
    addEventListener(type, callback) { this.listeners ??= {}; this.listeners[type] = callback; },
  };
  const q = document.getElementById('q');
  const timeline = document.getElementById('timeline');
  timeline.hidden = false;
  const actions = { applies: [], refreshes: 0 };
  const sandbox = {
    document, q, sel: 3,
    $: id => document.getElementById(id),
    apply: index => actions.applies.push(index),
    refresh: () => { actions.refreshes++; },
    moveSelection() {}, togglePin() {}, clearTimeout() {}, setTimeout() { return 1; },
  };
  vm.runInNewContext(html.slice(start, end), sandbox, { filename: 'timeline.html#keyboard' });

  const event = (target, isComposing = false) => ({
    key: 'Enter', target, isComposing, ctrlKey: false, metaKey: false,
    prevented: false, preventDefault() { this.prevented = true; },
  });
  q.closest = selector => selector.includes('input') ? q : null;
  const searchEnter = event(q);
  q.listeners.keydown(searchEnter);
  document.listeners.keydown(searchEnter);
  assert.equal(actions.refreshes, 1);
  assert.deepEqual(actions.applies, []);
  assert.equal(searchEnter.prevented, true);

  const pageEnter = event({ closest: () => null });
  document.listeners.keydown(pageEnter);
  assert.deepEqual(actions.applies, [3]);
  assert.equal(actions.refreshes, 1);

  const imeEnter = event({ closest: () => null }, true);
  document.listeners.keydown(imeEnter);
  assert.deepEqual(actions.applies, [3]);
  assert.equal(actions.refreshes, 1);
  assert.equal(imeEnter.prevented, false);
});
