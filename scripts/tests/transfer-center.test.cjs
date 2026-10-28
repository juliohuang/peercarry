const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const view = require('../../crates/peercarry-core/transfer-center.js');
const localeSource = fs.readFileSync(require.resolve('../../crates/peercarry-core/ui-locale.js'), 'utf8');

test('send and download completion labels preserve their distinct meaning', () => {
  assert.equal(view.label({ operation: 'send', status: 'completed' }, true), '条目已送达');
  assert.equal(view.label({ operation: 'download', status: 'completed' }, false), 'Files downloaded');
});

test('queue states use neutral retry language and stable badge classes', () => {
  assert.equal(view.label({ status: 'waiting_for_peer' }, true), '等待重试');
  assert.equal(view.label({ status: 'waiting_for_peer' }, false), 'Waiting to retry');
  assert.equal(view.statusClass('waiting_for_peer'), 'transfer-status--waiting_for_peer');
  assert.equal(view.statusClass('unexpected <state>'), 'transfer-status--unknown');
});

test('send cards omit file-download rows while download progress stays accurate', () => {
  const files = [{ name: 'a.txt', bytes: 4, total_bytes: 10 }, { name: 'b.txt', bytes: 3, total_bytes: 5 }];
  assert.deepEqual(view.visibleFiles({ operation: 'send', files }), []);
  assert.deepEqual(view.progress({ operation: 'send', files }), { bytes: 0, total: null, fraction: null });
  assert.deepEqual(view.progress({ operation: 'download', files }), { bytes: 7, total: 15, fraction: 7 / 15 });
  assert.equal(view.progress({ operation: 'download', files: [{ bytes: 100 }] }).fraction, null);
  assert.equal(view.progress({ operation: 'download', files: [{ bytes: 100, total_bytes: 90 }] }).fraction, 1);
});

test('titles stay concise and failure summaries are bounded', () => {
  assert.equal(view.title({ operation: 'send', target_name: 'Laptop' }, true), '发送至 Laptop');
  assert.equal(view.title({ operation: 'download', target_name: 'Desktop' }, false), 'Download files · Desktop');
  assert.equal(view.shortError('  first\nsecond  '), 'first second');
  assert.ok(view.shortError('x'.repeat(300)).length <= 120);
});

test('dates use the selected display locale', () => {
  const value = '2024-01-02T15:04:00.000Z';
  assert.notEqual(view.date(value, 'zh-CN'), view.date(value, 'en'));
});

test('shared locale prefers the native language and otherwise follows browser preferences', () => {
  const native = { navigator: { languages: ['en-US'], language: 'en' }, __PEERCARRY_LANG__: 'zh' };
  vm.runInNewContext(localeSource, native);
  assert.deepEqual({ ...native.PeerCarryLocale }, { language: 'zh', locale: 'zh-CN', isChinese: true });

  const browser = { navigator: { languages: ['zh-TW', 'en'], language: 'en' } };
  vm.runInNewContext(localeSource, browser);
  assert.deepEqual({ ...browser.PeerCarryLocale }, { language: 'zh', locale: 'zh-CN', isChinese: true });
});

test('completed or expired jobs cannot be replayed through the UI', () => {
  for (const status of ['completed', 'expired']) assert.deepEqual(view.actions({ status }), ['delete']);
  assert.deepEqual(view.actions({ status: 'running' }), ['cancel']);
  assert.deepEqual(view.actions({ status: 'paused' }), ['retry', 'cancel']);
  assert.deepEqual(view.actions({ status: 'failed' }), ['retry', 'delete']);
  assert.deepEqual(view.actions({ status: 'unknown' }), []);
});

test('list redraw preserves expanded details and action focus, then forgets deleted jobs', async () => {
  const source = require.resolve('../../crates/peercarry-core/transfer-center.js');
  const saved = Object.fromEntries(['window', 'document', 'navigator', '__PEERCARRY_LANG__', 'PeerCarryLocale', 'fetch', 'setInterval', 'clearInterval']
    .map(key => [key, Object.getOwnPropertyDescriptor(global, key)]));
  let poll;
  let rows = [{ id: 'job-1', operation: 'send', status: 'queued', node_id: 'node-1', target_name: 'Laptop' }];

  class Element {
    constructor(tag, owner) {
      this.tagName = tag.toUpperCase(); this.owner = owner; this.dataset = {}; this.children = [];
      this.attributes = {}; this.listeners = {}; this.parentNode = null; this.textContent = '';
      this.value = ''; this.disabled = false; this.hidden = false; this.open = false;
      this.options = [];
    }
    set className(value) { this._className = value; }
    get className() { return this._className || ''; }
    setAttribute(name, value) { this.attributes[name] = value; }
    addEventListener(name, callback) { (this.listeners[name] ||= []).push(callback); }
    append(...nodes) { for (const node of nodes) { node.parentNode = this; this.children.push(node); if (node.tagName === 'OPTION') this.options.push(node); } }
    replaceChildren(...nodes) {
      if (this.owner.activeElement && this.contains(this.owner.activeElement)) this.owner.activeElement = this.owner.body;
      for (const child of this.children) child.parentNode = null;
      this.children = []; this.options = []; this.append(...nodes);
    }
    contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
    get isConnected() { return this.owner.body.contains(this); }
    querySelectorAll(selector) {
      const all = []; const visit = node => { for (const child of node.children) { all.push(child); visit(child); } };
      visit(this);
      if (selector.startsWith('details')) return all.filter(node => node.tagName === 'DETAILS' && node.dataset.jobId);
      if (selector.startsWith('button')) return all.filter(node => node.tagName === 'BUTTON' && node.dataset.jobId && node.dataset.action);
      return [];
    }
    dispatch(name) { for (const callback of this.listeners[name] || []) callback({ target: this }); }
    focus() { this.owner.activeElement = this; }
  }
  const doc = {
    body: null, activeElement: null, hidden: false, listeners: {},
    elements: new Map(),
    getElementById(id) { return this.elements.get(id) || null; },
    createElement(tag) { return new Element(tag, this); },
    addEventListener(name, callback) { (this.listeners[name] ||= []).push(callback); },
  };
  doc.body = new Element('body', doc); doc.activeElement = doc.body;
  for (const id of ['transfers', 'transfer-peer', 'transfer-jobs', 'transfer-filter', 'transfer-send',
    'transfer-clear-selection', 'transfer-selection', 'transfer-status', 'transfer-help', 'transfer-title',
    'transfer-compose-title', 'transfer-options-title', 'transfer-ttl-label', 'transfer-list-title', 'transfer-ttl']) {
    const node = new Element(id === 'transfer-jobs' || id === 'transfers' ? 'section' : id === 'transfer-ttl' ? 'select' : 'div', doc);
    doc.elements.set(id, node); doc.body.append(node);
  }
  doc.elements.get('transfer-ttl').options = [{}, {}, {}];
  global.document = doc;
  Object.defineProperty(global, 'navigator', { value: { languages: ['en-US'], language: 'en' }, configurable: true });
  global.__PEERCARRY_LANG__ = 'zh';
  global.window = global;
  vm.runInThisContext(localeSource, { filename: 'ui-locale.js' });
  global.fetch = async path => ({ ok: true, status: 200, text: async () => path === '/v1/peers' ? '[]' : JSON.stringify(rows) });
  global.setInterval = callback => { poll = callback; return 1; };
  global.clearInterval = () => {};
  delete require.cache[source];
  try {
    require(source);
    assert.equal(global.PeerCarryLocale.language, 'zh');
    assert.equal(doc.getElementById('transfer-title').textContent, '传输中心');
    assert.equal(doc.getElementById('transfer-filter').attributes['aria-label'], '筛选传输任务');
    global.window.PeerCarryTransfers.setVisible(true);
    await new Promise(resolve => setImmediate(resolve));
    let detail = doc.getElementById('transfer-jobs').querySelectorAll('details')[0];
    detail.open = true; detail.dispatch('toggle');
    let button = doc.getElementById('transfer-jobs').querySelectorAll('button')[0];
    button.focus();
    poll();
    await new Promise(resolve => setImmediate(resolve));
    detail = doc.getElementById('transfer-jobs').querySelectorAll('details')[0];
    button = doc.getElementById('transfer-jobs').querySelectorAll('button')[0];
    assert.equal(detail.open, true);
    assert.equal(doc.activeElement, button);

    rows = [];
    poll(); await new Promise(resolve => setImmediate(resolve));
    rows = [{ id: 'job-1', operation: 'send', status: 'queued', node_id: 'node-1', target_name: 'Laptop' }];
    poll(); await new Promise(resolve => setImmediate(resolve));
    detail = doc.getElementById('transfer-jobs').querySelectorAll('details')[0];
    assert.equal(detail.open, false);

    global.__PEERCARRY_LANG__ = 'en';
    Object.defineProperty(global, 'navigator', { value: { languages: ['zh-CN'], language: 'zh-CN' }, configurable: true });
    global.window = global;
    vm.runInThisContext(localeSource, { filename: 'ui-locale.js' });
    delete require.cache[source];
    require(source);
    assert.equal(global.PeerCarryLocale.language, 'en');
    assert.equal(doc.getElementById('transfer-title').textContent, 'Transfers');
    assert.equal(doc.getElementById('transfer-filter').attributes['aria-label'], 'Filter transfers');
  } finally {
    delete require.cache[source];
    for (const [key, descriptor] of Object.entries(saved)) {
      if (descriptor === undefined) delete global[key]; else Object.defineProperty(global, key, descriptor);
    }
  }
});
