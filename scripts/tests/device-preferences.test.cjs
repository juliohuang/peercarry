const test = require('node:test');
const assert = require('node:assert/strict');
const { create, KEY, WORKBENCH_KEY, QUICK_KEY } = require('../../crates/peercarry-core/device-preferences.js');

function storage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    values,
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
  };
}

test('migrates both legacy keys without copying unrelated content', () => {
  const saved = storage({
    [WORKBENCH_KEY]: 'workstation-id',
    [QUICK_KEY]: JSON.stringify({ favorite: ['laptop-id', 'laptop-id'], recent: ['laptop-id', 'other-id'], text: 'private text', token: 'secret' }),
  });
  const prefs = create(saved);
  assert.deepEqual(prefs.read(), {
    version: 1, selected: 'workstation-id', favorites: ['laptop-id'], recent: ['workstation-id', 'laptop-id', 'other-id'],
  });
  assert.equal(saved.values.get(WORKBENCH_KEY), 'workstation-id');
  assert.equal(JSON.parse(saved.values.get(QUICK_KEY)).recent[0], 'laptop-id');
  assert.equal(saved.values.get(KEY).includes('private text'), false);
  assert.equal(saved.values.get(KEY).includes('secret'), false);
});

test('uses a legacy recent choice but does not select a favorite by itself', () => {
  const recent = create(storage({ [QUICK_KEY]: JSON.stringify({ favorite: ['favorite-id'], recent: ['recent-id'] }) }));
  assert.equal(recent.read().selected, 'recent-id');
  const favoriteOnly = create(storage({ [QUICK_KEY]: JSON.stringify({ favorite: ['favorite-id'], recent: [] }) }));
  assert.equal(favoriteOnly.read().selected, '');
  assert.equal(favoriteOnly.resolve(['favorite-id']).selected, '');
});

test('rejects malformed stored IDs and never substitutes another available device', () => {
  const saved = storage({
    [KEY]: JSON.stringify({ version: 1, selected: 'removed-id', favorites: ['ok-id', 'ok-id', '', '\n', 'x'.repeat(129)], recent: [42, 'ok-id'] }),
  });
  const prefs = create(saved);
  assert.deepEqual(prefs.read().favorites, ['ok-id']);
  assert.deepEqual(prefs.read().recent, ['ok-id']);
  assert.equal(prefs.resolve(['ok-id']).selected, '');
  assert.equal(prefs.resolve(['ok-id']).missing, true);
  assert.equal(prefs.read().selected, 'removed-id');
  const broken = create(storage({ [KEY]: '{bad', [QUICK_KEY]: '[]' }));
  assert.equal(broken.resolve(['ok-id']).selected, '');
});

test('both windows share explicit selection and favorites without reviving old values', () => {
  const saved = storage({ [QUICK_KEY]: JSON.stringify({ favorite: ['old-id'], recent: ['old-id'] }) });
  const first = create(saved), second = create(saved);
  assert.equal(first.read().selected, 'old-id');
  first.toggleFavorite('old-id');
  first.select('new-id');
  assert.deepEqual(second.read(), { version: 1, selected: 'new-id', favorites: [], recent: ['new-id', 'old-id'] });
  second.toggleFavorite('new-id');
  first.markUsed('old-id');
  assert.equal(first.read().selected, 'new-id');
  assert.deepEqual(first.read().favorites, ['new-id']);
  assert.deepEqual(first.read().recent, ['old-id', 'new-id']);
});

test('storage read or write failures keep a usable in-memory choice without overwriting unread data', () => {
  let writes = 0;
  const unreadable = create({ getItem() { throw Error('denied'); }, setItem() { writes++; } });
  assert.equal(unreadable.read().selected, '');
  unreadable.select('local-id');
  unreadable.toggleFavorite('local-id');
  assert.equal(unreadable.resolve(['local-id']).selected, 'local-id');
  assert.deepEqual(unreadable.read().favorites, ['local-id']);
  assert.equal(writes, 0);

  const readOnly = create({ getItem: () => null, setItem() { throw Error('denied'); } });
  readOnly.select('another-id');
  assert.equal(readOnly.read().selected, 'another-id');
  assert.equal(readOnly.resolve(['different-id']).selected, '');
});
