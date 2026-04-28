// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS Delivery Service for Lakko.
//!
//! Append-only per-group log with FIFO ordering by `(epoch, seq)`.
//! Stateless front, stateful storage layer (FoundationDB). Per-device
//! mailboxes with cursor pull and QUIC long-stream push. Server stores
//! ciphertext only — no message keys, no chain keys, no plaintext.
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (Delivery Service).

#![forbid(unsafe_code)]

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("ds service starting (Phase 0 stub)");

    Ok(())
}
