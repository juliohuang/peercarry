const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const notebook = require('../../crates/peercarry-core/notebook.js');

class Element {
  constructor(tag, owner) {
    this.tagName = tag.toUpperCase(); this.ownerDocument = owner; this.children = []; this.attributes = {};
    this.dataset = {}; this.listeners = {}; this.parentNode = null; this.className = ''; this.textContent = '';
    this.value = ''; this.checked = false; this.hidden = false; this.disabled = false; this.type = ''; this.href = '';
  }
  get classList() { return { toggle: (name, value) => { const set = new Set(this.className.split(/\s+/).filter(Boolean)); if (value) set.add(name); else set.delete(name); this.className = [...set].join(' '); } }; }
  append(...nodes) { for (const node of nodes) { if (!node || typeof node !== 'object') continue; node.parentNode = this; this.children.push(node); } }
  replaceChildren(...nodes) { this.children.forEach(n => n.parentNode = null); this.children = []; this.append(...nodes); }
  addEventListener(name, fn) { (this.listeners[name] ||= []).push(fn); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name]; }
  click() { if (!this.disabled) { this.clicked = true; for (const fn of this.listeners.click || []) fn({ target: this }); if (this.onclick) this.onclick({ target: this }); } }
  focus() { this.ownerDocument.activeElement = this; }
  async dispatch(name, event = {}) { const e = { target: this, ...event }; for (const fn of this.listeners[name] || []) await fn(e); if (this['on' + name]) await this['on' + name](e); }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter(n => n !== this); }
  contains(node) { for (let current = node; current; current = current.parentNode) if (current === this) return true; return false; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  querySelectorAll(selector) {
    const found = []; const visit = n => { for (const c of n.children) { if (selector[0] === '.' && c.className.split(/\s+/).includes(selector.slice(1))) found.push(c); visit(c); } }; visit(this); return found;
  }
}
class Document {
  constructor() { this.created = []; this.listeners = {}; }
  createElement(tag) { const e = new Element(tag, this); this.created.push(e); return e; }
  createTextNode(text) { const e = new Element('#text', this); e.textContent = String(text); this.created.push(e); return e; }
  addEventListener(name, fn) { (this.listeners[name] ||= []).push(fn); }
  dispatch(name, event) { for (const fn of this.listeners[name] || []) fn(event); }
}
function walk(node, predicate) { for (const child of node.children) { if (predicate(child)) return child; const found = walk(child, predicate); if (found) return found; } return null; }
function all(node, predicate, output = []) { for (const child of node.children) { if (predicate(child)) output.push(child); all(child, predicate, output); } return output; }
const byClass = (root, cls) => root.querySelector('.' + cls);
const buttonNamed = (root, label) => walk(root, n => n.tagName === 'BUTTON' && n.textContent === label);

async function fixture({ items = [], peers = [], entries = [], onRequest, isChinese = false, storage, autoLoad = true } = {}) {
  const doc = new Document(), root = new Element('main', doc); root.id = 'notebook-view';
  if (storage) doc.defaultView = { localStorage: storage };
  const calls = []; let revision = 1;
  const fetchImpl = async (url, init = {}) => {
    const body = init.body ? JSON.parse(init.body) : undefined; calls.push({ url, method: init.method || 'GET', body });
    if (onRequest) { const handled = await onRequest(url, init, body, calls); if (handled) return handled; }
    let data = {};
    if (url.startsWith('/v1/notebook?')) { const kind = new URL('http://localhost' + url).searchParams.get('kind'); data = { items: items.filter(i => !kind || i.kind === kind) }; }
    else if (url === '/v1/peers') data = peers;
    else if (url.startsWith('/v1/timeline')) data = entries;
    else if (url === '/v1/notebook' && init.method === 'POST') data = { id: 'new-note', kind: body.kind, title: body.title, markdown: body.markdown, tasks: body.tasks, pinned: false, trashed: false, revision, created_at: 'now', updated_at: 'now' };
    else if (/^\/v1\/notebook\/[^/]+$/.test(url) && init.method === 'GET') { const itemId = decodeURIComponent(url.split('/').pop()); data = { ...(items.find(i => i.id === itemId) || items[0]), revision }; }
    else if (/^\/v1\/notebook\/[^/]+$/.test(url) && init.method === 'PUT') data = { revision: ++revision, updated_at: 'later' };
    else if (url.endsWith('/publish')) data = { entry_id: 'published-entry' };
    else if (url === '/v1/transfers') data = { ok: true };
    else if (url === '/v1/actions/apply' || url === '/v1/actions/fetch-clipboard') data = { ok: true };
    return { ok: true, status: 200, json: async () => data, text: async () => JSON.stringify(data) };
  };
  const api = notebook.mount(root, { isChinese, locale: isChinese ? 'zh-CN' : 'en', fetchImpl });
  if (autoLoad) await api.load();
  return { root, api, calls, fetchImpl };
}
const note = (extra = {}) => ({ id: 'n1', kind: 'markdown', title: 'Draft', markdown: 'initial', tasks: [], pinned: false, trashed: false, revision: 1, ...extra });
const treeText = node => [node.textContent || '', ...node.children.map(treeText)].join(' ');

function memoryStorage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return { getItem: key => values.get(key) || null, setItem: (key, value) => values.set(key, String(value)), removeItem: key => values.delete(key), values };
}

test('restoring another note aligns the editor, selected item and subsequent save', async () => {
  const first = note({ id: 'first', title: 'First note', markdown: 'first body' });
  const deleted = note({ id: 'deleted', title: 'Restored note', markdown: 'restored body', trashed: true });
  const f = await fixture({ items: [first, deleted], onRequest: async (url) => {
    if (url === '/v1/notebook/deleted/restore') {
      deleted.trashed = false;
      return { ok: true, status: 200, json: async () => ({ ...deleted, revision: 2 }) };
    }
  } });
  await buttonNamed(f.root, 'First note').dispatch('click');
  await buttonNamed(f.root, 'Recently Deleted').dispatch('click');
  await buttonNamed(f.root, 'Restore').dispatch('click');
  assert.equal(byClass(f.root, 'notebook-title-input').value, 'Restored note');
  const body = byClass(f.root, 'notebook-markdown-input');
  assert.equal(body.value, 'restored body');
  body.value = 'edited restored body'; await body.dispatch('input');
  assert.equal(await f.api.whenIdle(), true);
  const saved = f.calls.filter(c => c.method === 'PUT').at(-1);
  assert.equal(saved.url, '/v1/notebook/deleted');
  assert.equal(saved.body.title, 'Restored note');
  assert.equal(saved.body.markdown, 'edited restored body');
});

test('a delayed restore does not take over a draft edited during the request', async () => {
  const first = note({ id: 'first', title: 'First note', markdown: 'first body' });
  const deleted = note({ id: 'deleted', title: 'Restored note', markdown: 'restored body', trashed: true });
  let release;
  const f = await fixture({ items: [first, deleted], onRequest: async (url) => {
    if (url === '/v1/notebook/deleted/restore') {
      await new Promise(resolve => { release = resolve; });
      deleted.trashed = false;
      return { ok: true, status: 200, json: async () => ({ ...deleted, revision: 2 }) };
    }
  } });
  await buttonNamed(f.root, 'First note').dispatch('click');
  await buttonNamed(f.root, 'Recently Deleted').dispatch('click');
  const restoring = buttonNamed(f.root, 'Restore').dispatch('click');
  while (!release) await new Promise(resolve => setImmediate(resolve));
  const body = byClass(f.root, 'notebook-markdown-input');
  body.value = 'new first draft'; await body.dispatch('input');
  release(); await restoring;
  assert.equal(byClass(f.root, 'notebook-title-input').value, 'First note');
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'new first draft');
  assert.equal(await f.api.whenIdle(), true);
  const saved = f.calls.filter(c => c.method === 'PUT').at(-1);
  assert.equal(saved.url, '/v1/notebook/first');
  assert.equal(saved.body.markdown, 'new first draft');
});

test('the initial notes tab loads when the shell selects it before calling load', async () => {
  const f = await fixture({ items: [note()], autoLoad: false });
  assert.equal(buttonNamed(f.root, 'New'), null);
  assert.equal(await f.api.setSection('notes'), true);
  assert.ok(buttonNamed(f.root, 'New'));
  assert.ok(buttonNamed(f.root, 'Draft'));
  const before = f.calls.filter(call => call.url.startsWith('/v1/notebook?')).length;
  assert.equal(await f.api.setSection('notes'), true);
  assert.equal(f.calls.filter(call => call.url.startsWith('/v1/notebook?')).length, before);
});

test('section remembers only last item IDs and preserves a draft on the current tab', async () => {
  const storage = memoryStorage();
  const todo = note({ id: 't1', kind: 'todo', title: 'List', markdown: '', tasks: [] });
  let saved = note();
  const f = await fixture({ items: [note(), todo], storage, onRequest: async (url, init, body) => {
    if (url === '/v1/notebook/n1' && init.method === 'PUT') {
      saved = { ...saved, ...body, revision: saved.revision + 1 };
      return { ok: true, json: async () => saved };
    }
    if (url === '/v1/notebook/n1' && init.method === 'GET') return { ok: true, json: async () => ({ ...saved }) };
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  let input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'private draft'; await input.dispatch('input');
  assert.equal(await f.api.setSection('notes'), true);
  assert.equal(byClass(f.root, 'notebook-markdown-input'), input);
  assert.equal(input.value, 'private draft');
  assert.equal(await f.api.setSection('todo'), true);
  await buttonNamed(f.root, 'List').dispatch('click');
  assert.equal(await f.api.setSection('notes'), true);
  assert.equal(byClass(f.root, 'notebook-title-input').value, 'Draft');
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'private draft');
  assert.equal(storage.getItem('peercarry.notebook.last.notes'), 'n1');
  assert.equal(storage.getItem('peercarry.notebook.last.todo'), 't1');
  assert.deepEqual([...storage.values.values()].sort(), ['n1', 't1']);
  const reopened = await fixture({ items: [saved, todo], storage });
  assert.equal(byClass(reopened.root, 'notebook-title-input').value, 'Draft');
  assert.equal(byClass(reopened.root, 'notebook-markdown-input').value, 'private draft');
});

test('missing remembered entries fall back safely and storage failures do not prevent editing', async () => {
  const storage = memoryStorage({ 'peercarry.notebook.last.notes': 'missing' });
  const f = await fixture({ items: [note()], storage });
  assert.equal(storage.getItem('peercarry.notebook.last.notes'), null);
  assert.equal(byClass(f.root, 'notebook-title-input'), null);
  const broken = { getItem() { throw Error('blocked'); }, setItem() { throw Error('blocked'); }, removeItem() { throw Error('blocked'); } };
  const other = await fixture({ items: [note()], storage: broken });
  await buttonNamed(other.root, 'Draft').dispatch('click');
  assert.equal(byClass(other.root, 'notebook-title-input').value, 'Draft');
});

test('new notes focus the title and Todo Enter adds a focused row only after committed nonempty input', async () => {
  const f = await fixture();
  await buttonNamed(f.root, 'New').dispatch('click');
  assert.equal(f.root.ownerDocument.activeElement, byClass(f.root, 'notebook-title-input'));
  await f.api.setSection('todo');
  await buttonNamed(f.root, 'New').dispatch('click');
  await buttonNamed(f.root, 'Add task').dispatch('click');
  let fields = f.root.querySelectorAll('.notebook-task-input');
  assert.equal(f.root.ownerDocument.activeElement, fields[0]);
  fields[0].value = 'First'; await fields[0].dispatch('input');
  let prevented = false;
  await fields[0].dispatch('keydown', { key: 'Enter', isComposing: true, preventDefault() { prevented = true; } });
  assert.equal(f.root.querySelectorAll('.notebook-task-input').length, 1);
  assert.equal(prevented, false);
  f.root.ownerDocument.dispatch('compositionstart', { target: fields[0] });
  await fields[0].dispatch('keydown', { key: 'Enter', preventDefault() { prevented = true; } });
  assert.equal(f.root.querySelectorAll('.notebook-task-input').length, 1);
  f.root.ownerDocument.dispatch('compositionend', { target: fields[0] });
  await fields[0].dispatch('keydown', { key: 'Enter', repeat: true, preventDefault() { prevented = true; } });
  assert.equal(f.root.querySelectorAll('.notebook-task-input').length, 1);
  await fields[0].dispatch('keydown', { key: 'Enter', preventDefault() { prevented = true; } });
  fields = f.root.querySelectorAll('.notebook-task-input');
  assert.equal(fields.length, 2);
  assert.equal(prevented, true);
  assert.equal(f.root.ownerDocument.activeElement, fields[1]);
  await fields[1].dispatch('keydown', { key: 'Enter', preventDefault() {} });
  assert.equal(f.root.querySelectorAll('.notebook-task-input').length, 2);
  await f.api.whenIdle();
});

test('search keeps the input and editor while ignoring a slower prior response', async () => {
  let releaseOld;
  const f = await fixture({ items: [note()], onRequest: async url => {
    if (url.includes('q=old')) {
      await new Promise(resolve => { releaseOld = resolve; });
      return { ok: true, json: async () => ({ items: [note({ id: 'old', title: 'Old result' })] }) };
    }
    if (url.includes('q=new')) return { ok: true, json: async () => ({ items: [note({ id: 'new', title: 'New result' })] }) };
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const editor = byClass(f.root, 'notebook-markdown-input');
  const search = byClass(f.root, 'notebook-search'); search.focus();
  search.value = 'old'; await search.dispatch('input');
  await new Promise(resolve => setTimeout(resolve, 300));
  assert.equal(typeof releaseOld, 'function');
  search.value = 'new'; await search.dispatch('input');
  await new Promise(resolve => setTimeout(resolve, 300));
  releaseOld(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(byClass(f.root, 'notebook-search'), search);
  assert.equal(f.root.ownerDocument.activeElement, search);
  assert.equal(byClass(f.root, 'notebook-markdown-input'), editor);
  assert.ok(buttonNamed(f.root, 'New result'));
  assert.equal(buttonNamed(f.root, 'Old result'), null);
});

test('saving a title refreshes the list without replacing the focused editor', async () => {
  let current = note();
  const f = await fixture({ items: [current], onRequest: async (url, init, body) => {
    if (url.startsWith('/v1/notebook?')) return { ok: true, json: async () => ({ items: [current] }) };
    if (url === '/v1/notebook/n1' && init.method === 'GET') return { ok: true, json: async () => ({ ...current }) };
    if (url === '/v1/notebook/n1' && init.method === 'PUT') {
      current = { ...current, ...body, revision: current.revision + 1 };
      return { ok: true, json: async () => current };
    }
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const title = byClass(f.root, 'notebook-title-input');
  title.focus(); title.value = 'Renamed'; await title.dispatch('input');
  await buttonNamed(f.root, 'Save').dispatch('click');
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(buttonNamed(f.root, 'Renamed'));
  assert.equal(byClass(f.root, 'notebook-title-input'), title);
  assert.equal(f.root.ownerDocument.activeElement, title);
});

test('preview and more controls retain export and sharing; failed save blocks back until retry', async () => {
  let fail = true;
  const f = await fixture({ items: [note()], onRequest: async (url, init) => {
    if (init.method === 'PUT' && fail) return { ok: false, status: 500, json: async () => ({ error: 'offline' }) };
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  assert.ok(walk(byClass(f.root, 'notebook-more'), n => n.tagName === 'SUMMARY'));
  assert.ok(buttonNamed(byClass(f.root, 'notebook-more'), 'Export .md'));
  assert.ok(buttonNamed(byClass(f.root, 'notebook-more'), 'Send to device'));
  await buttonNamed(f.root, 'Preview').dispatch('click');
  assert.equal(byClass(f.root, 'notebook-preview').hidden, false);
  await buttonNamed(f.root, 'Edit').dispatch('click');
  const input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'keep me'; await input.dispatch('input');
  await buttonNamed(f.root, 'Back to list').dispatch('click');
  assert.ok(f.root.className.includes('notebook-show-editor'));
  assert.equal(byClass(f.root, 'notebook-retry').hidden, false);
  assert.equal(input.value, 'keep me');
  fail = false;
  await buttonNamed(f.root, 'Retry save').dispatch('click');
  await buttonNamed(f.root, 'Back to list').dispatch('click');
  assert.equal(f.root.className.includes('notebook-show-editor'), false);
});

test('mount exposes API, creates markdown notes and serially saves latest input', async () => {
  let releaseFirst, writes = 0; const savedBodies = [];
  const f = await fixture({ onRequest: async (url, init, body) => {
    if (url === '/v1/notebook' && init.method === 'POST') return { ok: true, status: 201, json: async () => ({ ...body, id: 'created', revision: 1 }) };
    if (url === '/v1/notebook/created' && init.method === 'PUT') { savedBodies.push(body); writes++; if (writes === 1) await new Promise(r => releaseFirst = r); return { ok: true, status: 200, json: async () => ({ revision: writes + 1 }) }; }
  }});
  assert.deepEqual(Object.keys(f.api).sort(), ['load', 'setSection', 'setVisible', 'whenIdle']);
  await buttonNamed(f.root, 'New').dispatch('click');
  let title = byClass(f.root, 'notebook-title-input'), input = byClass(f.root, 'notebook-markdown-input');
  title.value = 'first'; await title.dispatch('input'); const pendingSave = buttonNamed(f.root, 'Save').dispatch('click');
  await new Promise(r => setImmediate(r));
  title = byClass(f.root, 'notebook-title-input'); input = byClass(f.root, 'notebook-markdown-input');
  title.value = 'latest'; await title.dispatch('input'); input.value = 'latest body'; await input.dispatch('input');
  releaseFirst(); await pendingSave; await f.api.whenIdle();
  assert.equal(savedBodies.length, 2);
  assert.equal(savedBodies[1].title, 'latest'); assert.equal(savedBodies[1].markdown, 'latest body');
  assert.equal(savedBodies[0].expected_revision, 1); assert.equal(savedBodies[1].expected_revision, 2);
});

test('save failure keeps the draft and section switch refuses to drop it', async () => {
  const f = await fixture({ items: [note()], onRequest: async (url, init) => url === '/v1/notebook/n1' && init.method === 'PUT' ? ({ ok: false, status: 500, json: async () => ({ error: 'storage failed' }) }) : null });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const input = byClass(f.root, 'notebook-markdown-input'); input.value = 'kept draft'; await input.dispatch('input'); await buttonNamed(f.root, 'Save').dispatch('click');
  assert.equal(await f.api.setSection('todo'), false);
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'kept draft');
  assert.ok(byClass(f.root, 'notebook-status').textContent.includes('Save failed'), byClass(f.root, 'notebook-status').textContent);
});

test('pinning another note saves the current draft first and refuses on failure', async () => {
  for (const failSave of [false, true]) {
    const f = await fixture({ items: [note(), note({ id: 'n2', title: 'Second' })], onRequest: async (url, init) => {
      if (failSave && url === '/v1/notebook/n1' && init.method === 'PUT') {
        return { ok: false, status: 500, json: async () => ({ error: 'storage failed' }) };
      }
      return null;
    } });
    await buttonNamed(f.root, 'Draft').dispatch('click');
    const input = byClass(f.root, 'notebook-markdown-input');
    input.value = 'unsaved first-note draft'; await input.dispatch('input');
    const secondRow = all(f.root, n => n.className === 'notebook-item').find(n => n.dataset.id === 'n2');
    await buttonNamed(secondRow, 'Pin').dispatch('click');
    const firstWrite = f.calls.findIndex(c => c.url === '/v1/notebook/n1' && c.method === 'PUT');
    assert.ok(firstWrite >= 0, 'the original draft must be saved before selecting the other note');
    if (failSave) {
      assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'unsaved first-note draft');
      assert.equal(f.calls.some(c => c.url === '/v1/notebook/n2'), false);
    } else {
      assert.ok(f.calls.findIndex(c => c.url === '/v1/notebook/n2') > firstWrite);
    }
  }
});

test('IME automatic save waits for composition end', async () => {
  const f = await fixture({ items: [note()] });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const input = byClass(f.root, 'notebook-markdown-input');
  f.root.ownerDocument.dispatch('compositionstart', { target: input });
  input.value = 'intermediate IME text'; await input.dispatch('input');
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(f.calls.filter(c => c.method === 'PUT').length, 0);
  assert.equal(await f.api.setSection('todo'), false);
  input.value = 'final committed text'; await input.dispatch('input');
  f.root.ownerDocument.dispatch('compositionend', { target: input });
  await f.api.whenIdle();
  assert.equal(f.calls.filter(c => c.method === 'PUT').length, 1);
  assert.equal(f.calls.find(c => c.method === 'PUT').body.markdown, 'final committed text');
});

test('a delayed note selection cannot overwrite input entered while it loads', async () => {
  let release;
  const f = await fixture({ items: [note(), note({ id: 'n2', title: 'Second' })], onRequest: async (url, init) => {
    if (url === '/v1/notebook/n2' && init.method === 'GET') {
      await new Promise(resolve => { release = resolve; });
      return { ok: true, status: 200, json: async () => note({ id: 'n2', title: 'Second' }) };
    }
    return null;
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const pending = buttonNamed(f.root, 'Second').dispatch('click');
  await new Promise(resolve => setImmediate(resolve));
  const input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'new input while waiting'; await input.dispatch('input');
  release(); await pending;
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'new input while waiting');
  await f.api.whenIdle();
  assert.equal(f.calls.find(c => c.method === 'PUT').url, '/v1/notebook/n1');
});

test('creating a note cannot overwrite a draft edited while the create request is pending', async () => {
  let release;
  const f = await fixture({ items: [note()], onRequest: async (url, init, body) => {
    if (url === '/v1/notebook' && init.method === 'POST') {
      await new Promise(resolve => { release = resolve; });
      return { ok: true, status: 201, json: async () => note({ ...body, id: 'created' }) };
    }
    return null;
  } });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const creating = buttonNamed(f.root, 'New').dispatch('click');
  await new Promise(resolve => setImmediate(resolve));
  const input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'keep this draft while creating'; await input.dispatch('input');
  release(); await creating;
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'keep this draft while creating');
  await f.api.whenIdle();
  assert.equal(f.calls.find(c => c.method === 'PUT').url, '/v1/notebook/n1');
});

test('Ctrl/Cmd+S saves and ignores shortcuts during IME composition', async () => {
  const f = await fixture({ items: [note()] });
  await buttonNamed(f.root, 'Draft').dispatch('click');
  let input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'composing draft';
  await input.dispatch('input');
  let prevented = false;
  f.root.ownerDocument.dispatch('keydown', {
    target: input,
    ctrlKey: true,
    key: 's',
    isComposing: true,
    preventDefault() { prevented = true; },
  });
  assert.equal(prevented, false);
  assert.equal(f.calls.filter(call => call.method === 'PUT').length, 0);
  await f.api.whenIdle();
  assert.equal(f.calls.filter(call => call.method === 'PUT').length, 1);

  input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'shortcut draft';
  await input.dispatch('input');
  prevented = false;
  f.root.ownerDocument.dispatch('keydown', {
    target: input,
    metaKey: true,
    key: 'S',
    isComposing: false,
    preventDefault() { prevented = true; },
  });
  assert.equal(prevented, true);
  await f.api.whenIdle();
  assert.equal(f.calls.filter(call => call.method === 'PUT').length, 2);
});

test('409 keeps draft and offers reload or save as copy', async () => {
  let conflict = true; const f = await fixture({ items: [note()], onRequest: async (url, init) => {
    if (url === '/v1/notebook/n1' && init.method === 'PUT' && conflict) { conflict = false; return { ok: false, status: 409, json: async () => ({ error: 'revision_conflict', current_revision: 4 }) }; }
  }});
  await buttonNamed(f.root, 'Draft').dispatch('click'); const input = byClass(f.root, 'notebook-markdown-input'); input.value = 'local unsaved'; await input.dispatch('input'); await buttonNamed(f.root, 'Save').dispatch('click');
  assert.equal(byClass(f.root, 'notebook-conflict') != null, true); assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'local unsaved');
  assert.ok(buttonNamed(f.root, 'Save as copy'), byClass(f.root, 'notebook-status').textContent); assert.ok(buttonNamed(f.root, 'Reload server version'));
  await buttonNamed(f.root,'Save as copy').dispatch('click');
  assert.ok(f.calls.some(c=>c.url==='/v1/notebook'&&c.method==='POST'));
  assert.ok(f.calls.some(c=>c.method==='PUT'&&c.body.title==='Draft (copy)'&&c.body.markdown==='local unsaved'));
});

test('Markdown preview treats HTML as text and disables javascript links', async () => {
  const malicious = '<script>alert(1)</script> <img src=x onerror=alert(2)> [bad](javascript:alert(3)) [safe](https://example.com)';
  const f = await fixture({ items: [note({ markdown: malicious })] }); await buttonNamed(f.root, 'Draft').dispatch('click');
  const preview = byClass(f.root, 'notebook-preview');
  assert.equal(all(preview, n => n.tagName === 'SCRIPT' || n.tagName === 'IMG').length, 0);
  assert.ok(all(preview, n => n.tagName === '#TEXT').some(n => n.textContent.includes('<script>')));
  const links = all(preview, n => n.tagName === 'A'); assert.equal(links.length, 1); assert.equal(links[0].href, 'https://example.com/'); assert.equal(links[0].rel, 'noopener noreferrer');
});

test('Todo checkbox saves task state without clipboard apply', async () => {
  const todo = { id:'t1',kind:'todo',title:'List',markdown:'',tasks:[{id:'task1',text:'Do it',done:false}],pinned:false,trashed:false,revision:1 };
  const f = await fixture({ items:[todo] }); await f.api.setSection('todo'); await buttonNamed(f.root,'List').dispatch('click');
  const checkbox = walk(byClass(f.root, 'notebook-task-row'),n=>n.tagName==='INPUT'&&n.type==='checkbox'); checkbox.checked=true; await checkbox.dispatch('change'); await buttonNamed(f.root,'Save').dispatch('click');
  const update = f.calls.find(c=>c.method==='PUT'); assert.equal(update.body.tasks[0].done,true); assert.equal(f.calls.some(c=>c.url==='/v1/actions/apply'),false);
});

test('Markdown export keeps source text verbatim', async () => {
  const source = '<b>literal</b>\n[unsafe](javascript:bad)\n'; const f = await fixture({ items:[note({markdown:source})] });
  await buttonNamed(f.root,'Draft').dispatch('click');
  const oldCreate = URL.createObjectURL, oldRevoke = URL.revokeObjectURL; let exported;
  URL.createObjectURL = blob => { exported = blob; return 'blob:test'; }; URL.revokeObjectURL = () => {};
  try { buttonNamed(f.root,'Export .md').click(); assert.equal(await exported.text(), '# Draft\n\n' + source); }
  finally { URL.createObjectURL = oldCreate; URL.revokeObjectURL = oldRevoke; }
});

test('peer filter uses origin.node_id; clipboard copy, fetch, download, and send are explicit', async () => {
  const peers=[{node_id:'node-a',host:'A'},{node_id:'node-b',host:'B'}];
  const entries=[{id:'entry-a',kind:'text',preview:'A text',origin:{node_id:'node-a',host:'A'}},{id:'entry-b',kind:'files',preview:'B file',origin:{node_id:'node-b',host:'B'}}];
  const f=await fixture({peers,entries}); await f.api.setSection('clipboard');
  assert.equal(byClass(f.root,'notebook-clipboard-list').children.length,2);
  let select=byClass(f.root,'notebook-peer-filter'); select.value='node-b'; await select.dispatch('change'); await new Promise(r=>setImmediate(r));
  assert.equal(byClass(f.root,'notebook-clipboard-list').children.length,1);
  assert.equal(buttonNamed(f.root,'Copy'), null, 'file entries must not be applied as clipboard content');
  let downloaded; global.PeerCarryTransfers={download:e=>downloaded=e}; await buttonNamed(f.root,'Download').dispatch('click'); assert.equal(downloaded.id,'entry-b'); delete global.PeerCarryTransfers;
  const send=walk(f.root,n=>n.tagName==='BUTTON'&&n.dataset.nodeId==='node-a'); await send.dispatch('click');
  assert.deepEqual(f.calls.find(c=>c.url==='/v1/transfers').body,{operation:'send',node_id:'node-a',entry_id:'entry-b',expires_in_secs:86400});
  select=byClass(f.root,'notebook-peer-filter'); select.value='node-a'; await select.dispatch('change'); await new Promise(r=>setImmediate(r));
  await buttonNamed(f.root,'Copy').dispatch('click');
  assert.deepEqual(f.calls.find(c=>c.url==='/v1/actions/apply').body,{id:'entry-a'});
  select=byClass(f.root,'notebook-peer-filter'); select.value='node-b'; await select.dispatch('change'); await new Promise(r=>setImmediate(r));
  const applyCount=f.calls.filter(c=>c.url==='/v1/actions/apply').length; const before=f.calls.length; const fetchButton=buttonNamed(f.root,'Refresh'); await fetchButton.dispatch('click');
  assert.ok(f.calls.some(c=>c.url==='/v1/peers')); assert.ok(f.calls.some(c=>c.url.startsWith('/v1/timeline'))); assert.equal(f.calls.length>before,true);
  await buttonNamed(f.root,'Fetch selected device clipboard').dispatch('click');
  assert.deepEqual(f.calls.find(c=>c.url==='/v1/actions/fetch-clipboard').body,{node_id:'node-b'});
  assert.equal(f.calls.filter(c=>c.url==='/v1/actions/apply').length,applyCount); // Fetch only refreshes history; the prior Copy is the only apply.
});

test('notebook sharing publishes after save and sends exactly once to the selected node', async () => {
  const f=await fixture({items:[note()],peers:[{node_id:'n-a',host:'A'},{node_id:'n-b',host:'B'}]}); await buttonNamed(f.root,'Draft').dispatch('click');
  await buttonNamed(f.root,'Send to device').dispatch('click');
  const dialog=walk(f.root,n=>n.className==='notebook-share-dialog'); const select=dialog.children[0]; select.value='n-b'; await Promise.all([dialog.children[1].dispatch('click'),dialog.children[1].dispatch('click')]);
  assert.equal(f.calls.filter(c=>c.url.endsWith('/publish')).length,1);
  assert.deepEqual(f.calls.find(c=>c.url==='/v1/transfers').body,{operation:'send',node_id:'n-b',entry_id:'published-entry',expires_in_secs:86400});
  assert.ok(f.calls.findIndex(c=>c.method==='PUT') < f.calls.findIndex(c=>c.url.endsWith('/publish')));
});

test('publishing failure never reports delivery', async () => {
  const f=await fixture({items:[note()],peers:[{node_id:'n-a'}],onRequest:async url=>url.endsWith('/publish')?({ok:false,status:500,json:async()=>({message:'publish failed'})}):null});
  await buttonNamed(f.root,'Draft').dispatch('click'); await buttonNamed(f.root,'Send to device').dispatch('click'); const dialog=walk(f.root,n=>n.className==='notebook-share-dialog'); dialog.children[0].value='n-a'; await dialog.children[1].dispatch('click');
  assert.equal(f.calls.some(c=>c.url==='/v1/transfers'),false); assert.ok(byClass(f.root,'notebook-status').textContent.includes('before export'));
});

test('task title and text limits count Unicode code points and UTF-8 bytes', async () => {
  const todo={id:'t1',kind:'todo',title:'List',markdown:'',tasks:[{id:'task1',text:'',done:false}],pinned:false,trashed:false,revision:1};
  const f=await fixture({items:[todo]}); await f.api.setSection('todo'); await buttonNamed(f.root,'List').dispatch('click');
  const title=byClass(f.root,'notebook-title-input'); title.value='😀'.repeat(205); await title.dispatch('input'); assert.equal(Array.from(title.value).length,200);
  const task=byClass(f.root,'notebook-task-row').children[1]; task.value='汉'.repeat(1400); await task.dispatch('input'); assert.ok(new TextEncoder().encode(task.value).length<=4096);
});

test('transfer failure after publish identifies local history and does not claim delivery', async () => {
  const f=await fixture({items:[note()],peers:[{node_id:'n-a'}],onRequest:async url=>url==='/v1/transfers'?({ok:false,status:503,json:async()=>({message:'offline'})}):null});
  await buttonNamed(f.root,'Draft').dispatch('click'); await buttonNamed(f.root,'Send to device').dispatch('click'); const dialog=walk(f.root,n=>n.className==='notebook-share-dialog'); dialog.children[0].value='n-a'; await dialog.children[1].dispatch('click');
  assert.equal(f.calls.filter(c=>c.url.endsWith('/publish')).length,1); assert.equal(f.calls.filter(c=>c.url==='/v1/transfers').length,1);
  assert.ok(byClass(f.root,'notebook-status').textContent.includes('Export entered local history'));
  assert.ok(byClass(f.root,'notebook-status').textContent.includes('not delivered'));
});

test('test file is scoped to the assigned module contract', () => {
  assert.ok(fs.existsSync(path.join(__dirname,'../../crates/peercarry-core/notebook.js')));
});

test('notebook and todo labels follow the desktop language supplied by the shell', async () => {
  for (const [isChinese, labels] of [
    [false, { create: 'New', title: 'Title', save: 'Save', addTask: 'Add task' }],
    [true, { create: '新建', title: '标题', save: '保存', addTask: '添加任务' }],
  ]) {
    const f = await fixture({ isChinese });
    assert.ok(buttonNamed(f.root, labels.create));
    await buttonNamed(f.root, labels.create).dispatch('click');
    assert.equal(byClass(f.root, 'notebook-title-input').placeholder, labels.title);
    assert.ok(buttonNamed(f.root, labels.save));
    await f.api.setSection('todo');
    await buttonNamed(f.root, labels.create).dispatch('click');
    assert.ok(buttonNamed(f.root, labels.addTask));
  }
});

test('missing sync endpoints are reported as unsupported while local notes remain usable', async () => {
  const f = await fixture({ onRequest: async url => {
    if (url === '/v1/notebook/sync/settings') return { ok: false, status: 404, json: async () => ({ error: 'not_found' }) };
  }});
  assert.ok(byClass(f.root, 'notebook-sync-state').textContent.includes('unavailable'));
  assert.equal(byClass(f.root, 'notebook-sync-enable-control').disabled, true);
  await buttonNamed(f.root, 'New').dispatch('click');
  assert.equal(f.calls.some(call => call.url === '/v1/notebook' && call.method === 'POST'), true);
});

test('sync settings use discovered peer IDs privately and manual checks coalesce', async () => {
  const discovered = [{ node_id: 'private-node-uuid', host: 'Workstation' }];
  let settings = { enabled: false, peer_ids: [] };
  let runCount = 0, releaseRun;
  const runGate = new Promise(resolve => { releaseRun = resolve; });
  const f = await fixture({ peers: discovered, onRequest: async (url, init, body) => {
    if (url === '/v1/notebook/sync/settings' && init.method !== 'POST') return { ok: true, status: 200, json: async () => ({ ...settings, peer_ids: settings.peer_ids.slice() }) };
    if (url === '/v1/notebook/sync/status') return { ok: true, status: 200, json: async () => ({
      ...settings,
      ready: true,
      needs_authentication: false,
      conflicted_items: 0,
      peers: settings.peer_ids.map(node_id => ({ node_id, host: 'Workstation', state: 'checked', last_success_at: '2026-10-27T01:00:00Z' })),
    }) };
    if (url === '/v1/notebook/sync/settings' && init.method === 'POST') {
      settings = { enabled: body.enabled, peer_ids: body.peer_ids.slice() };
      return { ok: true, status: 200, json: async () => settings };
    }
    if (url === '/v1/notebook/sync/run') { runCount++; await runGate; return { ok: true, status: 202, json: async () => ({ accepted: true }) }; }
  }});
  const peerChoice = byClass(f.root, 'notebook-sync-peer-choice');
  peerChoice.checked = true;
  await peerChoice.dispatch('change');
  assert.deepEqual(f.calls.find(call => call.url === '/v1/notebook/sync/settings' && call.method === 'POST').body, {
    enabled: false,
    peer_ids: ['private-node-uuid'],
  });
  const enable = byClass(f.root, 'notebook-sync-enable-control');
  enable.checked = true;
  await enable.dispatch('change');
  assert.equal(settings.enabled, true);
  const rendered = treeText(byClass(f.root, 'notebook-sync-card'));
  assert.ok(rendered.includes('Workstation'));
  assert.equal(rendered.includes('private-node-uuid'), false);
  assert.equal(rendered.toLowerCase().includes('all devices synced'), false);

  const checkButton = buttonNamed(f.root, 'Check now');
  const first = checkButton.dispatch('click');
  await Promise.resolve();
  const second = checkButton.dispatch('click');
  await Promise.resolve();
  assert.equal(runCount, 1);
  releaseRun();
  await Promise.all([first, second]);
  assert.equal(f.calls.filter(call => call.url === '/v1/notebook/sync/run').length, 1);
});

test('sync enable failure gives localized authentication guidance without exposing tokens', async () => {
  const peers = [{ node_id: 'node-private', host: 'Desk' }];
  const f = await fixture({ peers, isChinese: true, onRequest: async (url, init, body) => {
    if (url === '/v1/notebook/sync/settings' && init.method !== 'POST') return { ok: true, status: 200, json: async () => ({ enabled: false, peer_ids: ['node-private'] }) };
    if (url === '/v1/notebook/sync/status') return { ok: true, status: 200, json: async () => ({ enabled: false, ready: false, needs_authentication: true, peer_ids: ['node-private'], peers: [] }) };
    if (url === '/v1/notebook/sync/settings' && init.method === 'POST') return { ok: false, status: 400, json: async () => ({ error: 'needs_authentication' }) };
  }});
  const enable = byClass(f.root, 'notebook-sync-enable-control');
  enable.checked = true;
  await enable.dispatch('change');
  const rendered = treeText(byClass(f.root, 'notebook-sync-card'));
  assert.ok(rendered.includes('请先在“设置”中配置设备认证') || rendered.includes('需要先配置设备认证'));
  assert.equal(rendered.includes('node-private'), false);
  assert.equal(rendered.includes('auth_token'), false);
  assert.equal(byClass(f.root, 'notebook-sync-enable-control').checked, false);
});

test('sync refresh does not reload the visible list while an editor is focused or composing', async () => {
  const item = note();
  const f = await fixture({ items: [item], peers: [{ node_id: 'peer', host: 'Desk' }], onRequest: async url => {
    if (url === '/v1/notebook/sync/settings') return { ok: true, status: 200, json: async () => ({ enabled: true, peer_ids: ['peer'] }) };
    if (url === '/v1/notebook/sync/status') return { ok: true, status: 200, json: async () => ({ enabled: true, ready: true, needs_authentication: false, peer_ids: ['peer'], conflicted_items: 0, peers: [{ node_id: 'peer', state: 'checked' }] }) };
  }});
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const input = byClass(f.root, 'notebook-markdown-input');
  const listRequests = () => f.calls.filter(call => call.url.startsWith('/v1/notebook?')).length;
  let before = listRequests();
  f.root.ownerDocument.activeElement = input;
  await buttonNamed(f.root, 'Check now').dispatch('click');
  assert.equal(listRequests(), before);
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'initial');

  f.root.ownerDocument.activeElement = null;
  f.root.ownerDocument.dispatch('compositionstart', { target: input });
  before = listRequests();
  await buttonNamed(f.root, 'Check now').dispatch('click');
  assert.equal(listRequests(), before);
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'initial');
});

test('sync conflicts keep a dirty draft, save it as a copy, and resolve by explicit version ID', async () => {
  const versions = [
    { version_id: 'version-a', device_id: 'replica-secret-id', kind: 'markdown', title: 'Remote A', markdown: 'remote body', tasks: [], pinned: false, trashed: false, created_at: '2026-10-27T00:00:00Z', updated_at: '2026-10-27T00:00:00Z' },
    { version_id: 'version-b', device_id: 'replica-unknown-id', kind: 'markdown', title: 'Remote B', markdown: 'other body', tasks: [], pinned: false, trashed: false, created_at: '2026-10-27T00:00:00Z', updated_at: '2026-10-27T00:00:00Z' },
  ];
  let current = note({ revision: 7, conflict_count: 2 });
  let copy = null;
  const f = await fixture({ items: [current], peers: [{ node_id: 'replica-secret-id', host: 'Laptop Two' }], onRequest: async (url, init, body) => {
    if (url === '/v1/notebook/n1' && init.method === 'PUT') return { ok: false, status: 409, json: async () => ({ error: 'sync_conflict' }) };
    if (url === '/v1/notebook/n1/conflicts') return { ok: true, status: 200, json: async () => ({ versions }) };
    if (url === '/v1/notebook/n1' && (!init.method || init.method === 'GET')) return { ok: true, status: 200, json: async () => ({ ...current }) };
    if (url === '/v1/notebook/n1/resolve') {
      current = { ...current, title: 'Remote A', markdown: 'remote body', revision: 8, conflict_count: 0 };
      return { ok: true, status: 200, json: async () => current };
    }
    if (url === '/v1/notebook' && init.method === 'POST') {
      copy = { ...body, id: 'copy-note', revision: 1 };
      return { ok: true, status: 201, json: async () => copy };
    }
    if (url.startsWith('/v1/notebook?')) return { ok: true, status: 200, json: async () => ({ items: [current, ...(copy ? [copy] : [])] }) };
  }});
  await buttonNamed(f.root, 'Draft').dispatch('click');
  const input = byClass(f.root, 'notebook-markdown-input');
  input.value = 'local unsaved draft';
  await input.dispatch('input');
  await buttonNamed(f.root, 'Save').dispatch('click');
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'local unsaved draft');
  assert.ok(buttonNamed(f.root, 'Save draft as copy'));
  await buttonNamed(f.root, 'View conflict versions').dispatch('click');
  await buttonNamed(f.root, 'Save draft as copy').dispatch('click');
  assert.equal(copy.markdown, 'local unsaved draft');
  assert.equal(copy.title, 'Draft (copy)');
  assert.ok(buttonNamed(f.root, 'Keep this version'));
  const rendered = treeText(f.root);
  assert.ok(rendered.includes('Laptop Two'));
  assert.equal(rendered.includes('replica-secret-id'), false);
  assert.equal(rendered.includes('replica-unknown-id'), false);
  await buttonNamed(f.root, 'Keep this version').dispatch('click');
  const resolution = f.calls.find(call => call.url === '/v1/notebook/n1/resolve');
  assert.deepEqual(resolution.body, { expected_revision: 7, version_id: 'version-a' });
  assert.equal(f.calls.some(call => call.url.endsWith('/conflicts')), true);
  assert.equal(byClass(f.root, 'notebook-markdown-input').value, 'remote body');
});
