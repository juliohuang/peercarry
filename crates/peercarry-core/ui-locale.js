// The desktop shell supplies the computer's display language before page scripts.
// A separately opened browser uses its own first preferred language instead.
(function (root) {
  'use strict';
  const native = root.__PEERCARRY_LANG__;
  const browser = root.navigator?.languages?.[0] || root.navigator?.language || 'en';
  const tag = native === 'zh' || native === 'en' ? native : browser;
  const isChinese = typeof tag === 'string' && /^zh(?:[-_.@]|$)/i.test(tag.trim());
  const language = isChinese ? 'zh' : 'en';
  const locale = isChinese ? 'zh-CN' : 'en';
  root.PeerCarryLocale = Object.freeze({ language, locale, isChinese });
  if (root.document?.documentElement) root.document.documentElement.lang = locale;
})(globalThis);
