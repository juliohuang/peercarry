const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '../../crates/peercarry-core');
const localeScript = fs.readFileSync(path.join(root, 'ui-locale.js'), 'utf8');
const preferencesScript = fs.readFileSync(path.join(root, 'device-preferences.js'), 'utf8');
const timeline = fs.readFileSync(path.join(root, 'timeline.html'), 'utf8');
function context(native, navigator = {}) {
  const sandbox = { __PEERCARRY_LANG__: native, navigator, document: { documentElement: {} } };
  sandbox.window = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(localeScript, sandbox);
  return sandbox;
}

test('native display language takes priority over browser languages in both directions', () => {
  for (const [native, browser, expected] of [['zh', 'en-US', 'zh-CN'], ['en', 'zh-CN', 'en']]) {
    const s = context(native, { languages: [browser], language: browser });
    assert.equal(s.PeerCarryLocale.locale, expected);
    assert.equal(s.document.documentElement.lang, expected);
    assert.equal(Object.isFrozen(s.PeerCarryLocale), true);
  }
});

test('standalone browser uses only its primary preference and falls back to English', () => {
  const cases = [
    [{ languages: ['en-US', 'zh-CN'], language: 'zh-CN' }, 'en'],
    [{ languages: ['zh-Hant-TW', 'en-US'], language: 'en-US' }, 'zh'],
    [{ language: 'zh' }, 'zh'], [{ language: 'ZH_hans_CN.UTF-8' }, 'zh'],
    [{ language: 'zhang' }, 'en'], [{ language: 'de-DE' }, 'en'], [{}, 'en'],
  ];
  for (const [navigator, language] of cases) {
    assert.equal(context(undefined, navigator).PeerCarryLocale.language, language);
  }
  assert.equal(context('invalid', { language: 'zh-CN' }).PeerCarryLocale.language, 'zh');
});

class Element {
  constructor() {
    this.value = ''; this.style = {}; this.children = []; this.attributes = {};
    this.classList = { add() {}, remove() {}, toggle() {} }; this.textContent = ''; this.hidden = false;
  }
  setAttribute(k, v) { this.attributes[k] = v; }
  removeAttribute(k) { delete this.attributes[k]; }
  addEventListener() {}
  querySelectorAll() { return []; }
  querySelector(selector) {
    if (selector === '[data-secret-op="network.auth_token"]') return this.authInput ||= new Element();
    return new Element();
  }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  focus() { this.focused = true; }
  scrollIntoView() { this.scrolled = true; }
}

function settingsEnvelope(s) {
  const fields = vm.runInContext('FIELDS', s);
  const optional = new Set(['node.name', 'network.bind', 'storage.download_dir', 'storage.data_dir']);
  const values = { apps: [] };
  for (const field of fields) {
    if (!field.path || field.type === 'password') continue;
    values[field.path] = field.type === 'bool' ? false : field.type === 'number' ? 1 : optional.has(field.path) ? null : '';
  }
  return { schema: 1, revision: 'synthetic-revision', values, secrets: { 'network.auth_token': true, ai_token: false } };
}

async function workspace(native, browser, notebookApi) {
  const s = context(native, { languages: [browser], language: browser });
  s.__NOTEBOOK_MOUNT_OPTIONS__ = null;
  if (notebookApi) s.PeerCarryNotebook = { mount: (_root, options) => { s.__NOTEBOOK_MOUNT_OPTIONS__ = options; return notebookApi; } };
  const elements = new Map(), calls = [];
  Object.assign(s.document, {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); },
    createElement: () => new Element(), querySelectorAll: () => [], addEventListener() {},
  });
  Object.assign(s, {
    scrollTo() {}, setTimeout() {}, clearTimeout() {}, setInterval() {}, clearInterval() {},
    localStorage: { getItem: () => null, setItem() {} },
    fetch: async (url, options = {}) => {
      calls.push({ url, options });
      const data = url === '/v1/desktop/settings' ? settingsEnvelope(s) : [];
      return { ok: true, status: 200, json: async () => data, text: async () => '' };
    },
  });
  vm.runInContext(preferencesScript, s);
  vm.runInContext(timeline.match(/<script>([\s\S]*?)<\/script>/)[1], s);
  await new Promise(resolve => setImmediate(resolve));
  return { s, elements, calls, notebookOptions: s.__NOTEBOOK_MOUNT_OPTIONS__ };
}

test('workspace, alerts, settings, accessible labels and dates share the native language', async () => {
  for (const [native, browser, expected] of [
    ['zh', 'en-US', { send: '发送当前剪贴板', alerts: 'AI 任务提醒', notes: '笔记', todo: '待办', clipboard: '设备剪贴板', nav: '主要功能', copyError: '读取最近内容失败', settings: '设置', save: '保存' }],
    ['en', 'zh-CN', { send: 'Send current clipboard', alerts: 'AI task alerts', notes: 'Notes', todo: 'To-dos', clipboard: 'Device clipboard', nav: 'Main', copyError: 'Could not load recent content', settings: 'Settings', save: 'Save' }],
  ]) {
    const { s, elements } = await workspace(native, browser);
    assert.ok(elements.get('remote-send').innerHTML.includes(expected.send));
    assert.equal(elements.get('ai-title').textContent, expected.alerts);
    assert.equal(elements.get('nav-primary').attributes['aria-label'], expected.nav);
    assert.ok(elements.get('tab-notes').innerHTML.includes(expected.notes));
    assert.ok(elements.get('tab-todo').innerHTML.includes(expected.todo));
    assert.ok(elements.get('tab-device-clipboard').innerHTML.includes(expected.clipboard));
    assert.equal(elements.get('tab-notes').attributes['aria-current'], 'page');
    s.showTab('settings');
    assert.equal(elements.get('page-title').textContent, expected.settings);
    assert.equal(s.document.title, 'PeerCarry · ' + expected.settings);
    s.acceptSettingsEnvelope(settingsEnvelope(s)); s.renderSettings();
    assert.equal(elements.get('cfg-save').textContent, expected.save);
    const date = '2000-01-02T12:00:00Z';
    assert.equal(s.dayLabel(date), new Date(date).toLocaleDateString(s.PeerCarryLocale.locale, {
      year: 'numeric', month: 'short', day: 'numeric', weekday: 'short',
    }));
    s.fetch = async () => { throw Error('network'); };
    await s.refresh();
    assert.ok(elements.get('toast').textContent.startsWith(expected.copyError));
    await assert.rejects(s.mobileApi('status'), native === 'zh' ? /连接失败/ : /Could not connect/);
  }
});

test('leaving a notebook view waits for a successful draft flush', async () => {
  let release;
  const pendingFlush = new Promise(resolve => { release = resolve; });
  const { s, elements } = await workspace('en', 'en-US', {
    setSection: async () => true,
    whenIdle: () => pendingFlush,
  });
  const navigation = s.showTab('timeline');
  await Promise.resolve();
  assert.equal(elements.get('notebook-view').hidden, false);
  assert.equal(elements.get('timeline').hidden, true);
  release(true);
  assert.equal(await navigation, true);
  assert.equal(elements.get('notebook-view').hidden, true);
  assert.equal(elements.get('timeline').hidden, false);

  const blocked = await workspace('en', 'en-US', {
    setSection: async () => true,
    whenIdle: async () => false,
  });
  assert.equal(await blocked.s.showTab('settings'), false);
  assert.equal(blocked.elements.get('notebook-view').hidden, false);
  assert.equal(blocked.elements.get('settings').hidden, true);
});

test('desktop system language is passed into the notebook and todo UI', async () => {
  for (const [native, browser, isChinese, locale] of [
    ['zh', 'en-US', true, 'zh-CN'],
    ['en', 'zh-CN', false, 'en'],
  ]) {
    const api = { setSection: async () => true, setVisible() {}, whenIdle: async () => true };
    const result = await workspace(native, browser, api);
    assert.equal(result.notebookOptions.isChinese, isChinese);
    assert.equal(result.notebookOptions.locale, locale);
  }
});

test('notebook settings loads and focuses authentication, then returns after saving', async () => {
  let flushes = 0;
  const sections = [];
  const api = { setSection: async section => { sections.push(section); return true; }, setVisible() {}, whenIdle: async () => { flushes++; return true; } };
  const { s, elements, calls, notebookOptions } = await workspace('zh', 'zh-CN', api);
  assert.equal(await notebookOptions.onOpenSettings(), true);
  assert.equal(flushes, 1);
  assert.equal(calls.filter(call => call.url === '/v1/desktop/settings' && !call.options.method).length, 1);
  assert.equal(elements.get('settings').hidden, false);
  assert.equal(elements.get('cfgform').authInput.focused, true);
  assert.equal(elements.get('cfgform').authInput.scrolled, true);
  assert.equal(elements.get('cfg-return').hidden, false);
  assert.equal(elements.get('cfg-return').textContent, '返回笔记');
  await s.saveConfig(false);
  assert.equal(calls.filter(call => call.url === '/v1/desktop/settings' && call.options.method === 'POST').length, 1);
  await elements.get('cfg-return').onclick();
  assert.equal(elements.get('notebook-view').hidden, false);
  assert.equal(sections.at(-1), 'notes');
});

test('settings entry does not load configuration when a notebook draft cannot be left', async () => {
  const api = { setSection: async () => true, setVisible() {}, whenIdle: async () => false };
  const { elements, calls, notebookOptions } = await workspace('en', 'en-US', api);
  assert.equal(await notebookOptions.onOpenSettings(), false);
  assert.equal(elements.get('notebook-view').hidden, false);
  assert.equal(elements.get('settings').hidden, true);
  assert.equal(calls.some(call => call.url === '/v1/desktop/settings'), false);
  assert.equal(await elements.get('tab-settings').onclick(), false);
  assert.equal(calls.some(call => call.url === '/v1/desktop/settings'), false);
});

test('a failed configuration load cannot save stale settings and still allows returning', async () => {
  const api = { setSection: async () => true, setVisible() {}, whenIdle: async () => true };
  const { s, elements, calls, notebookOptions } = await workspace('en', 'en-US', api);
  s.fetch = async (url, options = {}) => {
    calls.push({ url, options });
    if (url === '/v1/desktop/settings' && !options.method) throw Error('offline');
    return { ok: true, status: 200, json: async () => ({}) };
  };
  await notebookOptions.onOpenSettings();
  assert.equal(elements.get('cfg-save').disabled, true);
  await s.saveConfig(false);
  assert.equal(calls.some(call => call.url === '/v1/desktop/settings' && call.options.method === 'POST'), false);
  await elements.get('cfg-return').onclick();
  assert.equal(elements.get('notebook-view').hidden, false);
});

test('unavailable local settings are explained without sending credentials', async () => {
  const api = { setSection: async () => true, setVisible() {}, whenIdle: async () => true };
  const { s, elements, calls, notebookOptions } = await workspace('en', 'en-US', api);
  s.fetch = async (url, options = {}) => {
    calls.push({ url, options });
    return { ok: false, status: 401, text: async () => '' };
  };
  await notebookOptions.onOpenSettings();
  assert.match(elements.get('cfg-status').textContent, /could not load local settings/i);
  assert.equal(elements.get('cfg-save').disabled, true);
  assert.equal(calls.some(call => call.options.headers?.['x-peercarry-token']), false);
});

test('returning from settings restores the todo section', async () => {
  const sections = [];
  const api = { setSection: async section => { sections.push(section); return true; }, setVisible() {}, whenIdle: async () => true };
  const { s, elements, notebookOptions } = await workspace('en', 'en-US', api);
  await s.showTab('todo');
  await notebookOptions.onOpenSettings();
  assert.equal(elements.get('cfg-return').textContent, 'Return to to-dos');
  await elements.get('cfg-return').onclick();
  assert.equal(sections.at(-1), 'todo');
});

test('timeline copy reports failure and ignores a duplicate request', async () => {
  const { s, elements, calls } = await workspace('en', 'en-US');
  vm.runInContext("entries = [{ id: 'synthetic-entry' }]", s);
  let release;
  s.fetch = async (url, options = {}) => {
    calls.push({ url, options });
    return new Promise(resolve => { release = () => resolve({ ok: false, status: 500 }); });
  };
  const first = s.apply(0);
  await s.apply(0);
  assert.equal(calls.filter(call => call.url === '/v1/actions/apply').length, 1);
  release(); await first;
  assert.equal(elements.get('toast').textContent, 'Could not copy. Try again.');
});
