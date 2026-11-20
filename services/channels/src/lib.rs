// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-channels` — the channel-broadcast backend (ADR-0010) and the
//! channel directory (ADR-0059).
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
pub mod directory;
pub mod feedback;
pub mod feedback_api;
mod feedback_sql;
pub mod internal;
pub mod metrics;
pub mod owner_auth;
pub mod sql_store;
pub mod storage;

pub use api::{router, AppState};
pub use config::{Config, ConfigError};
pub use feedback::{PostReactionCounts, StoredComment};
pub use sql_store::SqlChannelStore;
pub use storage::{
    ChannelId, ChannelStore, DirectoryQuery, InMemoryChannelStore, InviteKeyRow, ModerationAction,
    PublicSeedOutcome, SealedTicket, StorageError, StoredListing, StoredPost, SubscriberEntry,
    SubscriberId, INVITE_USES_UNLIMITED, PUBLIC_SEED_LEN, SEALED_TICKET_LEN,
};

/// The service's migrations, for `main` and the DB-gated tests.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Service version pulled from the crate manifest at
/// compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
