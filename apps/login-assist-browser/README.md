# PeerCarry Login Assist browser extension

This directory builds a Chrome MV3 prototype that connects to the dedicated
Rust Native Messaging host. The extension checks browser tab/document/frame/
origin metadata, performs one-shot targeted field fills and requests a browser
capture that the host crops to the approved region. Page changes invalidate the
old authorization. It does not connect to the clipboard service or expose an
AI command receiver.

The shipped manifest requests `activeTab`, `scripting`, `nativeMessaging` and
`webNavigation`; it has no `host_permissions`. The user must invoke the
extension action to grant access to the active tab. The headless E2E test uses
a disposable extension copy with `<all_urls>` because it cannot click the
browser action. That temporary permission is not part of the build output.

## Build and install

Requirements: Node.js 22.18 or newer, stable Rust, Chrome, and Flutter for the
Android debug client. Build and registration commands are documented in
[`docs/login-assist/windows-setup.md`](../../docs/login-assist/windows-setup.md).
The installer requires the actual unpacked extension ID and host binary path;
it writes only the current-user Native Messaging registration. It does not
change firewall rules or ports 80/443.

## Verification

```sh
npm test
npm run build:extension
```

See [TESTING.md](TESTING.md) for the Windows Chromium integration evidence and
its limits. Tests use synthetic values only. The Dart client has been exercised
against the local TLS host; phone camera scanning and cross-device networking
remain unverified. The host's manual hand-back reports `ai_resumed: false` until
the cooperative barrier is connected across processes and to an executor.
