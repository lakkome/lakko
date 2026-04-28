// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ciphertext blob storage proxy for Lakko.
//!
//! Sits in front of SeaweedFS (or compatible S3 backend), enforcing
//! content-addressable upload and ciphertext-only policy. Server cannot
//! prove ciphertext-ness; clients verify integrity on download.
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (Object storage).

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

    tracing::info!("storage service starting (Phase 0 stub)");

    Ok(())
}
