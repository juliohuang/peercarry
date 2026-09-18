# Repository Guidelines

## Project Structure & Module Organization

This Rust 2021 workspace implements a manual, peer-to-peer clipboard bridge over Tailscale.

- `crates/peercarry-core/src/`: shared models, redb storage, transfer engine, HTTP client/server, configuration, and peer discovery. Platform clipboard adapters live in `src/clipboard/`.
- `crates/peercarry-cli/src/main.rs`: the `peercarry` command-line application.
- `crates/peercarry-tray/src/`: desktop tray application and `i18n.rs` translations.
- `crates/peercarry-core/timeline.html`: embedded timeline/settings UI; no separate frontend build.
- Tests are inline Rust modules. `target/` contains generated build artifacts and is ignored.

## Build, Test, and Development Commands

Run commands from the repository root:

- `cargo build --release`: build both applications into `target/release/`.
- `cargo build -p peercarry-cli`: build only the CLI.
- `cargo run -p peercarry-cli -- --help`: inspect CLI options.
- `cargo run -p peercarry-cli -- serve`: start the daemon.
- `cargo run -p peercarry-tray`: launch the tray with its embedded service.
- `cargo test --workspace`: run workspace tests.
- `cargo fmt --all -- --check`: check Rust formatting.
- `cargo clippy --workspace --all-targets`: inspect lint diagnostics.

Use stable Rust; Windows requires MSVC and Visual Studio build tools. Linux tray builds require GTK, libxdo, and WebKitGTK development libraries listed in `README.md`. Cross-device checks require authenticated Tailscale peers. Avoid simultaneous daemon/tray instances sharing one redb store.

## Coding Style & Naming Conventions

Use rustfmt defaults, four-space Rust indentation, `snake_case` functions/modules, `PascalCase` types, and `SCREAMING_SNAKE_CASE` constants. Keep shared behavior in core and OS-specific code behind target configuration. Follow existing error types and tracing patterns. Update both supported languages when changing tray labels.

## Testing Guidelines

Use Rust's built-in `#[test]` harness within `#[cfg(test)] mod tests`. Name tests after behavior, such as `same_text_fingerprints_equal`. Run focused checks with `cargo test -p peercarry-core`. No numeric coverage threshold is configured. Add regression tests for changed behavior; report manual clipboard, tray, and peer-transfer checks separately, including untested platforms.

## Commit & Pull Request Guidelines

History mixes descriptive subjects with `feat:`, `feat(core):`, and `chore:` prefixes; prefer these prefixes with concise, specific subjects. PRs should describe behavior changes, link applicable issues, list validation and platform limitations, and include screenshots for timeline/tray changes.

## Security & Architecture Constraints

Preserve explicit capture/pull actions: receiving an entry must not automatically overwrite the clipboard. Keep large payloads fetched on demand. Preserve loopback restrictions on sensitive endpoints and entry-based file access. Never commit authentication tokens, clipboard contents, or local databases.
