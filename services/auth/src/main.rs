// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Auth service for Lakko.
//!
//! Implements OPAQUE-PAKE password-based auth, WebAuthn passkey, and recovery
//! flows (BIP39 / Shamir 3-of-5 social recovery). No phone or email is ever
//! persisted.
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (Auth).

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

    tracing::info!("auth service starting (Phase 0 stub)");

    Ok(())
}
