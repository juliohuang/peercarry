# Login assist prototype

This crate contains the login-assist policy, a dedicated TLS service, and the
Native Messaging host binary. It is not wired into the tray, existing clipboard
HTTP API, MCP or an external AI runner. See the [implementation plan](../../docs/login-assist/implementation-plan.md),
[current status](../../docs/login-assist/p0-status.md) and [Windows setup](../../docs/login-assist/windows-setup.md).

The caller supplies authenticated identities, browser-verified target metadata
and a monotonic host clock. IDs and epochs are correlation values, not
authentication credentials. Adapter acknowledgement methods are trusted local
calls, not endpoints to expose to an AI or browser page.

Credential values do not enter the policy engine or clipboard. The TLS host
accepts a one-time pairing secret, issues a session token and serves only while
the user-approved host session is active. Pairing data must not be logged or
saved in documentation. The caller pins the host certificate. The crop helper
returns only the approved region to the phone. The extension sends a transient
full-tab capture to the local host for cropping; neither persists it.

The policy's device lease remains held until acknowledged hand-back. This is
not an operating-system lock and cannot stop unrelated automation or screenshot tools.
Manual mode reports `ai_resumed: false`. The local cooperative automation
barrier is not connected to this host across processes or to an external AI
environment, so this prototype does not prove AI pause/resume integration.
Faulted sessions remain locked. The explicit `--recover-lock` operation is for
local stale-lock recovery only and must refuse while a host process holds the
lock. Do not use this prototype with real passwords or treat it as a production
service.

```sh
cargo test -p peercarry-login-assist
cargo run -p peercarry-login-assist --example simulated_handoff
```
