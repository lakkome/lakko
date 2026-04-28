// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Edge service for Lakko — broadcast cascade and audio room signaling.
//!
//! Geographically distributed; ingests from origin SFU and forwards encrypted
//! frames to viewer-side. Subscriber population is not an MLS group — content
//! keys are distributed via HPKE-envelopes; see ADR-0001 §"Channels".
//!
//! See `~/.claude/plans/1-nested-wreath.md` §5 (SFU and call architecture).

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

    tracing::info!("edge service starting (Phase 0 stub)");

    Ok(())
}
