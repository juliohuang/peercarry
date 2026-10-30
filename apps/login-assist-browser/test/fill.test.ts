import test from 'node:test';
import assert from 'node:assert/strict';
import { FillSession, TEST_ORIGIN } from '../src/fill.ts';
import type { Binding, FillCommand, Target } from '../src/fill.ts';
const binding: Binding = { tabId: 1, documentId: 'doc-1', frameId: 0,
  origin: TEST_ORIGIN, fieldId: 'password', fieldKind: 'password', epoch: 1 };
function fixture() {
  let now = 1000;
  let writes = 0;
  let received = '';
  let actual: Binding = { ...binding };
  const session = new FillSession({ id: 'grant', expiresAt: 2000, binding }, () => now);
  const command: FillCommand = { commandId: 'cmd', grantId: 'grant', binding, value: 'synthetic-test-only' };
  const target: Target = { snapshot: () => actual, fill: value => { writes++; received = value; } };
  return { session, command, target, writes: () => writes, received: () => received,
    setNow: (value: number) => { now = value; },
    setActual: (value: Partial<Binding>) => { actual = { ...actual, ...value }; } };
}
test('matching grant fills once without returning input', () => {
  const f = fixture();
  assert.deepEqual(f.session.execute(f.command, f.target), { ok: true });
  assert.equal(f.received(), f.command.value);
  assert.deepEqual(f.session.execute(f.command, f.target), { ok: false, code: 'ALREADY_USED' });
  assert.equal(f.writes(), 1);
});
for (const [key, value] of Object.entries({ tabId: 2, documentId: 'doc-2', frameId: 2,
  origin: 'https://example.com', fieldId: 'other', fieldKind: 'account', epoch: 2 })) {
  test(`changed trusted ${key} invalidates the grant`, () => {
    const f = fixture(); f.setActual({ [key]: value });
    assert.deepEqual(f.session.execute(f.command, f.target), { ok: false, code: 'STALE_TARGET' });
    assert.equal(f.writes(), 0);
  });
}
test('old command binding is rejected', () => {
  const f = fixture();
  assert.deepEqual(f.session.execute({ ...f.command, binding: { ...binding, epoch: 0 } }, f.target),
    { ok: false, code: 'STALE_TARGET' });
});
test('expiry and backwards clock fail closed', () => {
  for (const time of [2000, 999]) {
    const f = fixture(); f.setNow(time);
    assert.deepEqual(f.session.execute(f.command, f.target), { ok: false, code: 'EXPIRED' });
    assert.equal(f.writes(), 0);
  }
});
test('revocation rejects further input', () => {
  const f = fixture(); f.session.revoke();
  assert.deepEqual(f.session.execute(f.command, f.target), { ok: false, code: 'REVOKED' });
});
test('failed fill cannot be replayed, errors contain no input', () => {
  const f = fixture(); f.target.fill = () => { throw new Error(f.command.value); };
  assert.deepEqual(f.session.execute(f.command, f.target), { ok: false, code: 'FILL_FAILED' });
  assert.deepEqual(f.session.execute({ ...f.command, commandId: 'another' }, f.target),
    { ok: false, code: 'ALREADY_USED' });
});
test('grant must be short lived and test origin only', () => {
  assert.throws(() => new FillSession({ id: 'g', expiresAt: 61001, binding }, () => 1000));
  assert.throws(() => new FillSession({ id: 'g', expiresAt: 2000,
    binding: { ...binding, origin: 'https://example.com' } }, () => 1000));
});
