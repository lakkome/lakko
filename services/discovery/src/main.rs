// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Discovery service for Lakko.
//!
//! Implements username lookup via OPRF (RFC 9497) blinded queries — server
//! never sees plaintext usernames. Returns KeyPackage bundles + Key
//! Transparency log inclusion proofs. Per-IP and per-account PoW rate
//! limiting; exact-match only (no prefix or fuzzy search).
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (Discovery) and ADR-0005.

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

    tracing::info!("discovery service starting (Phase 0 stub)");

    Ok(())
}
