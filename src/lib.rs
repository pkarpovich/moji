//! moji owns keyboard layout switching on this Mac.
//!
//! The crate is a library so that `tests/live.rs`, which needs the process's own main thread
//! because AppKit refuses an `NSWindow` anywhere else, drives exactly the modules the `moji`
//! binary runs.

pub mod barrier;
pub mod config;
pub mod daemon;
pub mod macos;
pub mod memory;
