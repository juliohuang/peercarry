import { TEST_ORIGIN } from './fill.ts';
import type { Binding, Target } from './fill.ts';

/** Test-page-only adapter. Production tab/document IDs must come from extension APIs. */
export function bindTestInput(document: Document, binding: Binding): Target {
  const expectedTypes = { account: 'text', password: 'password', otp: 'text' };
  const bound = Object.freeze({ ...binding });
  const element = document.getElementById(bound.fieldId);
  function valid(): boolean {
    const view = document.defaultView;
    return !!view && view.document === document && view.top === view &&
      document.location.origin === TEST_ORIGIN && bound.origin === TEST_ORIGIN &&
      bound.frameId === 0 && element instanceof view.HTMLInputElement &&
      element.ownerDocument === document && element.isConnected &&
      document.getElementById(bound.fieldId) === element &&
      element.type === expectedTypes[bound.fieldKind] &&
      element.dataset.loginField === bound.fieldKind &&
      !element.disabled && !element.readOnly && element.getClientRects().length > 0;
  }
  return {
    snapshot: () => valid() ? { ...bound } : null,
    fill(value) {
      if (!valid()) throw new Error('STALE_TARGET');
      const view = document.defaultView!;
      const setter = Object.getOwnPropertyDescriptor(view.HTMLInputElement.prototype, 'value')?.set;
      if (!setter) throw new Error('UNSUPPORTED_INPUT');
      setter.call(element, value);
      element!.dispatchEvent(new view.Event('input', { bubbles: true }));
      element!.dispatchEvent(new view.Event('change', { bubbles: true }));
    },
  };
}
