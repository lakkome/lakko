// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The server half `S` of the owned-channels password lock ([ADR-0101]
//! section 7, item 1).
//!
//! An account keeps the list of the channels it owns on the storage service,
//! under a random list key `R` that three kinds of lock hold. The password
//! lock is `BLAKE3-derive-key("lakko/channels/owned-lock-password/v1",
//! export_key ‖ S)`: the password alone does not open it, and neither does
//! this service alone. `S` is recomputed per request, never stored:
//!
//! ```text
//! S = BLAKE3-derive-key("lakko/auth/owned-channels/v1", master ‖ account_id(16))
//! ```
//!
//! `master` is `LAKKO_AUTH_OWNED_CHANNELS_SECRET` (64 hex), **required at
//! startup**: a server that could not give `S` at sign-up would leave the
//! account's owned channels without a password lock.
//!
//! `S` leaves this service in exactly two places:
//!
//! - the answer of `/signup/finish` (`owned_channels_secret`, base64url, 32
//!   bytes), where the account was created in that very exchange;
//! - `POST /account/owned-channels-secret {reset_grant}` (bearer), which
//!   requires the ADR-0073 reset factor grant (the password sign-in plus one
//!   second factor) and does not spend it, so the resetting device asks for
//!   `S` and then resets.
//!
//! Not a password sign-in, not a passkey sign-in, not a refresh. What this
//! service learns from the route: that the account asked for `S` and when.
//! A refused grant charges nothing; an accepted call is counted by
//! [`Scope::OwnedChannelsSecret`] per account (10 a day by default, chosen,
//! not measured).
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::SecretBytes;
use serde::{Deserialize, Serialize};

use crate::limits::Scope;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// The environment variable holding the master secret.
pub(crate) const SECRET_VAR: &str = "LAKKO_AUTH_OWNED_CHANNELS_SECRET";

/// BLAKE3 derive-key context of `S`. Frozen: a change orphans every
/// password lock.
pub(crate) const SERVER_HALF_DOMAIN: &str = "lakko/auth/owned-channels/v1";

/// The master secret. Zeroed on drop; `Debug` prints nothing.
#[derive(Clone)]
pub(crate) struct OwnedChannelsMaster(SecretBytes<32>);

impl core::fmt::Debug for OwnedChannelsMaster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("OwnedChannelsMaster(..)")
    }
}

impl OwnedChannelsMaster {
    /// The master from its 32 bytes.
    #[cfg(test)]
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(SecretBytes::new(bytes))
    }

    /// `S` of `account_id`.
    pub(crate) fn server_half(&self, account_id: &[u8; 16]) -> SecretBytes<32> {
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(self.0.as_bytes());
        material[32..].copy_from_slice(account_id);
        let material = SecretBytes::new(material);
        SecretBytes::new(blake3_derive_key_from_material(
            SERVER_HALF_DOMAIN,
            material.as_bytes(),
        ))
    }

    /// `S` of `account_id` as it rides on the wire: base64url, no padding.
    pub(crate) fn server_half_b64(&self, account_id: &[u8; 16]) -> String {
        URL_SAFE_NO_PAD.encode(self.server_half(account_id).as_bytes())
    }
}

/// Read the master secret.
///
/// # Errors
///
/// The variable unset or blank (required at startup), or not 64 hex
/// characters.
pub(crate) fn load_master() -> anyhow::Result<OwnedChannelsMaster> {
    parse_master(std::env::var(SECRET_VAR).ok().as_deref())
}

/// [`load_master`] over the variable's value (`None` when unset), so the
/// "required at startup" rule is testable without the process environment.
fn parse_master(value: Option<&str>) -> anyhow::Result<OwnedChannelsMaster> {
    let raw = value.filter(|v| !v.trim().is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "{SECRET_VAR} must be set (64 hex characters): the server half of every \
                 account's owned-channels password lock"
        )
    })?;
    crate::login_handle::parse_hex32(raw.trim())
        .map(|k| OwnedChannelsMaster(SecretBytes::new(k)))
        .ok_or_else(|| anyhow::anyhow!("{SECRET_VAR} must be 64 hex characters (32 bytes)"))
}

/// The master of `state`, or the `500` a state without one gets (a state
/// built by `main` always has one; only a test can build one without).
fn master(state: &AppState) -> Result<&OwnedChannelsMaster, ApiError> {
    state.owned_channels.as_deref().ok_or_else(|| {
        tracing::error!("owned-channels master missing from the state");
        ApiError::Internal
    })
}

/// `S` of a just-created account, for the `/signup/finish` answer.
///
/// # Errors
///
/// [`ApiError::Internal`] when the state has no master.
pub(crate) fn signup_secret(state: &AppState, account_id: &[u8; 16]) -> Result<String, ApiError> {
    Ok(master(state)?.server_half_b64(account_id))
}

/// Body of `POST /account/owned-channels-secret`.
#[derive(Deserialize)]
pub(crate) struct SecretReq {
    /// The `f1` reset factor grant a reset factor route returned. Optional
    /// on the wire so that its absence is the `403 reset-factor-required`
    /// every other refusal gets.
    #[serde(default)]
    pub(crate) reset_grant: Option<String>,
}

/// Answer of `POST /account/owned-channels-secret`.
#[derive(Serialize)]
pub(crate) struct SecretRes {
    /// Base64url (no padding) of the account's 32-byte `S`.
    pub(crate) owned_channels_secret: String,
}

/// `POST /account/owned-channels-secret` — bearer AND a live reset factor
/// grant for the bearer's account at its current generation. The grant is
/// not spent.
///
/// Order: the grant (`403 reset-factor-required`, nothing charged), then
/// [`Scope::OwnedChannelsSecret`] for the account (`429
/// too-many-attempts`), then `S`.
///
/// # Errors
///
/// As listed; [`ApiError::Db`]; [`ApiError::Internal`].
pub(crate) async fn secret(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<SecretReq>,
) -> Result<Json<SecretRes>, ApiError> {
    let account_id = auth.0;
    crate::session::require_reset_grant(&state, &account_id, req.reset_grant.as_deref()).await?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::OwnedChannelsSecret,
        &account_id,
    )
    .await?;
    let owned_channels_secret = master(&state)?.server_half_b64(&account_id);
    tracing::info!("owned-channels server half released");
    Ok(Json(SecretRes {
        owned_channels_secret,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_half_binds_the_master_and_the_account() {
        let a = OwnedChannelsMaster::from_bytes([1; 32]);
        let b = OwnedChannelsMaster::from_bytes([2; 32]);
        let s = a.server_half(&[7; 16]);
        assert_eq!(s.as_bytes(), a.server_half(&[7; 16]).as_bytes());
        assert_ne!(s.as_bytes(), a.server_half(&[8; 16]).as_bytes());
        assert_ne!(s.as_bytes(), b.server_half(&[7; 16]).as_bytes());
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(a.server_half_b64(&[7; 16]))
                .unwrap()
                .as_slice(),
            s.as_bytes()
        );
    }

    #[test]
    fn the_server_half_is_the_documented_derivation() {
        let master = [0x42u8; 32];
        let account = [0x24u8; 16];
        let mut material = Vec::new();
        material.extend_from_slice(&master);
        material.extend_from_slice(&account);
        assert_eq!(
            OwnedChannelsMaster::from_bytes(master)
                .server_half(&account)
                .as_bytes(),
            &blake3_derive_key_from_material("lakko/auth/owned-channels/v1", &material)
        );
    }

    #[test]
    fn startup_is_refused_without_the_secret_or_with_a_malformed_one() {
        for missing in [None, Some(""), Some("   ")] {
            let err = parse_master(missing).unwrap_err().to_string();
            assert!(err.contains("must be set"), "{err}");
        }
        for bad in ["abcd", &"zz".repeat(32), &"ab".repeat(31)] {
            assert!(parse_master(Some(bad)).is_err(), "{bad}");
        }
        let ok = parse_master(Some(&format!(" {} ", "ab".repeat(32)))).unwrap();
        assert_eq!(
            ok.server_half(&[1; 16]).as_bytes(),
            OwnedChannelsMaster::from_bytes([0xAB; 32])
                .server_half(&[1; 16])
                .as_bytes()
        );
    }

    #[test]
    fn the_debug_form_prints_nothing() {
        let m = OwnedChannelsMaster::from_bytes([9; 32]);
        assert_eq!(format!("{m:?}"), "OwnedChannelsMaster(..)");
    }
}
