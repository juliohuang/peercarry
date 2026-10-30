const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../crates/peercarry-core/ui-locale.js'), 'utf8');

function page(bootstrap, native = 'zh', browser = 'zh-CN') {
  const events = {}, calls = [], writes = [], messages = [];
  let reloads = 0;
  const s = {
    __PEERCARRY_LOCALE__: bootstrap, __PEERCARRY_LANG__: native,
    navigator: { language: browser, languages: [browser] },
    document: { documentElement: {}, addEventListener: (name, fn) => { events['document:' + name] = fn; } },
    addEventListener: (name, fn) => { events[name] = fn; },
    localStorage: { setItem: (...args) => writes.push(args) },
    ipc: { postMessage: value => messages.push(value) },
    location: { reload: () => { reloads++; } },
    AbortController, setTimeout, clearTimeout,
    fetch: async (url, options) => { calls.push({ url, options }); return { ok: true, json: async () => bootstrap }; },
  };
  vm.runInNewContext(source, s);
  return { s, locale: s.PeerCarryLocale, events, calls, writes, messages, reloads: () => reloads };
}

test('saved language wins, system language uses Chinese or English fallback independently of browser and stale native injection', () => {
  for (const [preference, system_language, expected] of [
    ['system', 'zh', 'zh'], ['system', 'en', 'en'], ['system', 'ja-JP', 'en'], ['system', '', 'en'],
    ['system', undefined, 'en'], ['zh', 'en', 'zh'], ['en', 'zh', 'en'],
  ]) {
    const f = page({ preference, system_language });
    assert.equal(f.locale.language, expected, `${preference} ${system_language}`);
    assert.equal(f.s.document.documentElement.lang, expected === 'zh' ? 'zh-CN' : 'en');
    assert.equal(Object.isFrozen(f.locale), true);
  }
});

test('a blocked reload keeps the page and retries after draft or operation completion', async () => {
  const f = page({ preference: 'en', system_language: 'zh', language: 'en' });
  let ready = false;
  await f.locale.beforeReload(() => ready);
  f.s.fetch = async () => ({ ok: true, json: async () => ({ preference: 'zh', system_language: 'zh', language: 'zh' }) });
  assert.equal(await f.locale.checkForChange(), false);
  assert.equal(f.reloads(), 0);
  ready = true;
  assert.equal(await f.locale.retry(), true);
  assert.equal(f.reloads(), 1);
  await f.locale.checkForChange();
  assert.equal(f.reloads(), 1);
});

test('a failed check or reload can be retried without treating the failure as a language change', async () => {
  const f = page({ preference: 'en', system_language: 'en', language: 'en' });
  await f.locale.beforeReload(() => true);
  for (const response of [{ ok: false }, { ok: true, json: async () => ({ language: 'unsupported' }) }]) {
    f.s.fetch = async () => response;
    assert.equal(await f.locale.checkForChange(), false);
    assert.equal(f.reloads(), 0);
  }
  f.s.fetch = async () => ({ ok: true, json: async () => ({ language: 'zh' }) });
  f.s.location.reload = () => { throw Error('synthetic navigation failure'); };
  assert.equal(await f.locale.checkForChange(), false);
  let restored = false;
  f.s.location.reload = () => { restored = true; };
  assert.equal(await f.locale.retry(), true);
  assert.equal(restored, true);
});

test('concurrent notifications share one read-only metadata request and emit no private content', async () => {
  const f = page({ preference: 'en', system_language: 'en', language: 'en' });
  await f.locale.beforeReload(() => true);
  let release;
  f.s.fetch = async (url, options) => {
    f.calls.push({ url, options });
    return new Promise(resolve => { release = () => resolve({ ok: true, json: async () => ({ language: 'en' }) }); });
  };
  const first = f.locale.notifyChange();
  await f.locale.checkForChange();
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].url, '/v1/desktop/language');
  assert.equal(f.calls[0].options.cache, 'no-store');
  assert.equal(f.calls[0].options.redirect, 'error');
  assert.equal(f.calls[0].options.method, undefined);
  assert.equal(f.calls[0].options.body, undefined);
  assert.equal(f.writes[0][0], 'peercarry.ui.language.changed');
  assert.deepEqual(f.messages, ['language-changed']);
  release(); await first;
  assert.equal(f.reloads(), 0);
});

test('IME composition blocks reload even before an editor has a dirty draft and resumes only after composition ends', async () => {
  const f = page({ preference: 'en', system_language: 'en', language: 'en' });
  await f.locale.beforeReload(() => true);
  f.s.fetch = async () => ({ ok: true, json: async () => ({ language: 'zh' }) });
  f.events['document:compositionstart']();
  assert.equal(await f.locale.checkForChange(), false);
  assert.equal(f.reloads(), 0);
  f.events['document:compositionend']();
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(f.reloads(), 1);
});
