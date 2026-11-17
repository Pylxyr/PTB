//! Dependency-free building blocks shared by the PyxeeTB binaries.
//!
//! Everything here is pure: text in, numbers or text out. That keeps it testable without a network, a
//! Twitch account or a Linux machine, which is what lets CI check it on every change.

pub mod procfs;
pub mod report;
pub mod stats;

/// The version every release file agrees on (`scripts/bump_version.py` keeps them in step).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
