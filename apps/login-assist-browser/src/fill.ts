/** P0 deterministic validator. No transport, recorder, clipboard or extension entry point. */
export const TEST_ORIGIN = 'http://127.0.0.1:5188';
export type FieldKind = 'account' | 'password' | 'otp';
export type Binding = Readonly<{
  tabId: number;
  documentId: string;
  frameId: number;
  origin: string;
  fieldId: string;
  fieldKind: FieldKind;
  epoch: number;
}>;
export type Grant = Readonly<{ id: string; expiresAt: number; binding: Binding }>;
export type FillCommand = Readonly<{
  commandId: string;
  grantId: string;
  binding: Binding;
  value: string;
}>;
export type FillResult = { ok: true } | { ok: false; code: string };
export interface Target {
  /** Must obtain fresh metadata from a trusted browser adapter, never page messages. */
  snapshot(): Binding | null;
  fill(value: string): void;
}
const keys: (keyof Binding)[] = [
  'tabId', 'documentId', 'frameId', 'origin', 'fieldId', 'fieldKind', 'epoch',
];
function matches(a: Binding, b: Binding): boolean {
  return keys.every(key => a[key] === b[key]);
}
function validBinding(b: Binding): boolean {
  return Number.isSafeInteger(b.tabId) && b.tabId >= 0 && b.frameId === 0 &&
    Number.isSafeInteger(b.epoch) && b.epoch >= 0 && b.origin === TEST_ORIGIN &&
    typeof b.documentId === 'string' && b.documentId.length > 0 &&
    typeof b.fieldId === 'string' && b.fieldId.length > 0 &&
    ['account', 'password', 'otp'].includes(b.fieldKind);
}

/** One grant allows one attempted fill. Request a new grant for another field or retry. */
export class FillSession {
  #grant: Grant;
  #revoked = false;
  #used = false;
  #now: () => number;
  #lastNow: number;
  constructor(grant: Grant, now: () => number = Date.now) {
    const current = now();
    if (!grant.id || !validBinding(grant.binding) || !Number.isFinite(current) ||
        !Number.isFinite(grant.expiresAt) || grant.expiresAt <= current ||
        grant.expiresAt - current > 60_000) throw new Error('INVALID_GRANT');
    this.#grant = Object.freeze({ ...grant, binding: Object.freeze({ ...grant.binding }) });
    this.#now = now;
    this.#lastNow = current;
  }
  revoke(): void { this.#revoked = true; }
  execute(command: FillCommand, target: Target): FillResult {
    const fail = (code: string): FillResult => ({ ok: false, code });
    if (this.#revoked) return fail('REVOKED');
    const now = this.#now();
    if (!Number.isFinite(now) || now < this.#lastNow || now >= this.#grant.expiresAt) {
      this.revoke();
      return fail('EXPIRED');
    }
    this.#lastNow = now;
    if (this.#used) return fail('ALREADY_USED');
    if (!command.commandId || command.grantId !== this.#grant.id) return fail('INVALID_COMMAND');
    if (!validBinding(command.binding) || !matches(command.binding, this.#grant.binding)) {
      this.revoke();
      return fail('STALE_TARGET');
    }
    let actual: Binding | null;
    try { actual = target.snapshot(); } catch { actual = null; }
    if (!actual || !matches(actual, this.#grant.binding)) {
      this.revoke();
      return fail('STALE_TARGET');
    }
    if (typeof command.value !== 'string' || command.value.length === 0 || command.value.length > 4096) {
      return fail('INVALID_VALUE');
    }
    // Consume before dispatch: the browser event handler may throw or reenter.
    this.#used = true;
    try { target.fill(command.value); } catch { return fail('FILL_FAILED'); }
    // DOM event dispatch is not proof of website login or task resumption.
    return { ok: true };
  }
}
