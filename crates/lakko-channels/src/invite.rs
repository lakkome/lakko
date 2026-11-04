// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A private channel's invitations ([ADR-0087] section 7): the invite key a
//! link derives, the proof the channels service checks at the door, and the
//! sealed admission ticket the publisher checks before it seals a seed.
//!
//! ## The invite key
//!
//! ```text
//! invite_signing_seed = BLAKE3-derive-key("lakko/channels/invite-key/v1", invite_seed)
//! invite_key          = Ed25519 from invite_signing_seed
//! ```
//!
//! `invite_seed` (32 bytes) travels in the link's sealed preview
//! (`lakko_contacts::group_invite`, preview kind 3). The publisher registers
//! the public key with the service (`PUT /v1/channels/{id}/invite-keys/{pk}`)
//! with an expiry and a use count. It names no account.
//!
//! ## The invite proof (the door)
//!
//! ```text
//! X-Lakko-Channel-Invite-Proof: v1.<invite_pk_b64url>.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! canonical = "lakko/channels/invite-proof/v1" ‖ 0x00 ‖ channel_id(16) ‖ subscriber_pk(32)
//!             ‖ unix_ms(8, BE) ‖ nonce(16)
//! ```
//!
//! The header names the key so the service checks one signature. The
//! service also checks the key is registered and live for the channel and
//! refuses a repeated nonce (both state); [`verify_invite_proof`] is pure and
//! checks the window first, then the signature. Every refusal is the one
//! `401 invite-proof-required`, before a token is spent.
//!
//! ## The admission ticket (the publisher)
//!
//! The server is not trusted, so the publisher does not seal a seed to a row
//! because the door let it in. Each subscriber row carries a ticket, a
//! sealed box to the channel's **admission key** of a given version:
//!
//! ```text
//! ticket   = version(1)=0x01 ‖ admission_version(2, BE) ‖ eph_x25519_pk(32)
//!            ‖ ChaCha20-Poly1305(plain) ‖ tag(16)                          (147 bytes)
//! plain    = invite_pk(32) ‖ sig(64)
//! sig      = Ed25519_invite(TICKET_DOMAIN ‖ channel_id ‖ subscriber_pk)
//! AAD      = TICKET_DOMAIN ‖ 0x01 ‖ admission_version ‖ channel_id ‖ subscriber_pk
//! ```
//!
//! **The admission key** (maintainer, 2026-11-02) is a random 32-byte X25519
//! seed with a `u16` version, minted by the owner ([`generate_admission_seed`]).
//! The owner hands each version to every co-publisher, sealed to that
//! co-publisher's account X25519 key (`lakko_contacts::admission_seed`), so
//! any publisher can check a ticket. When a co-publisher leaves or is
//! removed the owner mints version + 1 and hands it to the remaining
//! co-publishers; new links carry the new public key and version. Older
//! versions are kept, so tickets from older links still open (accepted: a
//! removed co-publisher can still open tickets from old links, which tells
//! it which link a subscriber came by, nothing more).
//!
//! The version is in the clear so the publisher picks the seed that opens
//! the ticket ([`ticket_admission_version`]), and in the AAD so it cannot be
//! rewritten. The AAD also binds the ticket to the row's
//! `(channel, subscriber)`, so a ticket copied onto another row does not
//! open. Sealed, so a copy of the table does not show which subscribers came
//! by one link.
//! [`open_admission_ticket`] returns the invite public key; the publisher
//! seals only when it is one of the channel's live invite keys.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::aead::{self, AeadKey, Algorithm, KEY_LEN, NONCE_LEN, TAG_LEN};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::hash;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::member_card::SUBSCRIBER_PK_LEN;
use crate::owner::{OwnerProof, OwnerProofError, OWNER_PROOF_NONCE_LEN, OWNER_PROOF_WINDOW_MS};
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of an invite signing seed. Frozen.
pub const INVITE_KEY_DOMAIN: &str = "lakko/channels/invite-key/v1";
/// Domain separator that opens the invite proof's canonical bytes. Frozen.
pub const INVITE_PROOF_DOMAIN: &[u8] = b"lakko/channels/invite-proof/v1";
/// The request header the invite proof travels in (lowercase).
pub const INVITE_PROOF_HEADER_NAME: &str = "x-lakko-channel-invite-proof";
/// Upper bound on the header's length; anything longer is refused unparsed.
pub const INVITE_PROOF_MAX_HEADER_LEN: usize = 256;
/// Domain separator of the ticket signature and AAD. Frozen.
pub const ADMISSION_TICKET_DOMAIN: &[u8] = b"lakko/channels/admission-ticket/v1\x00";
/// The ticket's first byte.
pub const ADMISSION_TICKET_VERSION_V1: u8 = 0x01;
/// Length of an invite seed and of an invite or admission public key.
pub const INVITE_KEY_LEN: usize = 32;

const SIG_LEN: usize = 64;
const EPH_LEN: usize = 32;
const TICKET_PLAIN_LEN: usize = INVITE_KEY_LEN + SIG_LEN;

/// The fixed length of an admission ticket.
pub const ADMISSION_TICKET_LEN: usize = 1 + 2 + EPH_LEN + TICKET_PLAIN_LEN + TAG_LEN;

/// Length of an admission seed.
pub const ADMISSION_SEED_LEN: usize = 32;

const HKDF_INFO_KEY: &[u8] = b"lakko/channels/admission-ticket/v1/key";
const HKDF_INFO_NONCE: &[u8] = b"lakko/channels/admission-ticket/v1/nonce";
const ALG: Algorithm = Algorithm::ChaCha20Poly1305;
const VERSION: &str = "v1";

/// Derive the Ed25519 signing seed of a link's invite key.
#[must_use]
pub fn invite_signing_seed(invite_seed: &[u8; INVITE_KEY_LEN]) -> [u8; 32] {
    hash::blake3_derive_key_from_material(INVITE_KEY_DOMAIN, invite_seed)
}

/// Mint a fresh admission seed (an X25519 secret). The owner keeps it with
/// its version and hands it to the co-publishers.
#[must_use]
pub fn generate_admission_seed() -> [u8; ADMISSION_SEED_LEN] {
    let mut seed = [0u8; ADMISSION_SEED_LEN];
    lakko_crypto::random::fill(&mut seed);
    seed
}

/// The X25519 public key of an admission seed: what a link's preview
/// carries.
#[must_use]
pub fn admission_public_key(admission_seed: &[u8; ADMISSION_SEED_LEN]) -> [u8; INVITE_KEY_LEN] {
    X25519SecretKey::from_bytes(*admission_seed)
        .public_key()
        .to_bytes()
}

/// The subscriber's side of an invitation: the invite key a link's seed
/// derives. Clock and nonce are the caller's.
pub struct ChannelInviteSigner {
    key: SigningKey,
}

impl core::fmt::Debug for ChannelInviteSigner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelInviteSigner")
            .field("verifying_key", &self.key.verifying_key().to_bytes())
            .finish_non_exhaustive()
    }
}

impl ChannelInviteSigner {
    /// The signer of the link whose invite seed is `invite_seed`.
    #[must_use]
    pub fn from_invite_seed(invite_seed: &[u8; INVITE_KEY_LEN]) -> Self {
        let mut seed = invite_signing_seed(invite_seed);
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self { key }
    }

    /// The invite public key the publisher registers.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// The invite proof for registering `subscriber_pk` in `channel_id`.
    #[must_use]
    pub fn sign_proof_at(
        &self,
        channel_id: &[u8; CHANNEL_ID_LEN],
        subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> InviteProof {
        let bytes = invite_proof_canonical_bytes(channel_id, subscriber_pk, now_ms, &nonce);
        InviteProof {
            invite_key: self.key.verifying_key().to_bytes(),
            proof: OwnerProof {
                unix_ms: now_ms,
                nonce,
                signature: self.key.sign(&bytes).to_bytes(),
            },
        }
    }

    /// Seal the admission ticket for `subscriber_pk` in `channel_id` to the
    /// channel's admission public key of `admission_version` (both from the
    /// link's preview).
    ///
    /// # Errors
    ///
    /// [`AdmissionTicketError::BadKey`] when the admission key is unusable,
    /// [`AdmissionTicketError::Seal`] (not expected).
    pub fn seal_ticket(
        &self,
        admission_public_key: &[u8; INVITE_KEY_LEN],
        admission_version: u16,
        channel_id: &[u8; CHANNEL_ID_LEN],
        subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    ) -> Result<[u8; ADMISSION_TICKET_LEN], AdmissionTicketError> {
        let recipient = X25519PublicKey::from_bytes(admission_public_key)
            .map_err(|_| AdmissionTicketError::BadKey)?;
        let signature = self
            .key
            .sign(&ticket_signed_bytes(channel_id, subscriber_pk));
        let mut plain = [0u8; TICKET_PLAIN_LEN];
        plain[..INVITE_KEY_LEN].copy_from_slice(&self.key.verifying_key().to_bytes());
        plain[INVITE_KEY_LEN..].copy_from_slice(&signature.to_bytes());

        let eph = X25519SecretKey::generate();
        let eph_pub = eph.public_key().to_bytes();
        let shared = eph.diffie_hellman(&recipient);
        let (key, nonce) = ticket_key_and_nonce(shared.as_bytes(), &eph_pub)?;
        let ciphertext = aead::seal(
            ALG,
            &key,
            &nonce,
            &ticket_aad(admission_version, channel_id, subscriber_pk),
            &plain,
        )
        .map_err(|_| AdmissionTicketError::Seal)?;
        if ciphertext.len() != TICKET_PLAIN_LEN + TAG_LEN {
            return Err(AdmissionTicketError::Seal);
        }
        let mut out = [0u8; ADMISSION_TICKET_LEN];
        out[0] = ADMISSION_TICKET_VERSION_V1;
        out[1..3].copy_from_slice(&admission_version.to_be_bytes());
        out[3..3 + EPH_LEN].copy_from_slice(&eph_pub);
        out[3 + EPH_LEN..].copy_from_slice(&ciphertext);
        Ok(out)
    }
}

/// The bytes an invite proof signs.
#[must_use]
pub fn invite_proof_canonical_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    unix_ms: u64,
    nonce: &[u8; OWNER_PROOF_NONCE_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        INVITE_PROOF_DOMAIN.len() + 1 + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN + 8 + nonce.len(),
    );
    out.extend_from_slice(INVITE_PROOF_DOMAIN);
    out.push(0);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// A parsed invite proof: the invite key it names and the time, nonce and
/// signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteProof {
    /// The invite public key that signed; the service checks it is live.
    pub invite_key: [u8; INVITE_KEY_LEN],
    /// Time, nonce and signature, as in an owner proof.
    pub proof: OwnerProof,
}

impl InviteProof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.invite_key),
            self.proof.unix_ms,
            URL_SAFE_NO_PAD.encode(self.proof.nonce),
            URL_SAFE_NO_PAD.encode(self.proof.signature),
        )
    }

    /// Parse a header value. Bounded and fail-closed: five dot-separated
    /// fields, a 32-byte key, a decimal time, a 16-byte nonce and a 64-byte
    /// signature in base64url without padding, nothing else.
    ///
    /// # Errors
    ///
    /// [`OwnerProofError::UnknownVersion`] or [`OwnerProofError::Malformed`].
    pub fn parse(value: &str) -> Result<Self, OwnerProofError> {
        if value.len() > INVITE_PROOF_MAX_HEADER_LEN {
            return Err(OwnerProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(OwnerProofError::Malformed)?;
        let key = parts.next().ok_or(OwnerProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(OwnerProofError::Malformed)?;
        let nonce = parts.next().ok_or(OwnerProofError::Malformed)?;
        let signature = parts.next().ok_or(OwnerProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(OwnerProofError::Malformed);
        }
        if version != VERSION {
            return Err(OwnerProofError::UnknownVersion);
        }
        let invite_key: [u8; INVITE_KEY_LEN] = URL_SAFE_NO_PAD
            .decode(key)
            .map_err(|_| OwnerProofError::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| OwnerProofError::Malformed)?;
        Ok(Self {
            invite_key,
            proof: OwnerProof::from_fields(unix_ms, nonce, signature)?,
        })
    }
}

/// Verify `proof` for registering `subscriber_pk` in `channel_id` under the
/// invite key it names, with `now_ms` the verifier's clock. Whether the key
/// is live for the channel, and whether the nonce is fresh, are the
/// service's (state).
///
/// # Errors
///
/// [`OwnerProofError::OutsideWindow`] or [`OwnerProofError::BadSignature`]
/// (a key that is not a usable curve point included).
pub fn verify_invite_proof(
    proof: &InviteProof,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    now_ms: u64,
) -> Result<(), OwnerProofError> {
    if now_ms.abs_diff(proof.proof.unix_ms) > OWNER_PROOF_WINDOW_MS {
        return Err(OwnerProofError::OutsideWindow);
    }
    let key =
        VerifyingKey::from_bytes(&proof.invite_key).map_err(|_| OwnerProofError::BadSignature)?;
    if key.is_weak() {
        return Err(OwnerProofError::BadSignature);
    }
    let bytes = invite_proof_canonical_bytes(
        channel_id,
        subscriber_pk,
        proof.proof.unix_ms,
        &proof.proof.nonce,
    );
    let signature =
        Signature::from_bytes(&proof.proof.signature).map_err(|_| OwnerProofError::BadSignature)?;
    key.verify(&bytes, &signature)
        .map_err(|_| OwnerProofError::BadSignature)
}

/// Why an admission ticket was refused. Every variant means "do not seal to
/// this row".
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionTicketError {
    /// Not [`ADMISSION_TICKET_LEN`] bytes.
    #[error("admission ticket has the wrong length")]
    BadLength,
    /// The first byte is not [`ADMISSION_TICKET_VERSION_V1`].
    #[error("admission ticket carries unknown version {0:#04x}")]
    UnknownVersion(u8),
    /// A key is unusable.
    #[error("admission ticket key is unusable")]
    BadKey,
    /// The AEAD refused: another admission key, another row, tampering.
    #[error("admission ticket did not open")]
    Open,
    /// The invite key's signature over the row does not verify.
    #[error("admission ticket signature does not verify")]
    BadSignature,
    /// Sealing failed.
    #[error("admission ticket could not be sealed")]
    Seal,
}

impl AdmissionTicketError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadKey => "badKey",
            Self::Open => "open",
            Self::BadSignature => "badSignature",
            Self::Seal => "seal",
        }
    }
}

fn ticket_signed_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(ADMISSION_TICKET_DOMAIN.len() + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN);
    out.extend_from_slice(ADMISSION_TICKET_DOMAIN);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out
}

fn ticket_aad(
    admission_version: u16,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(ADMISSION_TICKET_DOMAIN.len() + 3 + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN);
    out.extend_from_slice(ADMISSION_TICKET_DOMAIN);
    out.push(ADMISSION_TICKET_VERSION_V1);
    out.extend_from_slice(&admission_version.to_be_bytes());
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out
}

fn ticket_key_and_nonce(
    shared: &[u8; 32],
    eph_pub: &[u8; EPH_LEN],
) -> Result<(AeadKey, [u8; NONCE_LEN]), AdmissionTicketError> {
    let mut info = Vec::with_capacity(HKDF_INFO_NONCE.len() + EPH_LEN);
    info.extend_from_slice(HKDF_INFO_KEY);
    info.extend_from_slice(eph_pub);
    let key = hkdf_sha256::<KEY_LEN>(shared, &[], &info).map_err(|_| AdmissionTicketError::Seal)?;
    info.clear();
    info.extend_from_slice(HKDF_INFO_NONCE);
    info.extend_from_slice(eph_pub);
    let nonce_secret =
        hkdf_sha256::<NONCE_LEN>(shared, &[], &info).map_err(|_| AdmissionTicketError::Seal)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_secret.as_bytes());
    Ok((key, nonce))
}

/// The admission-key version a ticket names, without opening it: what a
/// publisher reads to pick the seed. `None` when the ticket is not the right
/// shape.
#[must_use]
pub fn ticket_admission_version(ticket: &[u8]) -> Option<u16> {
    if ticket.len() != ADMISSION_TICKET_LEN || ticket.first() != Some(&ADMISSION_TICKET_VERSION_V1)
    {
        return None;
    }
    Some(u16::from_be_bytes([*ticket.get(1)?, *ticket.get(2)?]))
}

/// Open the admission ticket of the row `(channel_id, subscriber_pk)` with
/// the admission seed of the version the ticket names
/// ([`ticket_admission_version`]), and verify the invite key's signature
/// over the row. Returns the invite public key; the caller seals only when
/// it is one of the channel's live invite keys. A seed of another version
/// fails to open.
///
/// # Errors
///
/// Every [`AdmissionTicketError`] but `Seal`.
pub fn open_admission_ticket(
    admission_seed: &[u8; ADMISSION_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    ticket: &[u8],
) -> Result<[u8; INVITE_KEY_LEN], AdmissionTicketError> {
    let (&version, rest) = ticket
        .split_first()
        .ok_or(AdmissionTicketError::BadLength)?;
    if version != ADMISSION_TICKET_VERSION_V1 {
        return Err(AdmissionTicketError::UnknownVersion(version));
    }
    if ticket.len() != ADMISSION_TICKET_LEN {
        return Err(AdmissionTicketError::BadLength);
    }
    let (version_bytes, rest) = rest.split_at(2);
    let admission_version = u16::from_be_bytes([version_bytes[0], version_bytes[1]]);
    let (eph, ciphertext) = rest.split_at(EPH_LEN);
    let eph: [u8; EPH_LEN] = eph
        .try_into()
        .map_err(|_| AdmissionTicketError::BadLength)?;
    let eph_pub = X25519PublicKey::from_bytes(&eph).map_err(|_| AdmissionTicketError::BadKey)?;
    let shared = X25519SecretKey::from_bytes(*admission_seed).diffie_hellman(&eph_pub);
    let (key, nonce) = ticket_key_and_nonce(shared.as_bytes(), &eph)?;
    let mut plain = aead::open(
        ALG,
        &key,
        &nonce,
        &ticket_aad(admission_version, channel_id, subscriber_pk),
        ciphertext,
    )
    .map_err(|_| AdmissionTicketError::Open)?;
    let result = verify_ticket_plaintext(&plain, channel_id, subscriber_pk);
    plain.fill(0);
    result
}

fn verify_ticket_plaintext(
    plain: &[u8],
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<[u8; INVITE_KEY_LEN], AdmissionTicketError> {
    if plain.len() != TICKET_PLAIN_LEN {
        return Err(AdmissionTicketError::BadLength);
    }
    let (invite_key, signature) = plain.split_at(INVITE_KEY_LEN);
    let invite_key: [u8; INVITE_KEY_LEN] = invite_key
        .try_into()
        .map_err(|_| AdmissionTicketError::BadLength)?;
    let key = VerifyingKey::from_bytes(&invite_key).map_err(|_| AdmissionTicketError::BadKey)?;
    if key.is_weak() {
        return Err(AdmissionTicketError::BadKey);
    }
    let signature =
        Signature::from_bytes(signature).map_err(|_| AdmissionTicketError::BadSignature)?;
    key.verify(&ticket_signed_bytes(channel_id, subscriber_pk), &signature)
        .map_err(|_| AdmissionTicketError::BadSignature)?;
    Ok(invite_key)
}

/// Fuzz oracle behind `fuzz_api::channel_invite_proof_parse`: parse `value`
/// and verify it for a fixed row and clock. `None` when it does not parse;
/// `Some(true)` when what parsed re-encodes to the same text and verifying
/// it did not panic.
pub(crate) fn fuzz_parse_proof(value: &str) -> Option<bool> {
    let proof = InviteProof::parse(value).ok()?;
    let _ = verify_invite_proof(
        &proof,
        &[0x0C; CHANNEL_ID_LEN],
        &[0x5B; SUBSCRIBER_PK_LEN],
        1_700_000_000_000,
    );
    Some(proof.encode() == value)
}

/// Fuzz oracle behind `fuzz_api::admission_ticket_open`: open `ticket` under
/// a fixed admission seed, and feed it to the plaintext check directly,
/// which a fuzzer cannot reach through the AEAD. Returns whether either
/// accepted (a fuzzer cannot forge a signature, so `false` is expected).
pub(crate) fn fuzz_open_ticket(ticket: &[u8]) -> bool {
    let channel = [0x0C; CHANNEL_ID_LEN];
    let subscriber = [0x5B; SUBSCRIBER_PK_LEN];
    let opened = open_admission_ticket(&[0x41; 32], &channel, &subscriber, ticket).is_ok();
    let parsed = verify_ticket_plaintext(ticket, &channel, &subscriber).is_ok();
    opened || parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const SUBSCRIBER: [u8; SUBSCRIBER_PK_LEN] = [0x5B; SUBSCRIBER_PK_LEN];
    const NOW: u64 = 1_700_000_000_000;

    fn signer() -> ChannelInviteSigner {
        ChannelInviteSigner::from_invite_seed(&[0x19; INVITE_KEY_LEN])
    }

    #[test]
    fn a_proof_round_trips_through_its_header_and_verifies() {
        let proof = signer().sign_proof_at(&CHANNEL, &SUBSCRIBER, NOW, [7; 16]);
        let header = proof.encode();
        assert!(header.len() <= INVITE_PROOF_MAX_HEADER_LEN);
        let parsed = InviteProof::parse(&header).unwrap();
        assert_eq!(parsed, proof);
        assert_eq!(fuzz_parse_proof(&header), Some(true));
        assert_eq!(parsed.invite_key, signer().verifying_key().to_bytes());
        assert_eq!(
            verify_invite_proof(&parsed, &CHANNEL, &SUBSCRIBER, NOW + 1_000),
            Ok(())
        );
    }

    #[test]
    fn a_proof_for_another_row_or_outside_the_window_is_refused() {
        let proof = signer().sign_proof_at(&CHANNEL, &SUBSCRIBER, NOW, [7; 16]);
        assert_eq!(
            verify_invite_proof(&proof, &[0x0D; CHANNEL_ID_LEN], &SUBSCRIBER, NOW),
            Err(OwnerProofError::BadSignature)
        );
        assert_eq!(
            verify_invite_proof(&proof, &CHANNEL, &[0x5C; SUBSCRIBER_PK_LEN], NOW),
            Err(OwnerProofError::BadSignature)
        );
        assert_eq!(
            verify_invite_proof(
                &proof,
                &CHANNEL,
                &SUBSCRIBER,
                NOW + OWNER_PROOF_WINDOW_MS + 1
            ),
            Err(OwnerProofError::OutsideWindow)
        );
        let mut other_key = proof;
        other_key.invite_key = ChannelInviteSigner::from_invite_seed(&[0x20; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_invite_proof(&other_key, &CHANNEL, &SUBSCRIBER, NOW),
            Err(OwnerProofError::BadSignature)
        );
    }

    #[test]
    fn a_malformed_header_is_refused() {
        let good = signer()
            .sign_proof_at(&CHANNEL, &SUBSCRIBER, NOW, [7; 16])
            .encode();
        assert_eq!(
            InviteProof::parse(&good.replacen("v1", "v2", 1)),
            Err(OwnerProofError::UnknownVersion)
        );
        for bad in [
            String::new(),
            "v1".to_string(),
            format!("{good}.x"),
            good.replacen('.', "..", 1),
            "x".repeat(INVITE_PROOF_MAX_HEADER_LEN + 1),
        ] {
            assert!(InviteProof::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_ticket_opens_to_the_invite_key_for_its_row_only() {
        let admission = [3; ADMISSION_SEED_LEN];
        let ticket = signer()
            .seal_ticket(&admission_public_key(&admission), 4, &CHANNEL, &SUBSCRIBER)
            .unwrap();
        assert_eq!(ticket.len(), ADMISSION_TICKET_LEN);
        assert_eq!(ADMISSION_TICKET_LEN, 147);
        assert_eq!(ticket_admission_version(&ticket), Some(4));
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &SUBSCRIBER, &ticket),
            Ok(signer().verifying_key().to_bytes())
        );
        // Copied onto another row, opened with another channel's key, or
        // tampered: refused.
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &[0x5C; SUBSCRIBER_PK_LEN], &ticket),
            Err(AdmissionTicketError::Open)
        );
        let other = generate_admission_seed();
        assert_eq!(
            open_admission_ticket(&other, &CHANNEL, &SUBSCRIBER, &ticket),
            Err(AdmissionTicketError::Open)
        );
        // The version is bound: rewriting it fails the AEAD.
        let mut moved = ticket;
        moved[2] = 5;
        assert_eq!(ticket_admission_version(&moved), Some(5));
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &SUBSCRIBER, &moved),
            Err(AdmissionTicketError::Open)
        );
        let mut tampered = ticket;
        tampered[ADMISSION_TICKET_LEN - 1] ^= 1;
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &SUBSCRIBER, &tampered),
            Err(AdmissionTicketError::Open)
        );
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &SUBSCRIBER, &ticket[..10]),
            Err(AdmissionTicketError::BadLength)
        );
        let mut v2 = ticket;
        v2[0] = 2;
        assert_eq!(
            open_admission_ticket(&admission, &CHANNEL, &SUBSCRIBER, &v2),
            Err(AdmissionTicketError::UnknownVersion(2))
        );
        assert!(!fuzz_open_ticket(&ticket));
    }

    #[test]
    fn a_ticket_whose_signature_names_another_row_is_refused() {
        // A ticket sealed for the right row whose inner signature covers
        // another subscriber.
        let s = signer();
        let mut plain = Vec::new();
        plain.extend_from_slice(&s.verifying_key().to_bytes());
        plain.extend_from_slice(
            &s.key
                .sign(&ticket_signed_bytes(&CHANNEL, &[0x5C; SUBSCRIBER_PK_LEN]))
                .to_bytes(),
        );
        assert_eq!(
            verify_ticket_plaintext(&plain, &CHANNEL, &SUBSCRIBER),
            Err(AdmissionTicketError::BadSignature)
        );
    }

    #[test]
    fn derivations_are_distinct_and_stable() {
        let invite = invite_signing_seed(&[0x19; 32]);
        assert_eq!(invite, invite_signing_seed(&[0x19; 32]));
        assert_ne!(invite, invite_signing_seed(&[0x1A; 32]));
        assert_ne!(generate_admission_seed(), generate_admission_seed());
    }
}
