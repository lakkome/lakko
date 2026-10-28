// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! push-gw's X25519 wake key ([ADR-0078] section 3).
//!
//! Members seal every wake to the public half (`x-lakko-wake` on a DS
//! publish), so the Delivery Service forwards a blob it cannot read. The key
//! is generated at first boot and stored in this service's own database
//! (single-row table `push_wake_key`, migration `0006`), the pattern auth
//! uses for its attestation key: every process start rebuilds the key pair
//! from the stored 32 bytes. It is not taken from the environment file, so
//! the one file every service reads does not also open every wake.
//!
//! The public half is served at `GET /push/wake-key`; clients fetch it, or
//! carry a build-time pin, the way they treat the device-attestation key.
//! A wake sealed to anything else does not open here, so a substituted key
//! costs availability (no wake), never confidentiality of anything the
//! server could not see before. Rotation is out of scope (same posture as
//! the other service keys).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use anyhow::Context as _;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use lakko_contacts::push_wake::{PushGwKey, PUSH_GW_KEY_LEN};
use serde::Serialize;
use sqlx::MySqlPool;

use crate::AppState;

/// Load the wake key, generating and storing one on the first run.
///
/// # Errors
///
/// A database error, or a stored secret of the wrong length (corruption);
/// fatal at startup.
pub(crate) async fn load_or_init_wake_key(pool: &MySqlPool) -> anyhow::Result<PushGwKey> {
    if let Some(key) = read(pool).await? {
        return Ok(key);
    }
    // `INSERT IGNORE` so two racing first starts do not both insert; the
    // re-read lets the winner's key stand.
    let fresh = PushGwKey::generate();
    sqlx::query("INSERT IGNORE INTO push_wake_key (id, secret) VALUES (1, ?)")
        .bind(&fresh.secret_bytes().as_bytes()[..])
        .execute(pool)
        .await
        .context("insert push_wake_key")?;
    read(pool)
        .await?
        .context("push_wake_key missing right after insert")
}

async fn read(pool: &MySqlPool) -> anyhow::Result<Option<PushGwKey>> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT secret FROM push_wake_key WHERE id = 1")
        .fetch_optional(pool)
        .await
        .context("read push_wake_key")?;
    let Some((bytes,)) = row else {
        return Ok(None);
    };
    let secret: [u8; PUSH_GW_KEY_LEN] = bytes.try_into().map_err(|b: Vec<u8>| {
        anyhow::anyhow!(
            "push_wake_key.secret length {} != {PUSH_GW_KEY_LEN}; database corruption?",
            b.len()
        )
    })?;
    Ok(Some(PushGwKey::from_secret_bytes(secret)))
}

#[derive(Serialize)]
pub(crate) struct WakeKeyResponse {
    /// The X25519 public key, base64url without padding.
    pub(crate) key: String,
}

/// `GET /push/wake-key` — the public key clients seal wakes to. No token:
/// it is public and the same for everybody.
pub(crate) async fn wake_key_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "public, max-age=3600")],
        Json(WakeKeyResponse {
            key: B64URL.encode(state.wake_key.public()),
        }),
    )
}
