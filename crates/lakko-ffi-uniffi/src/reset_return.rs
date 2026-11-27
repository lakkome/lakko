// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for ADR-0101: reaching an account after a key reset.
//! Mirror of `crates/lakko-ffi-wasm/src/reset_return.rs`.
//!
//! - The continuity binding (`lakko_contacts::continuity`): the reset
//!   identity signs its continuity mailbox key and the window's end
//!   ([`sign_continuity_binding`]); a peer verifies it under the attested new
//!   key ([`verify_continuity_binding`]). The key itself is
//!   `MailboxKey.continuityFromAccountIdentity`, its DS signer
//!   `dsContinuityMailboxSigner`.
//! - The channel return (`lakko_contacts::channel_return`), a
//!   continuity-mailbox row (`FfiMailboxKind.channelReturn`).
//! - The history request (`lakko_contacts::history_request`), answered with
//!   the ADR-0091 gap history unchanged.
//! - The channel recovery key (`lakko_channels::recovery`), derived from the
//!   owned-channels list key `R` since 2026-11-26 (the OPRF input and
//!   `fromOprfOutput` are removed; the list and its locks are in
//!   `owned_channels.rs`).
//!
//! Every refusal is [`FfiError::ResetReturn`] with `"<code>: <reason>"`.

use std::sync::Arc;

use lakko_channels::recovery::{ChannelRecoveryKey as RustRecoveryKey, RECOVERY_PROOF_HEADER_NAME};
use lakko_contacts::channel_epoch_seeds::ChannelEpochSeed;
use lakko_contacts::channel_return::{
    compose_channel_return as rust_compose_return, decode_channel_return as rust_decode_return,
    ChannelReturn, ChannelReturnError, ChannelReturnRole, CHANNEL_RETURN_MAX_INVITATION_LEN,
};
use lakko_contacts::continuity::{
    verify_continuity_binding as rust_verify_binding, ContinuityBinding, ContinuityError,
    CONTINUITY_BINDING_LEN, CONTINUITY_WINDOW_MS,
};
use lakko_contacts::history_request::{
    compose_history_request as rust_compose_request, decode_history_request as rust_decode_request,
    HistoryRequest, HistoryRequestError, HISTORY_REQUEST_ENVELOPE_LEN,
};

use crate::knock::FfiChannelEpochSeed;
use crate::{FfiError, Identity};

fn err(code: &str, reason: impl core::fmt::Display) -> FfiError {
    FfiError::ResetReturn(format!("{code}: {reason}"))
}

fn continuity_err(e: ContinuityError) -> FfiError {
    err(e.code(), e)
}

fn return_err(e: ChannelReturnError) -> FfiError {
    err(&e.code(), e)
}

fn request_err(e: HistoryRequestError) -> FfiError {
    err(e.code(), e)
}

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        err(
            "keyLength",
            format!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

// ============================================================================
// Continuity binding (ADR-0101 sections 1-2)
// ============================================================================

/// The 105-byte continuity binding of `account_identity`: its continuity
/// mailbox key and `expires_at_ms`, signed by its identity key. Upload it
/// beside the identity bundle after a key reset; set `expires_at_ms` to now
/// plus `continuityWindowMs()`.
#[uniffi::export]
#[must_use]
pub fn sign_continuity_binding(account_identity: &Identity, expires_at_ms: u64) -> Vec<u8> {
    ContinuityBinding::for_account_seed(&account_identity.inner.ed25519_seed_bytes(), expires_at_ms)
        .encode()
        .to_vec()
}

/// Verify a continuity binding served for an account against the 32-byte
/// identity key the caller took from the attested reset, at its clock
/// `now_ms`; returns the 32-byte continuity mailbox key to write to.
///
/// # Errors
///
/// [`FfiError::ResetReturn`]: `keyLength`, `badLength`, `unknownVersion`,
/// `badKey`, `expired`, `expiryTooFar`, `badSignature`. Any of them means
/// "do not write".
#[uniffi::export]
pub fn verify_continuity_binding(
    binding: Vec<u8>,
    identity_pk: Vec<u8>,
    now_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    let pk = array::<32>(&identity_pk, "identityPk")?;
    rust_verify_binding(&binding, &pk, now_ms)
        .map(|k| k.to_vec())
        .map_err(continuity_err)
}

/// The binding's length (105).
#[uniffi::export]
#[must_use]
pub fn continuity_binding_len() -> u32 {
    u32::try_from(CONTINUITY_BINDING_LEN).unwrap_or(u32::MAX)
}

/// The announcement window, ms (30 days; chosen, not measured).
#[uniffi::export]
#[must_use]
pub fn continuity_window_ms() -> u64 {
    CONTINUITY_WINDOW_MS
}

// ============================================================================
// Channel return (ADR-0101 section 7)
// ============================================================================

/// What the recipient was in the channel. Order is the wire order.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiChannelReturnRole {
    /// A subscriber: re-subscribe.
    Subscriber,
    /// A publisher: an owner device admits the new devices.
    Publisher,
}

impl From<FfiChannelReturnRole> for ChannelReturnRole {
    fn from(r: FfiChannelReturnRole) -> Self {
        match r {
            FfiChannelReturnRole::Subscriber => Self::Subscriber,
            FfiChannelReturnRole::Publisher => Self::Publisher,
        }
    }
}

impl From<ChannelReturnRole> for FfiChannelReturnRole {
    fn from(r: ChannelReturnRole) -> Self {
        match r {
            ChannelReturnRole::Subscriber => Self::Subscriber,
            ChannelReturnRole::Publisher => Self::Publisher,
        }
    }
}

/// A channel return. `invitation` and the seeds are secret.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiChannelReturn {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// What the recipient was in the channel.
    pub role: FfiChannelReturnRole,
    /// The 64-byte account binding signature of the recipient's old member
    /// card (`OpenedMemberCard.bindingSig`).
    pub old_binding_sig: Vec<u8>,
    /// The 32-byte subscriber key that card was uploaded under.
    pub old_subscriber_pk: Vec<u8>,
    /// The 32-byte BLAKE3 of that card's profile bytes.
    pub profile_hash: Vec<u8>,
    /// The channel's invitation, opaque here; empty for none.
    pub invitation: Vec<u8>,
    /// Epoch seeds, strictly ascending by epoch, at most 64.
    pub epoch_seeds: Vec<FfiChannelEpochSeed>,
    /// The newest signed profile envelope held for the person, or none.
    pub profile: Option<Vec<u8>>,
}

impl core::fmt::Debug for FfiChannelReturn {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FfiChannelReturn")
            .field("channel_id", &self.channel_id)
            .field("role", &self.role)
            .field("invitation_len", &self.invitation.len())
            .field("epoch_seeds", &self.epoch_seeds)
            .finish_non_exhaustive()
    }
}

impl TryFrom<FfiChannelReturn> for ChannelReturn {
    type Error = FfiError;

    fn try_from(r: FfiChannelReturn) -> Result<Self, FfiError> {
        let epoch_seeds = r
            .epoch_seeds
            .iter()
            .map(|s| {
                Ok(ChannelEpochSeed {
                    epoch: s.epoch,
                    seed: array::<32>(&s.seed, "seed")?,
                })
            })
            .collect::<Result<Vec<_>, FfiError>>()?;
        Ok(Self {
            channel_id: array::<16>(&r.channel_id, "channelId")?,
            role: r.role.into(),
            old_binding_sig: array::<64>(&r.old_binding_sig, "oldBindingSig")?,
            old_subscriber_pk: array::<32>(&r.old_subscriber_pk, "oldSubscriberPk")?,
            profile_hash: array::<32>(&r.profile_hash, "profileHash")?,
            invitation: r.invitation,
            epoch_seeds,
            profile: r.profile,
        })
    }
}

impl From<ChannelReturn> for FfiChannelReturn {
    fn from(r: ChannelReturn) -> Self {
        Self {
            channel_id: r.channel_id.to_vec(),
            role: r.role.into(),
            old_binding_sig: r.old_binding_sig.to_vec(),
            old_subscriber_pk: r.old_subscriber_pk.to_vec(),
            profile_hash: r.profile_hash.to_vec(),
            invitation: r.invitation.clone(),
            epoch_seeds: r
                .epoch_seeds
                .iter()
                .map(|s| FfiChannelEpochSeed {
                    epoch: s.epoch,
                    seed: s.seed.to_vec(),
                })
                .collect(),
            profile: r.profile.clone(),
        }
    }
}

/// Compose a channel return, the body of a `channelReturn` mailbox row to
/// the recipient's continuity mailbox.
///
/// # Errors
///
/// [`FfiError::ResetReturn`]: `keyLength`, `invitationTooLong`,
/// `tooManySeeds`, `seedsNotAscending`, `profileLength`, `profile.*`.
#[uniffi::export]
pub fn compose_channel_return_envelope(ret: FfiChannelReturn) -> Result<Vec<u8>, FfiError> {
    rust_compose_return(&ret.try_into()?).map_err(return_err)
}

/// Decode a channel return. Layout only; see
/// [`verify_channel_return_card_binding`].
///
/// # Errors
///
/// [`FfiError::ResetReturn`] with a `ChannelReturnError` code.
#[uniffi::export]
pub fn decode_channel_return_envelope(plaintext: Vec<u8>) -> Result<FfiChannelReturn, FfiError> {
    rust_decode_return(&plaintext)
        .map(Into::into)
        .map_err(return_err)
}

/// Decode a channel return and verify its old card binding against this
/// account's 32-byte **previous** identity key (from the KT log). Act on a
/// return without a prompt only when this succeeds; otherwise show it as an
/// ordinary invitation.
///
/// # Errors
///
/// [`FfiError::ResetReturn`]: every decode code, `keyLength`, `badKey`,
/// `badSignature`.
#[uniffi::export]
pub fn verify_channel_return_card_binding(
    plaintext: Vec<u8>,
    previous_identity_pk: Vec<u8>,
) -> Result<FfiChannelReturn, FfiError> {
    let pk = array::<32>(&previous_identity_pk, "previousIdentityPk")?;
    let ret = rust_decode_return(&plaintext).map_err(return_err)?;
    ret.verify_old_card_binding(&pk).map_err(return_err)?;
    Ok(ret.into())
}

/// The longest invitation a channel return carries.
#[uniffi::export]
#[must_use]
pub fn channel_return_max_invitation_len() -> u32 {
    u32::try_from(CHANNEL_RETURN_MAX_INVITATION_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// History request (ADR-0101 section 9)
// ============================================================================

/// A decoded history request.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHistoryRequest {
    /// The 32-byte public gap key the answer is sealed to.
    pub gap_key_pub: Vec<u8>,
    /// Answer with messages older than this DS id; 0 = the newest.
    pub before_message_id: i64,
}

/// Compose a history request to send in the conversation.
///
/// # Errors
///
/// [`FfiError::ResetReturn`]: `keyLength`, `badMessageId` (negative id).
#[uniffi::export]
pub fn compose_history_request_envelope(
    gap_key_pub: Vec<u8>,
    before_message_id: i64,
) -> Result<Vec<u8>, FfiError> {
    rust_compose_request(&HistoryRequest {
        gap_key_pub: array::<32>(&gap_key_pub, "gapKeyPub")?,
        before_message_id,
    })
    .map_err(request_err)
}

/// Decode a history request.
///
/// # Errors
///
/// [`FfiError::ResetReturn`] with a `HistoryRequestError` code.
#[uniffi::export]
pub fn decode_history_request_envelope(plaintext: Vec<u8>) -> Result<FfiHistoryRequest, FfiError> {
    rust_decode_request(&plaintext)
        .map(|r| FfiHistoryRequest {
            gap_key_pub: r.gap_key_pub.to_vec(),
            before_message_id: r.before_message_id,
        })
        .map_err(request_err)
}

/// The history request's length (66).
#[uniffi::export]
#[must_use]
pub fn history_request_envelope_len() -> u32 {
    u32::try_from(HISTORY_REQUEST_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Channel recovery key (ADR-0101 section 7, "Owner")
// ============================================================================

/// A channel's recovery key. Holds the secret.
#[derive(uniffi::Object)]
pub struct ChannelRecoveryKey {
    inner: RustRecoveryKey,
}

#[uniffi::export]
impl ChannelRecoveryKey {
    /// The key from its 32-byte seed.
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetReturn`] `keyLength`.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: RustRecoveryKey::from_seed(&array::<32>(&seed, "seed")?),
        }))
    }

    /// The key of `channel_id` from the owned-channels list key `R`
    /// (ADR-0101 section 7 item 5).
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetReturn`] `keyLength`.
    #[uniffi::constructor]
    pub fn from_list_key(list_key: Vec<u8>, channel_id: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: RustRecoveryKey::from_list_key(
                &array::<32>(&list_key, "listKey")?,
                &array::<16>(&channel_id, "channelId")?,
            ),
        }))
    }

    /// The 32-byte public key to register with `services/channels`.
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-recovery-proof` header value for an owner change
    /// of `channel_id` to `new_owner_pk` at `now_ms`.
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetReturn`] `keyLength`.
    pub fn sign_owner_change(
        &self,
        channel_id: Vec<u8>,
        new_owner_pk: Vec<u8>,
        now_ms: u64,
    ) -> Result<String, FfiError> {
        Ok(self
            .inner
            .sign_owner_change(
                &array::<16>(&channel_id, "channelId")?,
                &array::<32>(&new_owner_pk, "newOwnerPk")?,
                now_ms,
            )
            .encode())
    }
}

/// The request header the owner-change proof travels in.
#[uniffi::export]
#[must_use]
pub fn channel_recovery_proof_header_name() -> String {
    RECOVERY_PROOF_HEADER_NAME.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use lakko_channels::recovery::{verify_recovery_owner_change, RecoveryOwnerProof};
    use lakko_crypto::signing::SigningKey;

    fn identity(byte: u8) -> Identity {
        Identity::from_seed(vec![byte; 32]).unwrap()
    }

    fn identity_pk(id: &Identity) -> Vec<u8> {
        SigningKey::from_seed(&id.inner.ed25519_seed_bytes())
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn a_continuity_binding_round_trips_through_the_surface() {
        let id = identity(1);
        let now = 1_800_000_000_000;
        let binding = sign_continuity_binding(&id, now + continuity_window_ms());
        assert_eq!(binding.len(), continuity_binding_len() as usize);
        let key = crate::mailbox::MailboxKey::continuity_from_account_identity(&id);
        assert_eq!(
            verify_continuity_binding(binding.clone(), identity_pk(&id), now).unwrap(),
            key.public_key()
        );
        assert_ne!(
            key.public_key(),
            crate::mailbox::MailboxKey::from_account_identity(&id).public_key()
        );
        let e = verify_continuity_binding(binding, identity_pk(&identity(2)), now).unwrap_err();
        assert!(e.to_string().starts_with("badSignature"), "{e}");
        // The DS signer names the continuity mailbox.
        assert_eq!(
            crate::ds_proof::ds_continuity_mailbox_signer(&id).mailbox_address(),
            key.address()
        );
    }

    #[test]
    fn a_channel_return_round_trips_and_its_binding_verifies() {
        let old = identity(3);
        let old_signing = SigningKey::from_seed(&old.inner.ed25519_seed_bytes());
        let channel = [0xC1; 16];
        let sub = [0x5B; 32];
        let hash = [0xAA; 32];
        let sig = old_signing
            .sign(
                &lakko_contacts::channel_return::member_card_binding_bytes_from_hash(
                    &channel, &sub, &hash,
                ),
            )
            .to_bytes();
        let ret = FfiChannelReturn {
            channel_id: channel.to_vec(),
            role: FfiChannelReturnRole::Publisher,
            old_binding_sig: sig.to_vec(),
            old_subscriber_pk: sub.to_vec(),
            profile_hash: hash.to_vec(),
            invitation: Vec::new(),
            epoch_seeds: vec![FfiChannelEpochSeed {
                epoch: 4,
                seed: vec![4; 32],
            }],
            profile: None,
        };
        let wire = compose_channel_return_envelope(ret.clone()).unwrap();
        assert_eq!(decode_channel_return_envelope(wire.clone()).unwrap(), ret);
        assert_eq!(
            verify_channel_return_card_binding(wire.clone(), identity_pk(&old)).unwrap(),
            ret
        );
        assert!(
            verify_channel_return_card_binding(wire.clone(), identity_pk(&identity(4))).is_err()
        );
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::ChannelReturn
        );
    }

    #[test]
    fn a_history_request_round_trips_and_dispatches() {
        let wire = compose_history_request_envelope(vec![7; 32], 99).unwrap();
        assert_eq!(wire.len(), history_request_envelope_len() as usize);
        assert_eq!(
            decode_history_request_envelope(wire.clone()).unwrap(),
            FfiHistoryRequest {
                gap_key_pub: vec![7; 32],
                before_message_id: 99
            }
        );
        assert!(compose_history_request_envelope(vec![7; 32], -1).is_err());
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::HistoryRequest
        );
    }

    #[test]
    fn a_recovery_owner_change_signed_here_verifies_in_the_core() {
        let key = ChannelRecoveryKey::from_list_key(vec![0x41; 32], vec![0x0C; 16]).unwrap();
        let new_owner = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        let header = key
            .sign_owner_change(vec![0x0C; 16], new_owner.to_vec(), 1_000_000)
            .unwrap();
        let proof = RecoveryOwnerProof::parse(&header).unwrap();
        let rk: [u8; 32] = key.verifying_key().try_into().unwrap();
        assert_eq!(
            verify_recovery_owner_change(&proof, &[0x0C; 16], &new_owner, &rk, 1_000_000),
            Ok(())
        );
        assert!(ChannelRecoveryKey::from_seed(vec![1; 31]).is_err());
        assert!(ChannelRecoveryKey::from_list_key(vec![1; 31], vec![0; 16]).is_err());
        assert_eq!(
            channel_recovery_proof_header_name(),
            "x-lakko-channel-recovery-proof"
        );
    }
}
