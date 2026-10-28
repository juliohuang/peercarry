// Shared desktop preferences contain stable device IDs only.
(function (root) {
  'use strict';
  const KEY = 'peercarry.device-preferences.v1';
  const WORKBENCH_KEY = 'peercarry.workbench-target.v1';
  const QUICK_KEY = 'peercarry.quick.targets.v1';
  const MAX_FAVORITES = 12, MAX_RECENT = 8;

  function validId(id) {
    return typeof id === 'string' && id.length > 0 && id.length <= 128 &&
      id.trim().length > 0 && !/[\u0000-\u001f\u007f]/.test(id);
  }
  function idList(values, limit) {
    if (!Array.isArray(values)) return [];
    return [...new Set(values.filter(validId))].slice(0, limit);
  }
  function clean(value) {
    const data = value && typeof value === 'object' && !Array.isArray(value) ? value : {};
    return {
      version: 1,
      selected: validId(data.selected) ? data.selected : '',
      favorites: idList(data.favorites, MAX_FAVORITES),
      recent: idList(data.recent, MAX_RECENT),
    };
  }
  function clone(value) {
    return { ...value, favorites: [...value.favorites], recent: [...value.recent] };
  }
  function parse(raw) {
    try { return raw == null ? null : JSON.parse(raw); } catch (_) { return null; }
  }

  function create(getStorage) {
    let memory = null, volatile = false;
    function storage() {
      try { return typeof getStorage === 'function' ? getStorage() : getStorage; }
      catch (_) { return null; }
    }
    function get(store, key) {
      try {
        if (!store) throw new Error('storage unavailable');
        return store.getItem(key) ?? null;
      } catch (_) { volatile = true; return null; }
    }
    function write(value) {
      memory = clean(value);
      if (volatile) return clone(memory);
      try {
        const store = storage();
        if (!store) throw new Error('storage unavailable');
        store.setItem(KEY, JSON.stringify(memory));
      } catch (_) { volatile = true; }
      return clone(memory);
    }
    function read() {
      if (volatile) return clone(memory || (memory = clean({})));
      const store = storage();
      const saved = parse(get(store, KEY));
      if (volatile) return clone(memory || (memory = clean({})));
      if (saved && typeof saved === 'object' && !Array.isArray(saved) && saved.version === 1) {
        memory = clean(saved);
        return clone(memory);
      }
      if (memory) return clone(memory);
      const oldTarget = get(store, WORKBENCH_KEY);
      const oldQuick = parse(get(store, QUICK_KEY));
      if (volatile) return clone(memory || (memory = clean({})));
      const recent = idList(oldQuick?.recent, MAX_RECENT);
      const selected = validId(oldTarget) ? oldTarget : (recent[0] || '');
      return write({
        selected,
        favorites: idList(oldQuick?.favorite, MAX_FAVORITES),
        recent: idList([selected, ...recent], MAX_RECENT),
      });
    }
    function select(id) {
      if (id !== '' && !validId(id)) return read();
      const value = read();
      value.selected = id;
      if (id) value.recent = idList([id, ...value.recent], MAX_RECENT);
      return write(value);
    }
    function toggleFavorite(id) {
      const value = read();
      if (!validId(id)) return value;
      value.favorites = value.favorites.includes(id)
        ? value.favorites.filter(existing => existing !== id)
        : idList([id, ...value.favorites], MAX_FAVORITES);
      return write(value);
    }
    function markUsed(id) {
      const value = read();
      if (!validId(id)) return value;
      value.recent = idList([id, ...value.recent], MAX_RECENT);
      return write(value);
    }
    function resolve(peerIds) {
      const value = read();
      const available = Array.isArray(peerIds) ? peerIds.filter(validId) : [];
      const selected = available.includes(value.selected) ? value.selected : '';
      return { ...value, selected, missing: Boolean(value.selected && !selected) };
    }
    return Object.freeze({ key: KEY, isValidId: validId, read, select, toggleFavorite, markUsed, resolve });
  }

  const api = Object.freeze({ create, KEY, WORKBENCH_KEY, QUICK_KEY });
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root?.document) root.PeerCarryDevicePreferences = create(() => root.localStorage);
})(globalThis);
