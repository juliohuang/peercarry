//! Core library for peercarry.
//!
//! peercarry is a *manual, peer-to-peer* clipboard bridge for machines joined
//! to the same Tailscale tailnet. Nothing happens unless you ask for it:
//! there is no background clipboard watcher, so two machines can never fight
//! over the clipboard.
//!
//! # Model
//!
//! * Every machine runs the same daemon and keeps its **own** history.
//! * Peers are discovered with `tailscale status --json`.
//! * A UI (tray or CLI) merges the histories into one aggregated list.
//! * Pulling an entry fetches its bytes from the machine that captured it -
//!   large payloads are therefore never pushed around speculatively.

pub mod client;
pub mod clipboard;
pub mod config;
pub mod engine;
pub mod error;
pub mod maintenance;
pub mod model;
pub mod paths;
pub mod protocol;
pub mod server;
pub mod store;
pub mod tailscale;
pub mod uploads;

pub use engine::Engine;
pub use error::{Error, Result};
pub mod ai_monitor;
pub mod ai_tasks;
