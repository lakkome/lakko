// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for who may read a channel (ADR-0087 sections 6, 7 and
//! 14): a public channel's read key and the public seed envelopes it opens,
//! and a private channel's invitations — the invite key a link's seed
//! derives, the proof it signs at subscribe, and the admission ticket a
//! publisher opens before sealing the seed to a new subscriber.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/channel_access.rs`. The
//! subscriber invitation itself (`composeChannelSubscriberInviteCapability`)
//! and the chat card that carries it (`composeChannelInviteCard`) live in
//! `contacts`; the admission-seed envelope a channel owner sends its
//! co-publishers lives in `knock`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_channels::invite::{
    admission_public_key as rust_admission_public_key,
    generate_admission_seed as rust_generate_admission_seed,
    open_admission_ticket as rust_open_admission_ticket,
    ticket_admission_version as rust_ticket_admission_version, AdmissionTicketError,
    ChannelInviteSigner as RustChannelInviteSigner, ADMISSION_TICKET_LEN, INVITE_KEY_LEN,
    INVITE_PROOF_HEADER_NAME,
};
use lakko_channels::owner::OWNER_PROOF_NONCE_LEN;
use lakko_channels::public_seed::{
    channel_read_key as rust_channel_read_key, open_public_seed as rust_open_public_seed,
    public_seed_epoch as rust_public_seed_epoch, seal_public_seed as rust_seal_public_seed,
    PUBLIC_SEED_ENVELOPE_LEN,
};
use lakko_channels::EpochId;

use crate::channels::{channel_id_from_vec, content_seed, subscriber_pk_from_vec};
use crate::{FfiError, Identity};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; OWNER_PROOF_NONCE_LEN] {
    let mut nonce = [0u8; OWNER_PROOF_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn key32(what: &str, bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Channels(format!(
            "badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

fn ticket_error(e: AdmissionTicketError) -> FfiError {
    FfiError::Channels(format!("admissionTicket.{}: {e}", e.code()))
}

// ============================================================================
// Public channels: read key and public seed envelopes
// ============================================================================

/// The 32-byte read key of `channel_id`, derived from its OWNER's account
/// identity. Public by the owner's choice: it rides the channel's handle
/// record and its subscriber invitations.
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn channel_read_key(
    owner_account_identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    Ok(
        rust_channel_read_key(&owner_account_identity.inner.ed25519_seed_bytes(), &channel)
            .to_vec(),
    )
}

/// Seal the 32-byte `epoch_seed` of `epoch_id` under the read key: the one
/// envelope per epoch a public channel publishes.
///
/// # Errors
///
/// [`FfiError::Channels`] for a wrong-length argument.
#[uniffi::export]
pub fn seal_public_seed_envelope(
    read_key: Vec<u8>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    epoch_seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let key = key32("read key", &read_key)?;
    let channel = channel_id_from_vec(channel_id)?;
    let seed = content_seed(&epoch_seed)?;
    Ok(rust_seal_public_seed(&key, &channel, EpochId::new(epoch_id), &seed)?.to_vec())
}

/// The epoch a public seed envelope names, without opening it; `None` when
/// the length is wrong.
#[uniffi::export]
#[must_use]
pub fn public_seed_envelope_epoch(wire: Vec<u8>) -> Option<u32> {
    rust_public_seed_epoch(&wire).map(EpochId::as_u32)
}

/// Open a public seed envelope fetched for `expected_epoch`, returning the
/// 32-byte epoch seed.
///
/// # Errors
///
/// [`FfiError::Channels`] for another epoch, another read key or channel,
/// tampering, or a wrong-length argument.
#[uniffi::export]
pub fn open_public_seed_envelope(
    read_key: Vec<u8>,
    channel_id: Vec<u8>,
    expected_epoch: u32,
    wire: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let key = key32("read key", &read_key)?;
    let channel = channel_id_from_vec(channel_id)?;
    let seed = rust_open_public_seed(&key, &channel, EpochId::new(expected_epoch), &wire)?;
    Ok(seed.copy_to_bytes_for_export().to_vec())
}

/// Length of a public seed envelope.
#[uniffi::export]
#[must_use]
pub fn public_seed_envelope_len() -> u32 {
    u32::try_from(PUBLIC_SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Private channels: invitations and admission tickets
// ============================================================================

/// A fresh random 32-byte invite seed for one subscriber link. SECRET: it
/// rides the link's sealed preview.
#[uniffi::export]
#[must_use]
pub fn generate_channel_invite_seed() -> Vec<u8> {
    let mut seed = [0u8; INVITE_KEY_LEN];
    lakko_crypto::random::fill(&mut seed);
    seed.to_vec()
}

/// A fresh random 32-byte admission seed (an X25519 secret). The owner keeps
/// it with its version and sends it to the co-publishers
/// (`composeAdmissionSeedEnvelope`).
#[uniffi::export]
#[must_use]
pub fn generate_channel_admission_seed() -> Vec<u8> {
    rust_generate_admission_seed().to_vec()
}

/// The 32-byte X25519 public key of an admission seed: what a link's
/// preview carries.
///
/// # Errors
///
/// [`FfiError::Channels`] when the seed is not 32 bytes.
#[uniffi::export]
pub fn channel_admission_public_key(admission_seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_admission_public_key(&key32("admission seed", &admission_seed)?).to_vec())
}

/// The subscriber's side of a private-channel invitation: the invite key the
/// link's seed derives.
#[derive(uniffi::Object)]
pub struct ChannelInviteSigner {
    inner: RustChannelInviteSigner,
}

#[uniffi::export]
impl ChannelInviteSigner {
    /// The signer of the link whose 32-byte invite seed is `invite_seed`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] when the seed is not 32 bytes.
    #[uniffi::constructor]
    pub fn from_invite_seed(invite_seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: RustChannelInviteSigner::from_invite_seed(&key32("invite seed", &invite_seed)?),
        }))
    }

    /// The 32-byte invite public key the publisher registers with the
    /// channels service.
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-invite-proof` header value for registering
    /// `subscriber_pk` in `channel_id`. Good for one request.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length argument.
    pub fn sign_proof(
        &self,
        channel_id: Vec<u8>,
        subscriber_pk: Vec<u8>,
    ) -> Result<String, FfiError> {
        let channel = channel_id_from_vec(channel_id)?;
        let pk = subscriber_pk_from_vec(&subscriber_pk)?;
        Ok(self
            .inner
            .sign_proof_at(&channel, &pk, now_ms(), fresh_nonce())
            .encode())
    }

    /// Seal the admission ticket for `subscriber_pk` in `channel_id` to the
    /// channel's admission public key of `admission_version` (both from the
    /// link's preview).
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] with `admissionTicket.<code>` first, or a
    /// wrong-length argument.
    pub fn seal_ticket(
        &self,
        admission_public_key: Vec<u8>,
        admission_version: u16,
        channel_id: Vec<u8>,
        subscriber_pk: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let key = key32("admission public key", &admission_public_key)?;
        let channel = channel_id_from_vec(channel_id)?;
        let pk = subscriber_pk_from_vec(&subscriber_pk)?;
        self.inner
            .seal_ticket(&key, admission_version, &channel, &pk)
            .map(|t| t.to_vec())
            .map_err(ticket_error)
    }
}

/// The admission-key version a ticket names, without opening it; `None`
/// when the ticket is not the right shape.
#[uniffi::export]
#[must_use]
pub fn channel_ticket_admission_version(ticket: Vec<u8>) -> Option<u16> {
    rust_ticket_admission_version(&ticket)
}

/// Publisher side: open the admission ticket of the row `(channel_id,
/// subscriber_pk)` with the admission seed of the version the ticket names
/// and verify the invite key's signature. Returns the 32-byte invite public
/// key; seal the seed to the subscriber only when it is one of the channel's
/// live invite keys.
///
/// # Errors
///
/// [`FfiError::Channels`] with `admissionTicket.<code>` first, or a
/// wrong-length argument.
#[uniffi::export]
pub fn open_channel_admission_ticket(
    admission_seed: Vec<u8>,
    channel_id: Vec<u8>,
    subscriber_pk: Vec<u8>,
    ticket: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed = key32("admission seed", &admission_seed)?;
    let channel = channel_id_from_vec(channel_id)?;
    let pk = subscriber_pk_from_vec(&subscriber_pk)?;
    rust_open_admission_ticket(&seed, &channel, &pk, &ticket)
        .map(|k| k.to_vec())
        .map_err(ticket_error)
}

/// Length of an admission ticket.
#[uniffi::export]
#[must_use]
pub fn channel_admission_ticket_len() -> u32 {
    u32::try_from(ADMISSION_TICKET_LEN).unwrap_or(u32::MAX)
}

/// The request header an invite proof travels in.
#[uniffi::export]
#[must_use]
pub fn channel_invite_proof_header_name() -> String {
    INVITE_PROOF_HEADER_NAME.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_channels::invite::{verify_invite_proof, InviteProof};

    const CHANNEL: [u8; 16] = [0x0C; 16];

    #[test]
    fn public_seed_round_trips_under_the_read_key() {
        let owner = Identity::from_seed(vec![0x61; 64]).unwrap();
        let key = channel_read_key(&owner, CHANNEL.to_vec()).unwrap();
        let wire =
            seal_public_seed_envelope(key.clone(), CHANNEL.to_vec(), 3, vec![0x44; 32]).unwrap();
        assert_eq!(wire.len(), public_seed_envelope_len() as usize);
        assert_eq!(public_seed_envelope_epoch(wire.clone()), Some(3));
        assert_eq!(
            open_public_seed_envelope(key.clone(), CHANNEL.to_vec(), 3, wire.clone()).unwrap(),
            vec![0x44; 32]
        );
        assert!(open_public_seed_envelope(key, CHANNEL.to_vec(), 4, wire).is_err());
    }

    #[test]
    fn invite_proof_and_ticket_round_trip() {
        let invite_seed = generate_channel_invite_seed();
        let signer = ChannelInviteSigner::from_invite_seed(invite_seed).unwrap();
        let subscriber = vec![0x77_u8; 32];
        let header = signer
            .sign_proof(CHANNEL.to_vec(), subscriber.clone())
            .unwrap();
        let proof = InviteProof::parse(&header).unwrap();
        let pk: [u8; 32] = subscriber.clone().try_into().unwrap();
        verify_invite_proof(&proof, &CHANNEL, &pk, proof.proof.unix_ms).unwrap();

        let admission = generate_channel_admission_seed();
        let public = channel_admission_public_key(admission.clone()).unwrap();
        let ticket = signer
            .seal_ticket(public, 2, CHANNEL.to_vec(), subscriber.clone())
            .unwrap();
        assert_eq!(ticket.len(), channel_admission_ticket_len() as usize);
        assert_eq!(channel_ticket_admission_version(ticket.clone()), Some(2));
        let invite_key =
            open_channel_admission_ticket(admission, CHANNEL.to_vec(), subscriber, ticket).unwrap();
        assert_eq!(invite_key, signer.verifying_key());
        assert_eq!(
            channel_invite_proof_header_name(),
            "x-lakko-channel-invite-proof"
        );
    }
}
