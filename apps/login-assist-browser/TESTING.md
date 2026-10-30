# Browser and mobile client validation

Evidence snapshot: 2026-09-19. This is prototype evidence from synthetic values, not acceptance for real credentials or production deployment.

## Automated unit checks

Requirements: Node 22.18 or newer.

```powershell
cd apps/login-assist-browser
npm test
npm run build:extension
```

The tests cover one-time binding, target/epoch checks, expiry, revocation,
replay, input failure, and field binding. The build copies the pinned QR module
into the extension's vendor directory. These checks alone do not exercise
Chrome or a phone.

## Windows Chromium Native Messaging E2E

The integration runner is `test/native-e2e.mjs`. It requires a Windows test
environment with Playwright, Chromium, and `target/debug/peercarry-login-host.exe`.
Set `PLAYWRIGHT_MODULE` and `CHROMIUM_EXECUTABLE`; set `DART_EXECUTABLE` to also
run the real Dart client against the host. The runner creates a disposable
profile. Set `HOST_BINARY` to an absolute release executable path to test that
build instead of the default debug executable. The runner creates a temporary
Chromium profile and native host registration and removes them at exit. It
refuses to overwrite existing Chrome or Chromium Native Messaging registrations.

Headless Chromium cannot click the toolbar action to grant the release
extension's `activeTab` access. For this automated run only, the runner copies
the extension under `target/login-assist-e2e` and adds `<all_urls>` to that
temporary manifest. This does not modify the source or built release
manifest: `extension/manifest.json` contains no `host_permissions`.

The reported real Chromium E2E passed with synthetic data. It exercised:

- extension-to-host Native Messaging and a TLS service bound to loopback;
- one-time pairing, rejection of a wrong secret, and the paired Dart client's
  pinned TLS connection when `DART_EXECUTABLE` is configured;
- targeted password DOM fill and input event, with replay rejection;
- actual browser capture and crop bounds, with an outside-region marker absent;
- layout change invalidation, explicit reapproval, separation of viewing and
  click scopes, out-of-scope click rejection, and hand-back revocation.

The host completion state has `ai_resumed: false`. The E2E does not establish
cross-process executor acknowledgement, external AI pause/resume, or successful
website authentication.

## Mobile and remaining acceptance

The Dart client was exercised as a separate process against the local TLS host;
its ticket is passed on stdin and the client reports only `PASS` or a redacted
error code. This validates the Dart TLS client path, not a real mobile device.

Not yet accepted: QR scanning with a real camera, Android/iPhone device UI,
phone-to-PC network access, Mac capture, real-site login, external AI hand-back,
and actual user-invoked toolbar activeTab permissions. The Windows release
executable and Android debug APK were rebuilt; the APK is not a release package.
User has deferred real phone/camera/cross-device and
Mac/iPhone acceptance. Do not enter real credentials during local validation.

## Native lock recovery

Run `node apps/login-assist-browser/test/native-lock.mjs` from the repository root.
This Windows-only test copies the debug executable into a unique disposable
workspace directory. It verifies active-lock recovery refusal, concurrent-host
refusal, normal EOF cleanup, forced termination of its own test process followed
by explicit stale-lock recovery, and a fresh start. It never starts a network
listener or touches the user's installed host. All five paths passed locally.
