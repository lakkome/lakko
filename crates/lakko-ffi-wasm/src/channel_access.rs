// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for who may read a channel (ADR-0087 sections 6, 7
//! and 14): the read key and public seed envelopes of a public channel, and
//! the invite signer, proof and admission ticket of a private one.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/channel_access.rs`.
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`.

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
use lakko_channels::{ContentKeySeed, EpochId, CHANNEL_ID_LEN, SUBSCRIBER_PK_LEN};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

/// The clock a proof is signed at: `Date.now()` in the browser, the system
/// clock on the host. A broken clock reads as 0 and the server refuses.
#[cfg(target_arch = "wasm32")]
pub(crate) fn now_ms() -> u64 {
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = now as u64;
        ms
    } else {
        0
    }
}

/// The clock a proof is signed at (host build).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn fresh_nonce() -> [u8; OWNER_PROOF_NONCE_LEN] {
    let mut nonce = [0u8; OWNER_PROOF_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

pub(crate) fn fixed<const N: usize>(
    area: &str,
    what: &str,
    bytes: &[u8],
) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        format!(
            "{area}.badLength: {what} must be {N} bytes, got {}",
            bytes.len()
        )
    })
}

pub(crate) fn channel16(area: &str, bytes: &[u8]) -> Result<[u8; CHANNEL_ID_LEN], String> {
    fixed::<CHANNEL_ID_LEN>(area, "channel id", bytes)
}

pub(crate) fn seed32(area: &str, bytes: &[u8]) -> Result<ContentKeySeed, String> {
    Ok(ContentKeySeed::from_bytes_for_test(fixed::<32>(
        area,
        "epoch seed",
        bytes,
    )?))
}

fn ticket_error(e: AdmissionTicketError) -> String {
    format!("admissionTicket.{}: {e}", e.code())
}

/// The 32-byte read key of `channelId`, from its OWNER's account identity.
///
/// # Errors
///
/// Throws `publicSeed.badLength: ...` for a bad channel id.
#[wasm_bindgen(js_name = channelReadKey)]
pub fn channel_read_key(
    owner_account_identity: &Identity,
    channel_id: &[u8],
) -> Result<Vec<u8>, String> {
    let channel = channel16("publicSeed", channel_id)?;
    Ok(rust_channel_read_key(
        &owner_account_identity.mls_identity().ed25519_seed_bytes(),
        &channel,
    )
    .to_vec())
}

/// Seal the 32-byte seed of `epochId` under the read key.
///
/// # Errors
///
/// Throws `publicSeed.<code>: ...`.
#[wasm_bindgen(js_name = sealPublicSeedEnvelope)]
pub fn seal_public_seed_envelope(
    read_key: &[u8],
    channel_id: &[u8],
    epoch_id: u32,
    epoch_seed: &[u8],
) -> Result<Vec<u8>, String> {
    let key = fixed::<32>("publicSeed", "read key", read_key)?;
    let channel = channel16("publicSeed", channel_id)?;
    let seed = seed32("publicSeed", epoch_seed)?;
    rust_seal_public_seed(&key, &channel, EpochId::new(epoch_id), &seed)
        .map(|w| w.to_vec())
        .map_err(|e| format!("publicSeed.seal: {e}"))
}

/// The epoch a public seed envelope names, or `undefined`.
#[wasm_bindgen(js_name = publicSeedEnvelopeEpoch)]
#[must_use]
pub fn public_seed_envelope_epoch(wire: &[u8]) -> Option<u32> {
    rust_public_seed_epoch(wire).map(EpochId::as_u32)
}

/// Open a public seed envelope fetched for `expectedEpoch`.
///
/// # Errors
///
/// Throws `publicSeed.open: ...` for another epoch, key or channel, or
/// tampering.
#[wasm_bindgen(js_name = openPublicSeedEnvelope)]
pub fn open_public_seed_envelope(
    read_key: &[u8],
    channel_id: &[u8],
    expected_epoch: u32,
    wire: &[u8],
) -> Result<Vec<u8>, String> {
    let key = fixed::<32>("publicSeed", "read key", read_key)?;
    let channel = channel16("publicSeed", channel_id)?;
    rust_open_public_seed(&key, &channel, EpochId::new(expected_epoch), wire)
        .map(|s| s.copy_to_bytes_for_export().to_vec())
        .map_err(|e| format!("publicSeed.open: {e}"))
}

/// Length of a public seed envelope.
#[wasm_bindgen(js_name = publicSeedEnvelopeLen)]
#[must_use]
pub fn public_seed_envelope_len() -> u32 {
    u32::try_from(PUBLIC_SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// A fresh random 32-byte invite seed for one subscriber link (SECRET).
#[wasm_bindgen(js_name = generateChannelInviteSeed)]
#[must_use]
pub fn generate_channel_invite_seed() -> Vec<u8> {
    let mut seed = [0u8; INVITE_KEY_LEN];
    lakko_crypto::random::fill(&mut seed);
    seed.to_vec()
}

/// A fresh random 32-byte admission seed (an X25519 secret).
#[wasm_bindgen(js_name = generateChannelAdmissionSeed)]
#[must_use]
pub fn generate_channel_admission_seed() -> Vec<u8> {
    rust_generate_admission_seed().to_vec()
}

/// The X25519 public key of an admission seed.
///
/// # Errors
///
/// Throws `admissionTicket.badLength: ...`.
#[wasm_bindgen(js_name = channelAdmissionPublicKey)]
pub fn channel_admission_public_key(admission_seed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_admission_public_key(&fixed::<32>(
        "admissionTicket",
        "admission seed",
        admission_seed,
    )?)
    .to_vec())
}

/// The subscriber's side of a private-channel invitation.
#[wasm_bindgen]
pub struct ChannelInviteSigner {
    inner: RustChannelInviteSigner,
}

#[wasm_bindgen]
impl ChannelInviteSigner {
    /// The signer of the link whose 32-byte invite seed is `inviteSeed`.
    ///
    /// # Errors
    ///
    /// Throws `invite.badLength: ...`.
    #[wasm_bindgen(js_name = fromInviteSeed)]
    pub fn from_invite_seed(invite_seed: &[u8]) -> Result<ChannelInviteSigner, String> {
        Ok(Self {
            inner: RustChannelInviteSigner::from_invite_seed(&fixed::<32>(
                "invite",
                "invite seed",
                invite_seed,
            )?),
        })
    }

    /// The 32-byte invite public key the publisher registers.
    #[wasm_bindgen(getter, js_name = verifyingKey)]
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-invite-proof` header value for registering
    /// `subscriberPk` in `channelId`. Good for one request.
    ///
    /// # Errors
    ///
    /// Throws `invite.badLength: ...`.
    #[wasm_bindgen(js_name = signProof)]
    pub fn sign_proof(&self, channel_id: &[u8], subscriber_pk: &[u8]) -> Result<String, String> {
        let channel = channel16("invite", channel_id)?;
        let pk = fixed::<SUBSCRIBER_PK_LEN>("invite", "subscriber key", subscriber_pk)?;
        Ok(self
            .inner
            .sign_proof_at(&channel, &pk, now_ms(), fresh_nonce())
            .encode())
    }

    /// Seal the admission ticket for `subscriberPk` in `channelId` to the
    /// channel's admission public key of `admissionVersion`.
    ///
    /// # Errors
    ///
    /// Throws `admissionTicket.<code>: ...`.
    #[wasm_bindgen(js_name = sealTicket)]
    pub fn seal_ticket(
        &self,
        admission_public_key: &[u8],
        admission_version: u16,
        channel_id: &[u8],
        subscriber_pk: &[u8],
    ) -> Result<Vec<u8>, String> {
        let key = fixed::<32>(
            "admissionTicket",
            "admission public key",
            admission_public_key,
        )?;
        let channel = channel16("admissionTicket", channel_id)?;
        let pk = fixed::<SUBSCRIBER_PK_LEN>("admissionTicket", "subscriber key", subscriber_pk)?;
        self.inner
            .seal_ticket(&key, admission_version, &channel, &pk)
            .map(|t| t.to_vec())
            .map_err(ticket_error)
    }
}

/// The admission-key version a ticket names, or `undefined`.
#[wasm_bindgen(js_name = channelTicketAdmissionVersion)]
#[must_use]
pub fn channel_ticket_admission_version(ticket: &[u8]) -> Option<u16> {
    rust_ticket_admission_version(ticket)
}

/// Publisher side: open an admission ticket and verify the invite key's
/// signature; returns the 32-byte invite public key.
///
/// # Errors
///
/// Throws `admissionTicket.<code>: ...`.
#[wasm_bindgen(js_name = openChannelAdmissionTicket)]
pub fn open_channel_admission_ticket(
    admission_seed: &[u8],
    channel_id: &[u8],
    subscriber_pk: &[u8],
    ticket: &[u8],
) -> Result<Vec<u8>, String> {
    let seed = fixed::<32>("admissionTicket", "admission seed", admission_seed)?;
    let channel = channel16("admissionTicket", channel_id)?;
    let pk = fixed::<SUBSCRIBER_PK_LEN>("admissionTicket", "subscriber key", subscriber_pk)?;
    rust_open_admission_ticket(&seed, &channel, &pk, ticket)
        .map(|k| k.to_vec())
        .map_err(ticket_error)
}

/// Length of an admission ticket.
#[wasm_bindgen(js_name = channelAdmissionTicketLen)]
#[must_use]
pub fn channel_admission_ticket_len() -> u32 {
    u32::try_from(ADMISSION_TICKET_LEN).unwrap_or(u32::MAX)
}

/// The request header an invite proof travels in.
#[wasm_bindgen(js_name = channelInviteProofHeaderName)]
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
    fn public_seed_and_invite_round_trip() {
        let owner = Identity::from_seed(&[0x61; 64]).unwrap();
        let key = channel_read_key(&owner, &CHANNEL).unwrap();
        let wire = seal_public_seed_envelope(&key, &CHANNEL, 3, &[0x44; 32]).unwrap();
        assert_eq!(public_seed_envelope_epoch(&wire), Some(3));
        assert_eq!(
            open_public_seed_envelope(&key, &CHANNEL, 3, &wire).unwrap(),
            vec![0x44; 32]
        );
        assert!(open_public_seed_envelope(&key, &CHANNEL, 4, &wire).is_err());

        let signer =
            ChannelInviteSigner::from_invite_seed(&generate_channel_invite_seed()).unwrap();
        let subscriber = [0x77_u8; 32];
        let proof = InviteProof::parse(&signer.sign_proof(&CHANNEL, &subscriber).unwrap()).unwrap();
        verify_invite_proof(&proof, &CHANNEL, &subscriber, proof.proof.unix_ms).unwrap();
        let admission = generate_channel_admission_seed();
        let public = channel_admission_public_key(&admission).unwrap();
        let ticket = signer
            .seal_ticket(&public, 2, &CHANNEL, &subscriber)
            .unwrap();
        assert_eq!(channel_ticket_admission_version(&ticket), Some(2));
        assert_eq!(
            open_channel_admission_ticket(&admission, &CHANNEL, &subscriber, &ticket).unwrap(),
            signer.verifying_key()
        );
    }
}
