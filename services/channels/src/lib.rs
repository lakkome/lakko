// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-channels` — channels backend for Phase 4a
//! (publisher path; see ADR-0010).
//!
//! This crate is shipped both as a binary (`src/main.rs`)
//! and as a library (this file) so the integration tests
//! under `tests/` can construct the router without going
//! through the full `main` entry point. Same lib/bin split
//! as `services/sfu`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod config;
pub mod sql_store;
pub mod storage;

pub use api::{router, AppState};
pub use config::{Config, ConfigError};
pub use sql_store::SqlChannelStore;
pub use storage::{
    ChannelId, ChannelStore, InMemoryChannelStore, StorageError, StoredPost, SubscriberId,
};

/// Service version pulled from the crate manifest at
/// compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
