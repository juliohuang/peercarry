const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const core = path.join(__dirname, '../../crates/peercarry-core');
const timeline = fs.readFileSync(path.join(core, 'timeline.html'), 'utf8');
const locale = fs.readFileSync(path.join(core, 'ui-locale.js'), 'utf8');
const preferences = fs.readFileSync(path.join(core, 'device-preferences.js'), 'utf8');

class Element {
  constructor(id = '') {
    this.id = id; this.value = ''; this.checked = false; this.style = {}; this.hidden = false;
    this.disabled = false; this.dataset = {}; this.attributes = {}; this.listeners = {};
    this.classList = { add() {}, remove() {}, toggle() {} }; this.children = []; this._html = '';
    this.fields = []; this.secretOps = {}; this.appRows = [];
  }
  set innerHTML(value) {
    this._html = value;
    if (this.id === 'cfgform') {
      this.fields = [...value.matchAll(/<input\b[^>]*data-path="([^"]+)"[^>]*>/g)].map(match => {
        const html = match[0], field = new Element();
        field.dataset.path = match[1];
        field.type = (html.match(/type="([^"]+)"/) || [,'text'])[1];
        field.value = (html.match(/\bvalue="([^"]*)"/) || [,''])[1];
        field.checked = /\schecked(?:\s|>)/.test(html);
        field.disabled = /\sdisabled(?:\s|>)/.test(html);
        return field;
      });
      for (const match of value.matchAll(/<select\b[^>]*data-path="([^"]+)"[^>]*>([\s\S]*?)<\/select>/g)) {
        const field = new Element();
        field.dataset.path = match[1]; field.type = 'select-one';
        field.value = (match[2].match(/<option value="([^"]+)" selected/) || [,''])[1];
        this.fields.push(field);
      }
      this.secretOps = {};
      for (const match of value.matchAll(/<select data-secret-op="([^"]+)"/g)) {
        const control = new Element(); control.value = 'keep'; control.dataset.secretOp = match[1];
        this.secretOps[match[1]] = control;
      }
    }
    if (this.id === 'apps-rows') {
      this.appRows = [...value.matchAll(/<div class="row" data-app="(\d+)">([\s\S]*?)<\/div>/g)].map(match => {
        const row = new Element(); row.dataset.app = match[1]; row.appFields = {};
        for (const field of match[2].matchAll(/<input\b[^>]*data-app-field="([^"]+)"[^>]*>/g)) {
          const input = new Element(); input.dataset.appField = field[1];
          input.value = (field[0].match(/\bvalue="([^"]*)"/) || [,''])[1];
          row.appFields[field[1]] = input;
        }
        row.querySelector = selector => row.appFields[(selector.match(/data-app-field=([^\]]+)/) || [])[1]];
        return row;
      });
    }
  }
  get innerHTML() { return this._html; }
  setAttribute(k, v) { this.attributes[k] = v; }
  removeAttribute(k) { delete this.attributes[k]; }
  addEventListener(name, fn) { (this.listeners[name] ||= []).push(fn); }
  emit(name, event) { for (const fn of this.listeners[name] || []) fn(event); }
  querySelector(selector) {
    const path = (selector.match(/data-path="([^"]+)"/) || [])[1];
    if (path) return this.fields.find(f => f.dataset.path === path) || null;
    const secret = (selector.match(/data-secret-op="([^"]+)"/) || [])[1];
    return secret ? this.secretOps[secret] || null : new Element();
  }
  querySelectorAll() { return []; }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  focus() { this.focused = true; }
  scrollIntoView() {}
}

const optional = new Set(['node.name', 'network.bind', 'storage.download_dir', 'storage.data_dir']);
function envelope(s, revision = 'r1', secret = true) {
  const values = { apps: [{ name: 'Example', path: '/synthetic/app', args: ['a,b'] }] };
  for (const field of vm.runInContext('FIELDS', s)) {
    if (!field.path || field.type === 'password') continue;
    values[field.path] = field.type === 'language' ? 'system' : field.type === 'bool' ? false : field.type === 'number' ? 1 : optional.has(field.path) ? null : '';
  }
  return { schema: 1, revision, instance_id: 'instance-before', desktop_url: 'http://127.0.0.1:5199/',
    restart_recommended: false, values, secrets: { 'network.auth_token': secret, ai_token: false } };
}
const ok = body => ({ ok: true, status: 200, json: async () => body });
const fail = code => ({ ok: false, status: code, json: async () => ({ error: 'synthetic' }) });

async function fixture({ zh = false, notebook } = {}) {
  const elements = new Map(), calls = [], storageWrites = [], timers = new Map();
  let timerId = 0;
  const element = id => { if (!elements.has(id)) elements.set(id, new Element(id)); return elements.get(id); };
  const s = { __PEERCARRY_LANG__: zh ? 'zh' : 'en', navigator: { language: 'en-US' },
    document: { documentElement: {}, getElementById: element, createElement: () => new Element(),
      addEventListener() {}, querySelectorAll(selector) {
        if (selector === '#cfgform [data-path]') return element('cfgform').fields;
        if (selector === '#apps-rows [data-app]') return element('apps-rows').appRows;
        return [];
      } },
    URL, AbortController, location: { href: 'http://127.0.0.1:5199/', origin: 'http://127.0.0.1:5199' },
    scrollTo() {}, setTimeout(fn, delay) { const id = ++timerId; timers.set(id, { fn, delay }); return id; },
    clearTimeout(id) { timers.delete(id); }, setInterval() {}, clearInterval() {},
    localStorage: { getItem: () => null, setItem: (...args) => storageWrites.push(args) },
  };
  s.window = s;
  if (notebook) s.PeerCarryNotebook = { mount: (_root, options) => { s.notebookOptions = options; return notebook; } };
  s.fetch = async (url, init = {}) => { calls.push({ url, init }); return url === '/v1/desktop/settings' ? ok(envelope(s)) : ok([]); };
  vm.createContext(s); vm.runInContext(locale, s); vm.runInContext(preferences, s);
  vm.runInContext(timeline.match(/<script>([\s\S]*?)<\/script>/)[1], s);
  await new Promise(resolve => setImmediate(resolve));
  return { s, element, calls, storageWrites, timers, async tick(delay = 500) {
    const timer = [...timers].find(([, item]) => item.delay === delay);
    assert.ok(timer, 'expected a scheduled reconnect check'); timers.delete(timer[0]);
    await timer[1].fn();
  } };
}
const control = (f, path) => f.element('cfgform').fields.find(item => item.dataset.path === path);
const op = (f, path, value) => {
  const select = f.element('cfgform').secretOps[path]; select.value = value;
  f.element('cfgform').emit('change', { target: select });
};

test('language saves only the selected preference and reloads after an accepted save', async () => {
  const f = await fixture();
  await f.s.openSettings();
  assert.match(f.element('cfgform').innerHTML, /Follow system/);
  assert.match(f.element('cfgform').innerHTML, /简体中文/);
  assert.equal(control(f, 'ui.language').value, 'system');
  let reloads = 0;
  f.s.location.reload = () => { reloads++; };
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    if (url === '/v1/desktop/language') return ok({ preference: 'zh', system_language: 'en', language: 'zh' });
    const saved = envelope(f.s, 'language-saved');
    saved.values['ui.language'] = 'zh';
    return ok(saved);
  };
  control(f, 'ui.language').value = 'zh';
  assert.equal(reloads, 0);
  await f.s.saveConfig(false);
  const submitted = f.calls.find(call => call.url === '/v1/desktop/settings' && call.init.method === 'POST');
  assert.deepEqual(JSON.parse(submitted.init.body).changes, { 'ui.language': 'zh' });
  assert.deepEqual(JSON.parse(submitted.init.body).secret_changes, {});
  assert.equal(reloads, 1);
  assert.equal(f.storageWrites.at(-1)[0], 'peercarry.ui.language.changed');
});

test('failed language save and a language notification never discard a settings draft', async () => {
  const f = await fixture(); await f.s.openSettings();
  let reloads = 0;
  f.s.location.reload = () => { reloads++; };
  control(f, 'ui.language').value = 'zh';
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return url === '/v1/desktop/language' ? ok({ language: 'zh' }) : fail(409);
  };
  await f.s.saveConfig(false);
  assert.equal(control(f, 'ui.language').value, 'zh');
  assert.equal(reloads, 0);
  assert.equal(f.storageWrites.length, 0);
  assert.equal(await f.s.PeerCarryLocale.checkForChange(), false);
  assert.equal(control(f, 'ui.language').value, 'zh');
  assert.equal(reloads, 0);
});

test('discarding a settings draft resumes a pending language change after leaving safely', async () => {
  const f = await fixture(); await f.s.openSettings();
  let reloads = 0;
  f.s.location.reload = () => { reloads++; };
  control(f, 'node.name').value = 'Unsaved name';
  f.s.fetch = async url => url === '/v1/desktop/language' ? ok({ language: 'zh' }) : ok([]);
  assert.equal(await f.s.PeerCarryLocale.checkForChange(), false);
  assert.equal(await f.s.showTab('notes'), false);
  assert.equal(reloads, 0);
  await f.element('cfg-discard').onclick();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(reloads, 1);
  assert.equal(f.element('cfgform').innerHTML, '');
});

test('unsaved public fields, secrets and apps stay in settings until explicitly discarded', async () => {
  for (const kind of ['public', 'secret', 'apps']) {
    const f = await fixture(); await f.s.openSettings();
    if (kind === 'public') control(f, 'node.name').value = 'Retained name';
    if (kind === 'secret') { op(f, 'network.auth_token', 'replace'); control(f, 'network.auth_token').value = 'R'.repeat(32); }
    if (kind === 'apps') {
      const name = f.element('apps-rows').appRows[0].appFields.name;
      name.value = 'Retained application'; f.element('apps-rows').emit('input', { target: name });
    }
    assert.equal(await f.s.showTab('notes'), false, kind);
    assert.equal(f.element('settings').hidden, false);
    assert.equal(f.element('cfg-leave').hidden, false);
    await f.element('cfg-stay').onclick();
    assert.equal(f.element('cfg-leave').hidden, true);
    if (kind === 'public') assert.equal(control(f, 'node.name').value, 'Retained name');
    if (kind === 'secret') assert.equal(control(f, 'network.auth_token').value, 'R'.repeat(32));
    if (kind === 'apps') assert.equal(f.element('apps-rows').appRows[0].appFields.name.value, 'Retained application');
    assert.equal(await f.s.showTab('notes'), false);
    assert.equal(await f.element('cfg-discard').onclick(), true);
    assert.equal(f.element('settings').hidden, true);
    assert.equal(f.element('cfgform').innerHTML.includes('R'.repeat(32)), false);
    await f.s.openSettings();
    assert.equal(control(f, 'node.name').value, '');
    assert.equal(control(f, 'network.auth_token').value, '');
  }
});

test('a pending settings save cannot be abandoned by navigation', async () => {
  const f = await fixture(); await f.s.openSettings();
  control(f, 'node.name').value = 'Pending form';
  let release;
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return new Promise(resolve => { release = () => resolve(fail(400)); });
  };
  const pending = f.s.saveConfig(false);
  assert.equal(await f.s.showTab('notes'), false);
  assert.equal(f.element('settings').hidden, false);
  assert.equal(control(f, 'node.name').value, 'Pending form');
  release(); await pending;
  assert.equal(control(f, 'node.name').value, 'Pending form');
  assert.equal(f.element('cfgform').inert, false);
});

test('the first failed settings GET provides a labelled reload action in both languages', async () => {
  for (const zh of [false, true]) {
    const f = await fixture({ zh });
    f.s.fetch = async () => fail(500);
    await f.s.openSettings();
    assert.equal(f.element('cfg-reload').hidden, false);
    assert.equal(f.element('cfg-reload').textContent, zh ? '重新加载' : 'Reload');
  }
});

test('redacted configured settings load and submit only changed public fields', async () => {
  const f = await fixture();
  await f.s.openSettings();
  assert.match(f.element('cfgform').innerHTML, /Configured/);
  assert.equal(control(f, 'network.auth_token').value, '');
  assert.equal(control(f, 'network.auth_token').disabled, true);
  control(f, 'node.name').value = 'Synthetic computer';
  await f.s.saveConfig(false);
  const request = f.calls.find(c => c.url === '/v1/desktop/settings' && c.init.method === 'POST');
  const body = JSON.parse(request.init.body);
  assert.deepEqual(body, { revision: 'r1', changes: { 'node.name': 'Synthetic computer' }, secret_changes: {} });
  assert.equal(f.calls.some(c => c.url === '/v1/config'), false);
  assert.equal(f.storageWrites.length, 0);
});

test('unexpected hidden and secret fields never enter the rendered form or ordinary patch', async () => {
  const f = await fixture();
  const data = envelope(f.s);
  data.values['network.auth_token'] = 'unexpected-synthetic-secret';
  data.values['mobile.token'] = 'unexpected-mobile-secret';
  f.s.fetch = async (url, init = {}) => { f.calls.push({ url, init }); return ok(data); };
  await f.s.openSettings();
  assert.equal(f.element('cfgform').innerHTML.includes('unexpected-synthetic-secret'), false);
  assert.equal(vm.runInContext('JSON.stringify(cfg)', f.s).includes('unexpected-mobile-secret'), false);
  await f.s.saveConfig(false);
  const body = JSON.parse(f.calls.find(c => c.init.method === 'POST').init.body);
  assert.deepEqual(body.changes, {});
  assert.deepEqual(body.secret_changes, {});
});

test('secret keep, replace and clear are explicit; successful save clears the entered value', async () => {
  const f = await fixture({ zh: true });
  await f.s.openSettings();
  assert.match(f.element('cfgform').innerHTML, /已配置/);
  op(f, 'network.auth_token', 'replace');
  assert.equal(control(f, 'network.auth_token').disabled, false);
  control(f, 'network.auth_token').value = 'S'.repeat(32);
  op(f, 'ai_token', 'clear');
  await f.s.saveConfig(false);
  const body = JSON.parse(f.calls.find(c => c.init.method === 'POST').init.body);
  assert.equal(body.secret_changes['network.auth_token'].op, 'replace');
  assert.equal(body.secret_changes['network.auth_token'].value.length, 32);
  assert.deepEqual(body.secret_changes.ai_token, { op: 'clear' });
  assert.equal(control(f, 'network.auth_token').value, '');
  assert.equal(f.element('cfgform').innerHTML.includes('S'.repeat(32)), false);
  assert.equal(f.storageWrites.length, 0);
});

test('invalid replacement does not send; empty or untouched secrets stay unchanged', async () => {
  const f = await fixture(); await f.s.openSettings();
  op(f, 'network.auth_token', 'replace'); control(f, 'network.auth_token').value = 'short';
  await f.s.saveConfig(true);
  assert.equal(f.calls.some(c => c.init.method === 'POST'), false);
  op(f, 'network.auth_token', 'keep');
  await f.s.saveConfig(false);
  const body = JSON.parse(f.calls.find(c => c.init.method === 'POST').init.body);
  assert.deepEqual(body.secret_changes, {});
});

test('first-time unset secrets remain unset when no secret operation is selected', async () => {
  const f = await fixture();
  f.s.fetch = async (url, init = {}) => { f.calls.push({ url, init }); return ok(envelope(f.s, 'r1', false)); };
  await f.s.openSettings();
  assert.match(f.element('cfgform').innerHTML, /Not configured/);
  await f.s.saveConfig(false);
  const body = JSON.parse(f.calls.find(c => c.init.method === 'POST').init.body);
  assert.deepEqual(body.secret_changes, {});
});

test('a pending save blocks duplicate requests and restores the editable form on validation failure', async () => {
  const f = await fixture(); await f.s.openSettings();
  let release;
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return new Promise(resolve => { release = () => resolve(fail(400)); });
  };
  control(f, 'node.name').value = 'Retained';
  const pending = f.s.saveConfig(false);
  assert.equal(f.element('cfgform').inert, true);
  await f.s.saveConfig(false);
  assert.equal(f.calls.filter(c => c.init.method === 'POST').length, 1);
  release(); await pending;
  assert.equal(f.element('cfgform').inert, false);
  assert.equal(control(f, 'node.name').value, 'Retained');
});

test('conflict keeps the edited form, disables save and offers explicit reload', async () => {
  const f = await fixture(); await f.s.openSettings();
  control(f, 'node.name').value = 'Unsaved name';
  op(f, 'network.auth_token', 'replace'); control(f, 'network.auth_token').value = 'R'.repeat(32);
  f.s.fetch = async (url, init = {}) => { f.calls.push({ url, init }); return init.method === 'POST' ? fail(409) : ok(envelope(f.s, 'r2')); };
  await f.s.saveConfig(true);
  assert.equal(control(f, 'node.name').value, 'Unsaved name');
  assert.equal(control(f, 'network.auth_token').value.length, 32);
  assert.equal(f.element('cfg-save').disabled, true);
  assert.equal(f.element('cfg-reload').hidden, false);
  assert.equal(f.calls.some(c => c.url.endsWith('/restart')), false);
  const before = f.calls.length;
  await f.s.openSettings();
  assert.equal(f.calls.length, before);
  assert.equal(control(f, 'node.name').value, 'Unsaved name');
  await f.element('cfg-reload').onclick();
  assert.equal(f.element('cfg-save').disabled, false);
  assert.equal(control(f, 'network.auth_token').value, '');
});

test('failed or unavailable save preserves input and never restarts', async () => {
  for (const mode of ['401', '404', '500', 'network']) {
    const f = await fixture(); await f.s.openSettings();
    control(f, 'node.name').value = 'Still here';
    f.s.fetch = async (url, init = {}) => {
      f.calls.push({ url, init });
      if (mode === 'network') throw Error('synthetic offline');
      return fail(Number(mode));
    };
    await f.s.saveConfig(true);
    assert.equal(control(f, 'node.name').value, 'Still here');
    assert.equal(f.element('cfg-save').disabled, true);
    assert.equal(f.calls.some(c => c.url.endsWith('/restart')), false);
  }
});

test('explicit restart follows successful save with the new revision and remains unconfirmed', async () => {
  const f = await fixture(); await f.s.openSettings();
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return url.endsWith('/restart') ? ok({}) : ok({ ...envelope(f.s, 'r2'), restart_recommended: true });
  };
  await f.s.saveConfig(true);
  const posts = f.calls.filter(c => c.init.method === 'POST');
  assert.deepEqual(posts.map(c => c.url), ['/v1/desktop/settings', '/v1/desktop/settings/restart']);
  assert.deepEqual(JSON.parse(posts[1].init.body), { revision: 'r2' });
  assert.match(f.element('cfg-status').textContent, /not yet confirmed/);
  assert.equal(f.calls.some(c => c.url === '/v1/hello'), false);
});

test('a restart rejection or lost connection never reports settings live', async () => {
  for (const lost of [false, true]) {
    const f = await fixture(); await f.s.openSettings();
    f.s.fetch = async (url, init = {}) => {
      f.calls.push({ url, init });
      if (url.endsWith('/restart')) {
        if (lost) throw Error('synthetic connection closed');
        return fail(409);
      }
      return ok({ ...envelope(f.s, 'r2'), restart_recommended: true });
    };
    await f.s.saveConfig(true);
    assert.equal(f.element('cfg-reload').hidden, false);
    assert.equal(f.element('cfg-save').disabled, true);
    assert.match(f.element('cfg-status').textContent, lost ? /unconfirmed/ : /restart request failed/);
    assert.doesNotMatch(f.element('cfg-status').textContent, /config is live/i);
  }
});

test('restart recovery requires a different process, then refreshes settings and revision', async () => {
  const f = await fixture(); await f.s.openSettings();
  let current = envelope(f.s, 'changed-on-disk');
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    if (url.endsWith('/restart')) return ok({ ok: true });
    return ok(init.method === 'POST' ? envelope(f.s, 'saved') : current);
  };
  await f.s.saveConfig(true);
  await f.tick();
  assert.equal(f.element('cfg-save').disabled, true, 'same process must remain unconfirmed');
  current = { ...envelope(f.s, 'new-process-revision'), instance_id: 'instance-after' };
  current.values['node.name'] = 'Loaded by replacement';
  await f.tick();
  assert.equal(control(f, 'node.name').value, 'Loaded by replacement');
  assert.equal(vm.runInContext('configRevision', f.s), 'new-process-revision');
  assert.equal(f.element('cfg-save').disabled, false);
  assert.match(f.element('cfg-status').textContent, /reconnected/i);
  assert.equal(f.calls.some(c => c.url === '/v1/hello'), false);
});

test('a replacement with further pending changes does not claim all settings applied', async () => {
  const f = await fixture({ zh: true }); await f.s.openSettings();
  f.s.fetch = async (url, init = {}) => url.endsWith('/restart') ? ok({ ok: true }) : ok({
    ...envelope(f.s, 'next'), instance_id: init.method === 'POST' ? 'instance-before' : 'instance-after',
    restart_recommended: true,
  });
  await f.s.saveConfig(true); await f.tick();
  assert.match(f.element('cfg-status').textContent, /仍有.*重启/);
  assert.doesNotMatch(f.element('cfg-status').textContent, /配置已生效/);
  assert.equal(f.element('cfg-save').disabled, false);
});

test('localhost pages can probe their own origin when the reconnect candidate names a loopback alias', async () => {
  const f = await fixture(); f.s.location.origin = 'http://localhost:5199';
  await f.s.openSettings();
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return url.endsWith('/restart') ? ok({ ok: true }) : ok({
      ...envelope(f.s, 'next'), instance_id: init.method === 'POST' ? 'instance-before' : 'instance-after',
    });
  };
  await f.s.saveConfig(true); await f.tick();
  assert.match(f.element('cfg-status').textContent, /reconnected/i);
  assert.equal(f.calls.some(c => /^https?:/.test(c.url)), false);
});

test('a lost restart response or already-pending request can recover without a second POST', async () => {
  for (const mode of ['lost', 'pending']) {
    const f = await fixture(); await f.s.openSettings();
    f.s.fetch = async (url, init = {}) => {
      f.calls.push({ url, init });
      if (url.endsWith('/restart')) {
        if (mode === 'lost') throw Error('lost acknowledgement');
        return { ok: false, status: 409, json: async () => ({ error: 'restart_pending' }) };
      }
      return ok({ ...envelope(f.s, 'next'), instance_id: init.method === 'POST' ? 'instance-before' : 'instance-after' });
    };
    await f.s.saveConfig(true); await f.tick();
    assert.match(f.element('cfg-status').textContent, /reconnected/i);
    assert.equal(f.calls.filter(c => c.url.endsWith('/restart')).length, 1);
  }
});

test('changed addresses offer a local link and never trigger cross-origin probing', async () => {
  const f = await fixture(); await f.s.openSettings();
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    return url.endsWith('/restart') ? ok({ ok: true }) : ok({ ...envelope(f.s, 'saved'), desktop_url: 'http://127.0.0.1:6200/' });
  };
  await f.s.saveConfig(true);
  assert.equal(f.element('cfg-reconnect').hidden, false);
  assert.equal(f.element('cfg-reconnect').href, 'http://127.0.0.1:6200/');
  assert.equal(f.element('cfg-reconnect').rel, 'noopener noreferrer');
  assert.equal([...f.timers.values()].some(t => t.delay === 500), false);
  assert.equal(f.calls.some(c => /^https?:/.test(c.url)), false);
  assert.equal(f.element('cfg-save').disabled, true);
});

test('reconnect links reject remote hosts, credentials, scripts and extra URL state', async () => {
  const f = await fixture();
  for (const unsafe of ['https://127.0.0.1:5199/', 'http://example.com/', 'javascript:alert(1)',
    'http://user:password@127.0.0.1/', 'http://127.0.0.1/?token=secret', 'http://127.0.0.1/#secret',
    'http://127.0.0.1/v1/actions/restart']) {
    assert.equal(f.s.localReconnectUrl(unsafe), null, unsafe);
  }
  assert.equal(f.s.localReconnectUrl('http://[::1]:6200/').href, 'http://[::1]:6200/');
});

test('navigation and explicit reload cancel late recovery responses without overwriting a draft', async () => {
  for (const action of ['leave', 'reload']) {
    const f = await fixture(); await f.s.openSettings();
    let release, recovering = false;
    f.s.fetch = async (url, init = {}) => {
      if (url.endsWith('/restart')) return ok({ ok: true });
      if (init.method === 'POST') return ok(envelope(f.s, 'saved'));
      if (!recovering) {
        recovering = true;
        return { ok: true, json: () => new Promise(resolve => { release = resolve; }) };
      }
      return ok(envelope(f.s, 'manual-reload'));
    };
    await f.s.saveConfig(true);
    const check = f.tick(); await new Promise(resolve => setImmediate(resolve));
    if (action === 'leave') { await f.s.showTab('notes'); await f.s.openSettings(); }
    else await f.s.loadConfig();
    control(f, 'node.name').value = 'Keep current draft';
    release({ ...envelope(f.s, 'late'), instance_id: 'instance-after' });
    await check;
    assert.equal(control(f, 'node.name').value, 'Keep current draft');
    assert.equal(vm.runInContext('configRevision', f.s), 'manual-reload');
  }
});

test('restart checks stop after a bounded number of attempts and retain manual recovery', async () => {
  const f = await fixture(); await f.s.openSettings();
  f.s.fetch = async (url, init = {}) => {
    if (url.endsWith('/restart')) return ok({ ok: true });
    return init.method === 'POST' ? ok(envelope(f.s, 'saved')) : fail(503);
  };
  await f.s.saveConfig(true);
  for (let i = 0; i < 20; i++) await f.tick();
  assert.equal([...f.timers.values()].some(t => t.delay === 500), false);
  assert.match(f.element('cfg-status').textContent, /unconfirmed/i);
  assert.equal(f.element('cfg-reload').hidden, false);
  assert.equal(f.element('cfg-reconnect').hidden, false);
});

test('navigation waits for a save and keeps its acknowledged revision before reopening', async () => {
  const f = await fixture(); await f.s.openSettings();
  let release;
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    if (init.method === 'POST') return new Promise(resolve => {
      release = () => {
        const data = envelope(f.s, 'saved'); data.values['node.name'] = 'Old form';
        resolve(ok(data));
      };
    });
    return ok(envelope(f.s, 'new'));
  };
  control(f, 'node.name').value = 'Old form';
  const pending = f.s.saveConfig(false);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(await f.s.showTab('timeline'), false);
  release(); await pending;
  assert.equal(control(f, 'node.name').value, 'Old form');
  assert.equal(vm.runInContext('configRevision', f.s), 'saved');
  assert.equal(await f.s.showTab('timeline'), true);
  await f.s.openSettings();
  assert.equal(vm.runInContext('configRevision', f.s), 'new');
  assert.equal(f.calls.some(c => c.url.endsWith('/restart')), false);
});

test('a late settings GET cannot replace a later settings context', async () => {
  const f = await fixture();
  let release, count = 0;
  f.s.fetch = async (url, init = {}) => {
    f.calls.push({ url, init });
    if (url !== '/v1/desktop/settings') return ok([]);
    if (++count === 1) return new Promise(resolve => { release = () => resolve(ok(envelope(f.s, 'old'))); });
    return ok(envelope(f.s, 'new'));
  };
  const first = f.s.openSettings();
  await new Promise(resolve => setImmediate(resolve));
  await f.s.showTab('timeline');
  await f.s.openSettings();
  control(f, 'node.name').value = 'New form';
  release(); await first;
  assert.equal(vm.runInContext('configRevision', f.s), 'new');
  assert.equal(control(f, 'node.name').value, 'New form');
});

test('editing apps is required before apps are included in the patch', async () => {
  const f = await fixture(); await f.s.openSettings();
  await f.s.saveConfig(false);
  let body = JSON.parse(f.calls.filter(c => c.init.method === 'POST').at(-1).init.body);
  assert.equal(Object.hasOwn(body.changes, 'apps'), false);
  const row = f.element('apps-rows').appRows[0];
  row.appFields.name.value = 'Renamed';
  f.element('apps-rows').emit('input', { target: row.appFields.name });
  await f.s.saveConfig(false);
  body = JSON.parse(f.calls.filter(c => c.init.method === 'POST').at(-1).init.body);
  assert.equal(body.changes.apps[0].name, 'Renamed');
  assert.deepEqual(body.changes.apps[0].args, ['a,b']);
});
