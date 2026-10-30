import { targetAction } from './target.js';
let native = null;
let target = null;
let ticket = null;
let lastError = '';
let busy = false;
let stopped = true;
const browser = crypto.randomUUID();

async function inject(tabId, action, documentId) {
  const results = await chrome.scripting.executeScript({
    target: documentId ? { tabId, documentIds: [documentId] } : { tabId, frameIds: [0] },
    func: targetAction, args: [action], world: 'ISOLATED',
  });
  const result = results.find(r => r.frameId === 0);
  if (!result?.result || !result.documentId) throw new Error('TARGET_CHANGED');
  return result;
}
async function inspect(tabId) {
  const result = await inject(tabId, { op: 'inspect' });
  return { ...result.result, target: {
    device: 'local-browser-host', browser, tab: tabId, document: result.documentId,
    frame: 0, origin: result.result.origin,
  } };
}
function snapshotWire(snapshot) {
  const { origin, ...wire } = snapshot;
  return wire;
}
async function revokeLocal() {
  const previous = target;
  stopped = true; ticket = null; target = null;
  if (previous) {
    try { await inject(previous.tab, { op: 'revoke' }, previous.document); } catch { /* The document may already be gone. */ }
  }
}
async function handleBrowser(request) {
  if (stopped || !target || request.target.tab !== target.tab || request.target.browser !== browser) throw new Error('TARGET_CHANGED');
  if (request.op === 'revoke') {
    await revokeLocal();
    return {};
  }
  if (request.op === 'inspect') {
    const snapshot = await inspect(target.tab);
    target = snapshot.target;
    return snapshotWire(snapshot);
  }
  if (request.target.document !== target.document || request.target.origin !== target.origin) throw new Error('TARGET_CHANGED');
  if (request.op === 'capture') {
    const tab = await chrome.tabs.get(target.tab);
    const [active] = await chrome.tabs.query({ active: true, windowId: tab.windowId });
    if (active?.id !== target.tab || tab.status !== 'complete') throw new Error('INACTIVE_TAB');
    const before = await inject(target.tab, { op: 'validate', revision: request.revision }, target.document);
    const data = await chrome.tabs.captureVisibleTab(tab.windowId, { format: 'png' });
    const after = await inject(target.tab, { op: 'validate', revision: request.revision }, target.document);
    const [stillActive] = await chrome.tabs.query({ active: true, windowId: tab.windowId });
    if (stopped || stillActive?.id !== target.tab || JSON.stringify(before.result) !== JSON.stringify(after.result)) throw new Error('TARGET_CHANGED');
    return { png: data.slice('data:image/png;base64,'.length) };
  }
  if (!['fill', 'click'].includes(request.op)) throw new Error('UNSUPPORTED_OPERATION');
  const result = await inject(target.tab, request, target.document);
  return result.result;
}
function connect() {
  const port = chrome.runtime.connectNative('dev.peercarry.login_assist');
  native = port;
  port.onDisconnect.addListener(() => {
    const reason = chrome.runtime.lastError?.message || '';
    if (native !== port) return;
    native = null;
    void revokeLocal();
    lastError = reason.includes('not found') ? 'HOST_NOT_INSTALLED' : 'HOST_DISCONNECTED';
  });
  port.onMessage.addListener(message => {
    if (native !== port) return;
    if (message.event === 'started') {
      ticket = message.ticket;
      setTimeout(() => { if (native === port) ticket = null; }, 60_000);
    }
    if (message.event === 'error') lastError = message.code;
    if (message.event !== 'browser') return;
    // Serial host requests are also serialized at the browser boundary.
    if (busy) { native?.postMessage({ op: 'reply', reply_id: message.request_id, ok: false, data: null }); return; }
    busy = true;
    handleBrowser(message.payload).then(
      data => { if (native === port) port.postMessage({ op: 'reply', reply_id: message.request_id, ok: true, data }); },
      () => { if (native === port) port.postMessage({ op: 'reply', reply_id: message.request_id, ok: false, data: null }); },
    ).finally(() => { busy = false; });
  });
}
function invalidate() { ticket = null; native?.postMessage({ op: 'invalidate' }); }
chrome.webNavigation.onCommitted.addListener(details => {
  if (target && details.tabId === target.tab && details.frameId === 0) invalidate();
});
chrome.tabs.onActivated.addListener(info => { if (target && info.tabId !== target.tab) invalidate(); });
chrome.tabs.onRemoved.addListener(tabId => { if (target?.tab === tabId) { stopped = true; native?.postMessage({ op: 'stop' }); } });
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id) return;
  if (sender.url !== chrome.runtime.getURL('popup.html') && sender.tab) {
    if (sender.tab.id === target?.tab && message.type === 'target_changed') invalidate();
    return;
  }
  if (sender.url !== chrome.runtime.getURL('popup.html')) return;
  (async () => {
    if (message.type === 'status') return { connected: !!native, ticket, origin: target?.origin, error: lastError };
    if (message.type === 'stop') {
      await revokeLocal();
      native?.postMessage({ op: 'stop' }); native?.disconnect(); native = null;
      return { ok: true };
    }
    if (message.type === 'start') {
      if (native || busy) throw new Error('ALREADY_RUNNING');
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      if (!tab?.id) throw new Error('NO_TAB');
      const snapshot = await inspect(tab.id);
      target = snapshot.target; stopped = false; lastError = '';
      connect();
      native.postMessage({ op: 'start', bind: message.bind, snapshot: snapshotWire(snapshot) });
      return { ok: true };
    }
    // No unverified executor acknowledgement or arbitrary AI/browser command path.
    throw new Error('PAUSE_UNSUPPORTED');
  })().then(respond, () => { lastError = 'OPERATION_UNAVAILABLE'; respond({ error: lastError }); });
  return true;
});
