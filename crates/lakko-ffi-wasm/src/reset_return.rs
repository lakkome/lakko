// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for ADR-0101: reaching an account after a key reset.
//! Mirror of `crates/lakko-ffi-uniffi/src/reset_return.rs`.
//!
//! - The continuity binding: `signContinuityBinding` /
//!   `verifyContinuityBinding`; the key is
//!   `MailboxKey.continuityFromAccountIdentity`, its DS signer
//!   `dsContinuityMailboxSigner`.
//! - The channel return (`MailboxKind.ChannelReturn` rows).
//! - The history request, answered with the ADR-0091 gap history.
//! - The channel recovery key.
//!
//! Every refusal throws a string `"<area>.<code>: <reason>"` (`continuity`,
//! `channelReturn`, `historyRequest`, `channelRecovery`).

use lakko_channels::recovery::{
    channel_recovery_oprf_input as rust_oprf_input, ChannelRecoveryKey as RustRecoveryKey,
    RECOVERY_PROOF_HEADER_NAME,
};
use lakko_contacts::channel_epoch_seeds::ChannelEpochSeed;
use lakko_contacts::channel_return::{
    compose_channel_return as rust_compose_return, decode_channel_return as rust_decode_return,
    ChannelReturn as RustChannelReturn, ChannelReturnError, ChannelReturnRole as RustRole,
    CHANNEL_RETURN_MAX_INVITATION_LEN,
};
use lakko_contacts::continuity::{
    verify_continuity_binding as rust_verify_binding, ContinuityBinding, CONTINUITY_BINDING_LEN,
    CONTINUITY_WINDOW_MS,
};
use lakko_contacts::history_request::{
    compose_history_request as rust_compose_request, decode_history_request as rust_decode_request,
    HistoryRequest as RustHistoryRequest, HISTORY_REQUEST_ENVELOPE_LEN,
};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

fn array<const N: usize>(area: &str, what: &str, bytes: &[u8]) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        format!(
            "{area}.keyLength: {what} must be {N} bytes, got {}",
            bytes.len()
        )
    })
}

fn return_error(e: ChannelReturnError) -> String {
    format!("channelReturn.{}: {e}", e.code())
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// Continuity binding
// ============================================================================

/// The 105-byte continuity binding of `accountIdentity`: its continuity
/// mailbox key and `expiresAtMs`, signed by its identity key. Upload it
/// beside the identity bundle after a key reset; set `expiresAtMs` to now
/// plus `continuityWindowMs()`.
#[wasm_bindgen(js_name = signContinuityBinding)]
#[must_use]
pub fn sign_continuity_binding(account_identity: &Identity, expires_at_ms: u64) -> Vec<u8> {
    ContinuityBinding::for_account_seed(
        &account_identity.mls_identity().ed25519_seed_bytes(),
        expires_at_ms,
    )
    .encode()
    .to_vec()
}

/// Verify a continuity binding served for an account against the 32-byte
/// identity key the caller took from the attested reset, at its clock
/// `nowMs`; returns the 32-byte continuity mailbox key to write to.
///
/// # Errors
///
/// A `continuity.<code>` string (`keyLength`, `badLength`,
/// `unknownVersion`, `badKey`, `expired`, `expiryTooFar`, `badSignature`).
/// Any of them means "do not write".
#[wasm_bindgen(js_name = verifyContinuityBinding)]
pub fn verify_continuity_binding(
    binding: &[u8],
    identity_pk: &[u8],
    now_ms: u64,
) -> Result<Vec<u8>, String> {
    let pk = array::<32>("continuity", "identityPk", identity_pk)?;
    rust_verify_binding(binding, &pk, now_ms)
        .map(|k| k.to_vec())
        .map_err(|e| format!("continuity.{}: {e}", e.code()))
}

/// The binding's length (105).
#[wasm_bindgen(js_name = continuityBindingLen)]
#[must_use]
pub fn continuity_binding_len() -> u32 {
    len_u32(CONTINUITY_BINDING_LEN)
}

/// The announcement window, ms (30 days; chosen, not measured).
#[wasm_bindgen(js_name = continuityWindowMs)]
#[must_use]
pub fn continuity_window_ms() -> u64 {
    CONTINUITY_WINDOW_MS
}

// ============================================================================
// Channel return
// ============================================================================

/// What the recipient was in the channel. The numbers are the wire bytes.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelReturnRole {
    /// A subscriber: re-subscribe.
    Subscriber = 1,
    /// A publisher: an owner device admits the new devices.
    Publisher = 2,
    /// The owner: recover the channel with its recovery key.
    Owner = 3,
}

impl From<ChannelReturnRole> for RustRole {
    fn from(r: ChannelReturnRole) -> Self {
        match r {
            ChannelReturnRole::Subscriber => Self::Subscriber,
            ChannelReturnRole::Publisher => Self::Publisher,
            ChannelReturnRole::Owner => Self::Owner,
        }
    }
}

impl From<RustRole> for ChannelReturnRole {
    fn from(r: RustRole) -> Self {
        match r {
            RustRole::Subscriber => Self::Subscriber,
            RustRole::Publisher => Self::Publisher,
            RustRole::Owner => Self::Owner,
        }
    }
}

/// A decoded channel return. The invitation and the seeds are secret.
#[wasm_bindgen]
pub struct ChannelReturn {
    inner: RustChannelReturn,
}

#[wasm_bindgen]
impl ChannelReturn {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// What the recipient was in the channel.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn role(&self) -> ChannelReturnRole {
        self.inner.role.into()
    }

    /// The old card's 64-byte account binding signature.
    #[wasm_bindgen(getter, js_name = oldBindingSig)]
    #[must_use]
    pub fn old_binding_sig(&self) -> Vec<u8> {
        self.inner.old_binding_sig.to_vec()
    }

    /// The 32-byte subscriber key the old card was uploaded under.
    #[wasm_bindgen(getter, js_name = oldSubscriberPk)]
    #[must_use]
    pub fn old_subscriber_pk(&self) -> Vec<u8> {
        self.inner.old_subscriber_pk.to_vec()
    }

    /// The 32-byte BLAKE3 of the old card's profile.
    #[wasm_bindgen(getter, js_name = profileHash)]
    #[must_use]
    pub fn profile_hash(&self) -> Vec<u8> {
        self.inner.profile_hash.to_vec()
    }

    /// The channel's invitation, opaque; empty for none.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn invitation(&self) -> Vec<u8> {
        self.inner.invitation.clone()
    }

    /// The epochs of the seeds, ascending.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn epochs(&self) -> Vec<u32> {
        self.inner.epoch_seeds.iter().map(|s| s.epoch).collect()
    }

    /// The 32-byte seeds, concatenated in the order of `epochs`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seeds(&self) -> Vec<u8> {
        self.inner.epoch_seeds.iter().flat_map(|s| s.seed).collect()
    }

    /// The signed profile envelope the sender holds for the person, or
    /// `undefined`. Keep it only if `verifyProfileEnvelope` accepts it under
    /// this account's previous key.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn profile(&self) -> Option<Vec<u8>> {
        self.inner.profile.clone()
    }
}

/// Compose a channel return, the body of a `ChannelReturn` mailbox row to
/// the recipient's continuity mailbox. `epochs` and `seeds` are parallel:
/// `seeds` is `32 * epochs.length` bytes, strictly ascending by epoch, at
/// most 64; `profile` is optional.
///
/// # Errors
///
/// A `channelReturn.<code>` string.
#[wasm_bindgen(js_name = composeChannelReturnEnvelope)]
#[allow(clippy::too_many_arguments)]
pub fn compose_channel_return_envelope(
    channel_id: &[u8],
    role: ChannelReturnRole,
    old_binding_sig: &[u8],
    old_subscriber_pk: &[u8],
    profile_hash: &[u8],
    invitation: &[u8],
    epochs: Vec<u32>,
    seeds: &[u8],
    profile: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    if seeds.len() != epochs.len().saturating_mul(32) {
        return Err(format!(
            "channelReturn.keyLength: seeds must be 32 * epochs.length bytes, got {}",
            seeds.len()
        ));
    }
    let epoch_seeds = epochs
        .iter()
        .zip(seeds.chunks_exact(32))
        .map(|(&epoch, seed)| {
            Ok(ChannelEpochSeed {
                epoch,
                seed: array::<32>("channelReturn", "seed", seed)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    rust_compose_return(&RustChannelReturn {
        channel_id: array::<16>("channelReturn", "channelId", channel_id)?,
        role: role.into(),
        old_binding_sig: array::<64>("channelReturn", "oldBindingSig", old_binding_sig)?,
        old_subscriber_pk: array::<32>("channelReturn", "oldSubscriberPk", old_subscriber_pk)?,
        profile_hash: array::<32>("channelReturn", "profileHash", profile_hash)?,
        invitation: invitation.to_vec(),
        epoch_seeds,
        profile,
    })
    .map_err(return_error)
}

/// Decode a channel return. Layout only; see
/// `verifyChannelReturnCardBinding`.
///
/// # Errors
///
/// A `channelReturn.<code>` string.
#[wasm_bindgen(js_name = decodeChannelReturnEnvelope)]
pub fn decode_channel_return_envelope(plaintext: &[u8]) -> Result<ChannelReturn, String> {
    rust_decode_return(plaintext)
        .map(|inner| ChannelReturn { inner })
        .map_err(return_error)
}

/// Decode a channel return and verify its old card binding against this
/// account's 32-byte **previous** identity key (from the KT log). Act on a
/// return without a prompt only when this succeeds.
///
/// # Errors
///
/// A `channelReturn.<code>` string.
#[wasm_bindgen(js_name = verifyChannelReturnCardBinding)]
pub fn verify_channel_return_card_binding(
    plaintext: &[u8],
    previous_identity_pk: &[u8],
) -> Result<ChannelReturn, String> {
    let pk = array::<32>("channelReturn", "previousIdentityPk", previous_identity_pk)?;
    let inner = rust_decode_return(plaintext).map_err(return_error)?;
    inner.verify_old_card_binding(&pk).map_err(return_error)?;
    Ok(ChannelReturn { inner })
}

/// The longest invitation a channel return carries.
#[wasm_bindgen(js_name = channelReturnMaxInvitationLen)]
#[must_use]
pub fn channel_return_max_invitation_len() -> u32 {
    len_u32(CHANNEL_RETURN_MAX_INVITATION_LEN)
}

// ============================================================================
// History request
// ============================================================================

/// A decoded history request.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryRequest {
    inner: RustHistoryRequest,
}

#[wasm_bindgen]
impl HistoryRequest {
    /// The 32-byte public gap key the answer is sealed to.
    #[wasm_bindgen(getter, js_name = gapKeyPub)]
    #[must_use]
    pub fn gap_key_pub(&self) -> Vec<u8> {
        self.inner.gap_key_pub.to_vec()
    }

    /// Answer with messages older than this DS id; 0 = the newest.
    #[wasm_bindgen(getter, js_name = beforeMessageId)]
    #[must_use]
    pub fn before_message_id(&self) -> i64 {
        self.inner.before_message_id
    }
}

/// Compose a history request to send in the conversation.
///
/// # Errors
///
/// A `historyRequest.<code>` string (`keyLength`, `badMessageId`).
#[wasm_bindgen(js_name = composeHistoryRequestEnvelope)]
pub fn compose_history_request_envelope(
    gap_key_pub: &[u8],
    before_message_id: i64,
) -> Result<Vec<u8>, String> {
    rust_compose_request(&RustHistoryRequest {
        gap_key_pub: array::<32>("historyRequest", "gapKeyPub", gap_key_pub)?,
        before_message_id,
    })
    .map_err(|e| format!("historyRequest.{}: {e}", e.code()))
}

/// Decode a history request.
///
/// # Errors
///
/// A `historyRequest.<code>` string.
#[wasm_bindgen(js_name = decodeHistoryRequestEnvelope)]
pub fn decode_history_request_envelope(plaintext: &[u8]) -> Result<HistoryRequest, String> {
    rust_decode_request(plaintext)
        .map(|inner| HistoryRequest { inner })
        .map_err(|e| format!("historyRequest.{}: {e}", e.code()))
}

/// The history request's length (66).
#[wasm_bindgen(js_name = historyRequestEnvelopeLen)]
#[must_use]
pub fn history_request_envelope_len() -> u32 {
    len_u32(HISTORY_REQUEST_ENVELOPE_LEN)
}

// ============================================================================
// Channel recovery key
// ============================================================================

/// A channel's recovery key. Holds the secret.
#[wasm_bindgen]
pub struct ChannelRecoveryKey {
    inner: RustRecoveryKey,
}

#[wasm_bindgen]
impl ChannelRecoveryKey {
    /// The key from its 32-byte seed.
    ///
    /// # Errors
    ///
    /// `channelRecovery.keyLength`.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed: &[u8]) -> Result<ChannelRecoveryKey, String> {
        Ok(Self {
            inner: RustRecoveryKey::from_seed(&array::<32>("channelRecovery", "seed", seed)?),
        })
    }

    /// The key from the VOPRF output auth evaluated over
    /// `channelRecoveryOprfInput(channelId)`.
    #[wasm_bindgen(js_name = fromOprfOutput)]
    #[must_use]
    pub fn from_oprf_output(oprf_output: &[u8]) -> ChannelRecoveryKey {
        Self {
            inner: RustRecoveryKey::from_oprf_output(oprf_output),
        }
    }

    /// The 32-byte public key to register with `services/channels`.
    #[wasm_bindgen(js_name = verifyingKey)]
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-recovery-proof` header value for an owner change
    /// of `channelId` to `newOwnerPk` at `nowMs`.
    ///
    /// # Errors
    ///
    /// `channelRecovery.keyLength`.
    #[wasm_bindgen(js_name = signOwnerChange)]
    pub fn sign_owner_change(
        &self,
        channel_id: &[u8],
        new_owner_pk: &[u8],
        now_ms: u64,
    ) -> Result<String, String> {
        Ok(self
            .inner
            .sign_owner_change(
                &array::<16>("channelRecovery", "channelId", channel_id)?,
                &array::<32>("channelRecovery", "newOwnerPk", new_owner_pk)?,
                now_ms,
            )
            .encode())
    }
}

/// The VOPRF input a device blinds to ask auth for `channelId`'s recovery
/// seed.
///
/// # Errors
///
/// `channelRecovery.keyLength`.
#[wasm_bindgen(js_name = channelRecoveryOprfInput)]
pub fn channel_recovery_oprf_input(channel_id: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_oprf_input(&array::<16>(
        "channelRecovery",
        "channelId",
        channel_id,
    )?))
}

/// The request header the owner-change proof travels in.
#[wasm_bindgen(js_name = channelRecoveryProofHeaderName)]
#[must_use]
pub fn channel_recovery_proof_header_name() -> String {
    RECOVERY_PROOF_HEADER_NAME.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn a_continuity_binding_verifies_through_the_surface() {
        let id = Identity::from_seed(&[1; 32]).unwrap();
        let pk = SigningKey::from_seed(&id.mls_identity().ed25519_seed_bytes())
            .verifying_key()
            .to_bytes();
        let binding = sign_continuity_binding(&id, 10 + continuity_window_ms());
        assert_eq!(binding.len(), continuity_binding_len() as usize);
        let key = crate::mailbox::MailboxKey::continuity_from_account_identity(&id);
        assert_eq!(
            verify_continuity_binding(&binding, &pk, 10).unwrap(),
            key.public_key()
        );
        assert!(verify_continuity_binding(&binding, &[0x11; 32], 10).is_err());
    }

    #[test]
    fn a_channel_return_and_a_history_request_round_trip() {
        let old = SigningKey::from_seed(&[3; 32]);
        let channel = [0xC1; 16];
        let sub = [0x5B; 32];
        let hash = [0xAA; 32];
        let sig = old
            .sign(
                &lakko_contacts::channel_return::member_card_binding_bytes_from_hash(
                    &channel, &sub, &hash,
                ),
            )
            .to_bytes();
        let wire = compose_channel_return_envelope(
            &channel,
            ChannelReturnRole::Publisher,
            &sig,
            &sub,
            &hash,
            b"inv",
            vec![2, 5],
            &[[2u8; 32], [5u8; 32]].concat(),
            None,
        )
        .unwrap();
        let ret =
            verify_channel_return_card_binding(&wire, &old.verifying_key().to_bytes()).unwrap();
        assert_eq!(ret.role(), ChannelReturnRole::Publisher);
        assert_eq!(ret.epochs(), vec![2, 5]);
        assert_eq!(ret.seeds().len(), 64);
        assert!(verify_channel_return_card_binding(&wire, &[0x22; 32]).is_err());

        let req = compose_history_request_envelope(&[7; 32], 0).unwrap();
        assert_eq!(req.len(), history_request_envelope_len() as usize);
        let decoded = decode_history_request_envelope(&req).unwrap();
        assert_eq!(decoded.before_message_id(), 0);
        assert_eq!(decoded.gap_key_pub(), vec![7; 32]);
    }

    #[test]
    fn a_recovery_header_parses_in_the_core() {
        let key = ChannelRecoveryKey::from_oprf_output(&[0x41; 64]);
        let header = key.sign_owner_change(&[0x0C; 16], &[0x22; 32], 5).unwrap();
        assert!(lakko_channels::recovery::RecoveryOwnerProof::parse(&header).is_ok());
        assert_eq!(channel_recovery_oprf_input(&[0x0C; 16]).unwrap().len(), 49);
    }
}
