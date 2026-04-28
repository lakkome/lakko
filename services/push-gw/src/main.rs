// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push gateway for Lakko.
//!
//! Fanout to APNs, FCM and UnifiedPush. Payload contains only
//! `(group_pseudonym, envelope_pointer)` — push providers never see
//! plaintext content. UnifiedPush is mandatory for regions where Google
//! Play Services is unavailable.
//!
//! See `~/.claude/plans/1-nested-wreath.md` §4 (Push notifications).

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

    tracing::info!("push-gw service starting (Phase 0 stub)");

    Ok(())
}
