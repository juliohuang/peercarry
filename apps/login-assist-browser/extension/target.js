// Serialized by chrome.scripting into the extension's isolated world.
export function targetAction(action) {
  if (window.top !== window || !(location.protocol === 'https:' || location.origin === 'http://127.0.0.1:5188')) throw new Error('UNSUPPORTED_TARGET');
  const key = '__peercarryPrivateLogin';
  if (!globalThis[key]) {
    const state = { ids: new WeakMap(), fields: new Map(), signature: '', revision: '', revoked: false };
    globalThis[key] = state;
    const changed = () => {
      state.signature = '';
      state.revision = crypto.randomUUID();
      chrome.runtime.sendMessage({ type: 'target_changed' }).catch(() => {});
    };
    addEventListener('scroll', changed, true);
    addEventListener('resize', changed);
    new MutationObserver(changed).observe(document.documentElement, {
      subtree: true, childList: true, attributes: true,
      attributeFilter: ['class', 'style', 'type', 'disabled', 'readonly', 'hidden', 'id'],
    });
  }
  const state = globalThis[key];
  if (action.op === 'revoke') { state.revoked = true; state.fields.clear(); return {}; }
  if (state.revoked && action.op !== 'inspect') throw new Error('REVOKED');
  const visible = element => {
    const style = getComputedStyle(element);
    const r = element.getBoundingClientRect();
    return element.isConnected && !element.matches(':disabled') && !element.readOnly &&
      style.visibility === 'visible' && style.display !== 'none' && Number(style.opacity) > 0 &&
      r.width > 0 && r.height > 0 && r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight;
  };
  const fields = [];
  const geometry = [];
  state.fields.clear();
  for (const input of document.querySelectorAll('input')) {
    if (!visible(input)) continue;
    let kind;
    if (input.type === 'password') kind = 'password';
    else if (input.autocomplete === 'one-time-code' || input.dataset.loginField === 'otp') kind = 'otp';
    else if (['text', 'email', 'tel'].includes(input.type)) kind = 'account';
    else continue;
    if (!state.ids.has(input)) state.ids.set(input, crypto.randomUUID());
    const id = state.ids.get(input);
    state.fields.set(id, input);
    fields.push({ id, kind });
    const r = input.getBoundingClientRect();
    geometry.push([id, kind, input.type, r.x, r.y, r.width, r.height]);
    if (fields.length >= 64) break;
  }
  const password = [...state.fields.values()].find(i => i.type === 'password');
  const form = (password || state.fields.values().next().value)?.closest('form');
  const elements = form ? [form] : [...state.fields.values()];
  if (!elements.length) throw new Error('NO_SUPPORTED_FIELDS');
  const bounds = elements.map(e => e.getBoundingClientRect());
  // Round inward: fractional layout edges must not include a neighboring pixel.
  const x = Math.max(0, Math.ceil(Math.min(...bounds.map(r => r.left))));
  const y = Math.max(0, Math.ceil(Math.min(...bounds.map(r => r.top))));
  const right = Math.min(innerWidth, Math.floor(Math.max(...bounds.map(r => r.right))));
  const bottom = Math.min(innerHeight, Math.floor(Math.max(...bounds.map(r => r.bottom))));
  const view = { x, y, width: right - x, height: bottom - y };
  const viewport = { x: 0, y: 0, width: innerWidth, height: innerHeight };
  const signature = JSON.stringify([location.href, scrollX, scrollY, devicePixelRatio, viewport, view, geometry]);
  if (signature !== state.signature) { state.signature = signature; state.revision = crypto.randomUUID(); }
  if (action.op === 'inspect') { state.revoked = false; return { origin: location.origin, view, viewport, fields, revision: state.revision }; }
  if (action.revision !== state.revision) throw new Error('STALE_CAPTURE');
  if (action.op === 'fill') {
    const element = state.fields.get(action.field);
    if (!element || !visible(element) || typeof action.value !== 'string' || !action.value.length || action.value.length > 4096) throw new Error('INVALID_FIELD');
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(element, action.value);
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return {};
  }
  if (action.op === 'click') {
    if (!Number.isInteger(action.x) || !Number.isInteger(action.y) || action.x < view.x || action.y < view.y || action.x >= view.x + view.width || action.y >= view.y + view.height) throw new Error('OUT_OF_SCOPE');
    const element = document.elementFromPoint(action.x, action.y);
    if (!element || element.closest('iframe') || element.matches(':disabled')) throw new Error('UNSUPPORTED_CLICK');
    element.click();
    return {};
  }
  if (action.op === 'validate') return { revision: state.revision, viewport };
  throw new Error('UNSUPPORTED_OPERATION');
}
