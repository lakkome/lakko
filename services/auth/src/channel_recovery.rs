// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channel recovery evaluation ([ADR-0101] section 7, "Owner").
//!
//! `POST /auth/channel-recovery/evaluate` (bearer session): body
//! `{"blinded_element": <base64url>}`, answer `{"evaluation": <base64url>}`.
//! An OPRF evaluation (RFC 9497 OPRF mode, Ristretto255-SHA512, the suite
//! discovery uses) under a key derived for the session's account:
//!
//! ```text
//! seed  = BLAKE3-derive-key("lakko/auth/channel-recovery/v1", master ‖ account_id)
//! k_acct = DeriveKeyPair(seed, "lakko/auth/channel-recovery/oprf/v1")   (RFC 9497)
//! ```
//!
//! `master` is `LAKKO_AUTH_CHANNEL_RECOVERY_SECRET` (64 hex). No table: the
//! key is recomputed per request from the account id alone, so it is the
//! same before and after an ADR-0073 key reset (which changes the account's
//! keys, never its id). A device blinds
//! `lakko_channels::channel_recovery_oprf_input(channel_id)`; auth never sees
//! the channel id. The client finalises and derives the channel's recovery
//! key with `lakko_channels::ChannelRecoveryKey::from_oprf_output`.
//!
//! OPRF mode, not VOPRF: the evaluation carries no proof. A device that got
//! a wrong evaluation derives a recovery key that does not match the one the
//! channel registered, and `services/channels` refuses the owner change, so
//! a proof would add nothing the registered key does not already check.
//!
//! Refusals: `503 {"error":"channel-recovery-disabled"}` when the secret is
//! unset; `400` for a body that is not one valid element; `429
//! {"error":"too-many-attempts"}` past the per-account counter
//! ([`crate::limits::Scope::ChannelRecovery`]). A suspended account is
//! refused by the session middleware, as on every bearer route.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::SecretBytes;
use serde::{Deserialize, Serialize};
use voprf::{BlindedElement, OprfServer};

use crate::limits::Scope;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// The environment variable holding the master secret.
pub(crate) const SECRET_VAR: &str = "LAKKO_AUTH_CHANNEL_RECOVERY_SECRET";

/// BLAKE3 derive-key context of the per-account seed. Frozen.
pub(crate) const ACCOUNT_KEY_DOMAIN: &str = "lakko/auth/channel-recovery/v1";

/// RFC 9497 `DeriveKeyPair` info string. Frozen.
const OPRF_INFO: &[u8] = b"lakko/auth/channel-recovery/oprf/v1";

/// `503` body when the secret is unset.
pub(crate) const CODE_DISABLED: &str = "channel-recovery-disabled";

/// Serialised length of a Ristretto255 element.
const ELEMENT_LEN: usize = 32;

/// Upper bound on the base64url text before decoding.
const MAX_ELEMENT_B64_LEN: usize = 64;

/// The OPRF suite (RFC 9497 section 4.1, Ristretto255-SHA512).
type Suite = voprf::Ristretto255;

/// The master secret. Zeroed on drop; `Debug` prints nothing.
#[derive(Clone)]
pub(crate) struct ChannelRecoveryMaster(SecretBytes<32>);

impl core::fmt::Debug for ChannelRecoveryMaster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ChannelRecoveryMaster(..)")
    }
}

impl ChannelRecoveryMaster {
    /// The master from its 32 bytes.
    #[cfg(test)]
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(SecretBytes::new(bytes))
    }

    /// The OPRF server of `account_id`: the RFC 9497 key pair derived from
    /// the per-account seed.
    fn server_for(&self, account_id: &[u8; 16]) -> Result<OprfServer<Suite>, ApiError> {
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(self.0.as_bytes());
        material[32..].copy_from_slice(account_id);
        let material = SecretBytes::new(material);
        let seed = SecretBytes::new(blake3_derive_key_from_material(
            ACCOUNT_KEY_DOMAIN,
            material.as_bytes(),
        ));
        OprfServer::<Suite>::new_from_seed(seed.as_bytes(), OPRF_INFO).map_err(|_| {
            // DeriveKeyPair fails only with negligible probability per seed.
            tracing::error!("channel recovery key derivation failed");
            ApiError::Internal
        })
    }

    /// Evaluate a serialised blinded element under `account_id`'s key.
    fn evaluate(&self, account_id: &[u8; 16], blinded: &[u8]) -> Result<Vec<u8>, ApiError> {
        let element = BlindedElement::<Suite>::deserialize(blinded)
            .map_err(|_| ApiError::BadInput("blinded_element is not a valid element"))?;
        Ok(self
            .server_for(account_id)?
            .blind_evaluate(&element)
            .serialize()
            .to_vec())
    }
}

/// Read the master secret. `Ok(None)` when unset or blank.
///
/// # Errors
///
/// A value that is set but is not 64 hex characters.
pub(crate) fn load_master() -> anyhow::Result<Option<ChannelRecoveryMaster>> {
    match std::env::var(SECRET_VAR) {
        Ok(v) if !v.trim().is_empty() => crate::login_handle::parse_hex32(v.trim())
            .map(|k| Some(ChannelRecoveryMaster(SecretBytes::new(k))))
            .ok_or_else(|| anyhow::anyhow!("{SECRET_VAR} must be 64 hex characters (32 bytes)")),
        _ => Ok(None),
    }
}

/// Log the startup state.
pub(crate) fn log_startup(master: Option<&ChannelRecoveryMaster>) {
    if master.is_some() {
        tracing::info!("channel recovery evaluation enabled");
    } else {
        tracing::warn!(
            knob = SECRET_VAR,
            "channel recovery evaluation disabled (secret unset)"
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvaluateReq {
    /// Base64url (no padding) of the 32-byte blinded element.
    blinded_element: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct EvaluateRes {
    /// Base64url (no padding) of the 32-byte evaluation element.
    evaluation: String,
}

/// `POST /auth/channel-recovery/evaluate`.
///
/// # Errors
///
/// `503 channel-recovery-disabled`, `400` on a malformed element, `429
/// too-many-attempts` (`503 rate-limiter-unavailable` when the counter store
/// fails).
pub(crate) async fn evaluate(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EvaluateReq>,
) -> Result<Json<EvaluateRes>, ApiError> {
    let Some(master) = state.channel_recovery.as_deref() else {
        return Err(ApiError::Coded(
            StatusCode::SERVICE_UNAVAILABLE,
            CODE_DISABLED,
        ));
    };
    if req.blinded_element.len() > MAX_ELEMENT_B64_LEN {
        return Err(ApiError::BadInput("blinded_element too long"));
    }
    let blinded = URL_SAFE_NO_PAD
        .decode(req.blinded_element.as_bytes())
        .map_err(|_| ApiError::BadInput("blinded_element not URL-safe base64"))?;
    if blinded.len() != ELEMENT_LEN {
        return Err(ApiError::BadInput(
            "blinded_element must decode to 32 bytes",
        ));
    }
    // Validate the element before charging, so a malformed request spends
    // none of the account's budget.
    BlindedElement::<Suite>::deserialize(&blinded)
        .map_err(|_| ApiError::BadInput("blinded_element is not a valid element"))?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::ChannelRecovery, &auth.0).await?;
    let evaluation = master.evaluate(&auth.0, &blinded)?;
    Ok(Json(EvaluateRes {
        evaluation: URL_SAFE_NO_PAD.encode(evaluation),
    }))
}

#[cfg(test)]
mod tests;
