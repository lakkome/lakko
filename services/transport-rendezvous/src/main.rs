// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transport rendezvous service for Lakko.
//!
//! Serves Ed25519-signed bootstrap configs (current endpoints + working
//! pluggable transports). Distributed via DNS-over-HTTPS-resolvable rotating
//! domain pool, ECH-protected. Configs ship as TUF-signed data so new
//! transports can be deployed without code release.
//!
//! See `~/.claude/plans/1-nested-wreath.md` §6 and ADR-0004.

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

    tracing::info!("transport-rendezvous service starting (Phase 0 stub)");

    Ok(())
}
