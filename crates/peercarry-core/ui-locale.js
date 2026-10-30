// The service and tray share the computer's UI language and saved local preference.
(function (root) {
  'use strict';
  const KEY = 'peercarry.ui.language.changed';
  const chinese = tag => typeof tag === 'string' && /^zh(?:[-_.@]|$)/i.test(tag.trim());
  const bootstrap = root.__PEERCARRY_LOCALE__;
  const native = root.__PEERCARRY_LANG__;
  const browser = root.navigator?.languages?.[0] || root.navigator?.language || 'en';
  const hasBootstrap = bootstrap && typeof bootstrap === 'object';
  const preference = hasBootstrap && ['system', 'zh', 'en'].includes(bootstrap.preference) ? bootstrap.preference : 'system';
  const systemLanguage = chinese(hasBootstrap ? bootstrap.system_language : (native === 'zh' || native === 'en' ? native : browser)) ? 'zh' : 'en';
  const language = preference === 'system' ? systemLanguage : preference;
  const isChinese = language === 'zh', locale = isChinese ? 'zh-CN' : 'en';
  let guard = null, checking = false, pending = false, reloading = false, composing = false;

  async function checkForChange() {
    if (checking || reloading || typeof root.fetch !== 'function') return false;
    checking = true;
    const controller = typeof root.AbortController === 'function' ? new root.AbortController() : null;
    const timeout = controller ? root.setTimeout?.(() => controller.abort(), 1500) : null;
    try {
      const response = await root.fetch('/v1/desktop/language', { cache: 'no-store', redirect: 'error', ...(controller ? { signal: controller.signal } : {}) });
      if (!response.ok) return false;
      const next = await response.json();
      if (!next || !['zh', 'en'].includes(next.language)) return false;
      if (next.language === language) { pending = false; return true; }
      pending = true;
      if (composing || !guard || await guard(next) === false || composing) return false;
      if (typeof root.location?.reload !== 'function') return false;
      reloading = true;
      root.location.reload();
      return true;
    } catch (_) {
      reloading = false;
      return false;
    } finally { root.clearTimeout?.(timeout); checking = false; }
  }
  const retry = () => pending ? checkForChange() : Promise.resolve(false);
  const beforeReload = callback => { guard = callback; return retry(); };
  function notifyChange() {
    try { root.localStorage?.setItem(KEY, Date.now() + ':' + Math.random()); } catch (_) {}
    try { root.ipc?.postMessage?.('language-changed'); } catch (_) {}
    return checkForChange();
  }
  root.addEventListener?.('focus', checkForChange);
  root.addEventListener?.('storage', event => { if (event.key === KEY) checkForChange(); });
  root.document?.addEventListener?.('compositionstart', () => { composing = true; });
  root.document?.addEventListener?.('compositionend', () => { composing = false; root.setTimeout?.(retry, 0); });
  root.PeerCarryLocale = Object.freeze({ language, locale, isChinese, preference, systemLanguage,
    beforeReload, checkForChange, notifyChange, retry });
  if (root.document?.documentElement) root.document.documentElement.lang = locale;
})(globalThis);
