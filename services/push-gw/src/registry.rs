// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push token registry — `(device_pseudonym, provider) → token`.
//!
//! Re-registering the same (device, provider) pair replaces the token —
//! clients rotate tokens periodically (APNs token after backup restore,
//! FCM token after app update, etc.). The PK is composite so a single
//! device can be reachable through multiple providers (e.g. Web Push
//! on desktop browser plus FCM on the Android companion app).
//!
//! Every route here is anonymous by design (invariant #5: a session would
//! name the account behind a device pseudonym). Since ADR-0054 S3 each
//! request spends one `push-register` anonymous token, checked by the
//! router's gate (`x-lakko-anon-token`) before the handler runs.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::provider::ProviderKind;
use crate::AppState;

/// Device pseudonym length: 32 raw bytes, 64 lowercase hex chars on
/// the wire — same convention as `services/ds`.
const DEVICE_LEN: usize = 32;
/// Push tokens vary by provider. APNs is 32 hex bytes (~64 chars),
/// FCM is up to ~200 chars, Web Push subscription endpoints push the
/// envelope further. 2 KiB is generous and matches the column.
const MAX_TOKEN_BYTES: usize = 2048;

fn decode_device(hex_str: &str) -> Result<[u8; DEVICE_LEN], (StatusCode, String)> {
    if hex_str.len() != DEVICE_LEN * 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("device_pseudonym must be {} hex chars", DEVICE_LEN * 2),
        ));
    }
    if !hex_str
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "device_pseudonym must be lowercase hex [0-9a-f]".to_string(),
        ));
    }
    let mut out = [0u8; DEVICE_LEN];
    hex::decode_to_slice(hex_str, &mut out)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("hex decode: {e}")))?;
    Ok(out)
}

fn decode_token(b64: &str) -> Result<Vec<u8>, (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid token base64: {e}"),
        )
    })?;
    if bytes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "token must be non-empty".to_string(),
        ));
    }
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("token > {MAX_TOKEN_BYTES} bytes"),
        ));
    }
    Ok(bytes)
}

// ============================================================================
// /push/register
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct RegisterRequest {
    /// 32-byte device pseudonym, lowercase hex.
    pub(crate) device_pseudonym: String,
    /// Which platform this token is for.
    pub(crate) provider: ProviderKind,
    /// Base64-encoded raw token bytes (provider-specific format,
    /// opaque to us).
    pub(crate) token: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterResponse {
    pub(crate) ok: bool,
}

/// `POST /push/register` — store (or replace) a push token for a
/// `(device_pseudonym, provider)` pair. Idempotent: re-registering
/// the same pair updates the token in place.
pub(crate) async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let device = decode_device(&payload.device_pseudonym)?;
    let token = decode_token(&payload.token)?;
    let provider_db = payload.provider.as_db();

    // UPSERT — `INSERT ... ON DUPLICATE KEY UPDATE`. The composite PK
    // is `(device_id, provider)`, so re-registration with a fresh
    // token simply overwrites the bytes.
    sqlx::query(
        "INSERT INTO push_tokens (device_id, provider, token) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE token = VALUES(token)",
    )
    .bind(&device[..])
    .bind(provider_db)
    .bind(&token)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert push_tokens");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    tracing::info!(
        provider = payload.provider.as_str(),
        device_prefix = %hex::encode(&device[..4]),
        "push token registered",
    );
    Ok(Json(RegisterResponse { ok: true }))
}

// ============================================================================
// /push/unregister
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct UnregisterRequest {
    pub(crate) device_pseudonym: String,
    pub(crate) provider: ProviderKind,
}

#[derive(Serialize)]
pub(crate) struct UnregisterResponse {
    pub(crate) ok: bool,
    /// `true` if a row was deleted; `false` if there was nothing
    /// registered (idempotent — clients can call this safely on logout
    /// without checking first).
    pub(crate) removed: bool,
}

/// `POST /push/unregister` — drop a push token. Idempotent.
pub(crate) async fn unregister(
    State(state): State<AppState>,
    Json(payload): Json<UnregisterRequest>,
) -> Result<Json<UnregisterResponse>, (StatusCode, String)> {
    let device = decode_device(&payload.device_pseudonym)?;
    let provider_db = payload.provider.as_db();

    let result = sqlx::query("DELETE FROM push_tokens WHERE device_id = ? AND provider = ?")
        .bind(&device[..])
        .bind(provider_db)
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "delete push_tokens");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            )
        })?;

    let removed = result.rows_affected() > 0;
    if removed {
        tracing::info!(
            provider = payload.provider.as_str(),
            device_prefix = %hex::encode(&device[..4]),
            "push token unregistered",
        );
    }
    Ok(Json(UnregisterResponse { ok: true, removed }))
}

// ============================================================================
// /push/group/subscribe (Phase 1.20e — fan-out)
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct GroupSubscribeRequest {
    /// 32-byte device pseudonym, lowercase hex.
    pub(crate) device_pseudonym: String,
    /// 32-byte group pseudonym, lowercase hex. Same handle the
    /// DS uses as its group journal key.
    pub(crate) group_pseudonym: String,
}

#[derive(Serialize)]
pub(crate) struct GroupSubscribeResponse {
    pub(crate) ok: bool,
    /// `true` if the row was inserted fresh; `false` if it was
    /// already there (idempotent re-subscribe).
    pub(crate) inserted: bool,
}

/// `POST /push/group/subscribe` — declare that this device wants
/// wakes for messages published into `group_pseudonym`. Idempotent:
/// a re-subscribe with the same `(device, group)` pair returns
/// `inserted: false` and is a no-op.
pub(crate) async fn group_subscribe(
    State(state): State<AppState>,
    Json(payload): Json<GroupSubscribeRequest>,
) -> Result<Json<GroupSubscribeResponse>, (StatusCode, String)> {
    let device = decode_device(&payload.device_pseudonym)?;
    let group = decode_device(&payload.group_pseudonym)?;

    // INSERT IGNORE — the composite PK `(device_id, group_id)`
    // makes a re-subscribe a silent no-op. `rows_affected == 0`
    // is the "already subscribed" signal we surface as
    // `inserted: false`.
    let result = sqlx::query(
        "INSERT IGNORE INTO push_group_subscriptions (device_id, group_id) VALUES (?, ?)",
    )
    .bind(&device[..])
    .bind(&group[..])
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert push_group_subscriptions");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let inserted = result.rows_affected() > 0;
    tracing::debug!(
        device_prefix = %hex::encode(&device[..4]),
        group_prefix = %hex::encode(&group[..4]),
        inserted,
        "push group subscription",
    );
    Ok(Json(GroupSubscribeResponse { ok: true, inserted }))
}

// ============================================================================
// /push/group/unsubscribe (Phase 1.20e)
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct GroupUnsubscribeRequest {
    pub(crate) device_pseudonym: String,
    pub(crate) group_pseudonym: String,
}

#[derive(Serialize)]
pub(crate) struct GroupUnsubscribeResponse {
    pub(crate) ok: bool,
    /// `true` if a row was deleted; `false` if there was nothing
    /// registered — idempotent like the token unregister path.
    pub(crate) removed: bool,
}

/// `POST /push/group/unsubscribe` — drop a `(device, group)`
/// subscription. Idempotent.
pub(crate) async fn group_unsubscribe(
    State(state): State<AppState>,
    Json(payload): Json<GroupUnsubscribeRequest>,
) -> Result<Json<GroupUnsubscribeResponse>, (StatusCode, String)> {
    let device = decode_device(&payload.device_pseudonym)?;
    let group = decode_device(&payload.group_pseudonym)?;

    let result =
        sqlx::query("DELETE FROM push_group_subscriptions WHERE device_id = ? AND group_id = ?")
            .bind(&device[..])
            .bind(&group[..])
            .execute(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "delete push_group_subscriptions");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    let removed = result.rows_affected() > 0;
    Ok(Json(GroupUnsubscribeResponse { ok: true, removed }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_device_validates_format() {
        assert!(decode_device(&"a".repeat(64)).is_ok());
        assert!(decode_device(&"0".repeat(64)).is_ok());
        assert!(decode_device(&"A".repeat(64)).is_err()); // uppercase rejected
        assert!(decode_device(&"g".repeat(64)).is_err());
        assert!(decode_device(&"a".repeat(63)).is_err());
        assert!(decode_device(&"a".repeat(65)).is_err());
    }

    fn register_body(provider: &str) -> String {
        format!(
            r#"{{"device_pseudonym":"{}","provider":"{provider}","token":"{}"}}"#,
            "ab".repeat(32),
            B64.encode([7u8; 32]),
        )
    }

    /// The five provider names, and the row each is stored under.
    ///
    /// These are the exact strings the three clients send — iOS
    /// `PushClient.Provider`, Android `PushClient.kt`, web
    /// `push/client.ts`. A device that cannot register is a device that
    /// is never woken, so the wire names and the column values are
    /// pinned together here rather than assumed from the enum.
    const WIRE_NAMES: [(&str, ProviderKind, u8); 5] = [
        ("apns", ProviderKind::Apns, 1),
        ("fcm", ProviderKind::Fcm, 2),
        ("web_push", ProviderKind::WebPush, 3),
        ("unified_push", ProviderKind::UnifiedPush, 4),
        ("apns_voip", ProviderKind::ApnsVoip, 5),
    ];

    #[test]
    fn every_provider_kind_can_be_registered() {
        for (wire, kind, db) in WIRE_NAMES {
            let req: RegisterRequest = serde_json::from_str(&register_body(wire))
                .unwrap_or_else(|e| panic!("{wire} rejected by the extractor: {e}"));
            assert_eq!(req.provider, kind);
            assert_eq!(req.provider.as_db(), db);
            // The handler's own two decode gates, which run before the
            // UPSERT and are the rest of what a register has to clear.
            assert!(decode_device(&req.device_pseudonym).is_ok());
            assert!(decode_token(&req.token).is_ok());
        }
    }

    #[test]
    fn every_provider_kind_can_be_unregistered() {
        for (wire, kind, _) in WIRE_NAMES {
            let body = format!(
                r#"{{"device_pseudonym":"{}","provider":"{wire}"}}"#,
                "ab".repeat(32),
            );
            let req: UnregisterRequest = serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("{wire} rejected by the extractor: {e}"));
            assert_eq!(req.provider, kind);
        }
    }

    #[test]
    fn the_two_apple_tokens_are_separate_rows_of_one_device() {
        // The composite PK is `(device_id, provider)`, so this pair of
        // distinct column values is exactly what keeps a fresh call
        // token from evicting the alert token, and vice versa.
        assert_ne!(
            ProviderKind::Apns.as_db(),
            ProviderKind::ApnsVoip.as_db(),
            "one device's two Apple tokens would collide on the primary key",
        );
    }

    #[test]
    fn an_unknown_provider_name_is_refused() {
        for bad in ["voip", "apnsvoip", "ApnsVoip", "pushkit", "apns_alert"] {
            assert!(
                serde_json::from_str::<RegisterRequest>(&register_body(bad)).is_err(),
                "expected refusal for {bad}",
            );
        }
    }

    #[test]
    fn decode_token_validates_size() {
        assert!(decode_token(&B64.encode([0u8; 32])).is_ok());
        // empty payload after b64-decode is rejected
        assert!(decode_token("").is_err());
        // too big
        let huge = B64.encode(vec![0u8; MAX_TOKEN_BYTES + 1]);
        assert!(decode_token(&huge).is_err());
        // bad b64
        assert!(decode_token("not-valid!!!").is_err());
    }
}
