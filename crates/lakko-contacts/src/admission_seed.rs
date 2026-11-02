// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Admission-seed envelope ([ADR-0087] section 7, maintainer decision of
//! 2026-11-02): a private channel's owner hands one co-publisher the seed of
//! one version of the channel's **admission key**, so that co-publisher can
//! open the admission tickets subscribers seal (`lakko_channels::invite`)
//! and let them in.
//!
//! ## The key and its versions
//!
//! The admission key is a random 32-byte X25519 seed with a `u16` version,
//! minted by the owner. A subscriber invitation names the public key and the
//! version (`crate::group_invite`, preview kind 3); a ticket names the
//! version. When a co-publisher leaves or is removed the owner mints
//! version + 1 and sends it to the remaining co-publishers only; new links
//! carry it. Everyone keeps older versions so tickets from older links still
//! open (accepted: a removed co-publisher can still open tickets from old
//! links, which tells it which link a subscriber came by, nothing more).
//!
//! ## Why the seed is sealed
//!
//! The envelope rides an MLS application message on the channel's
//! PublisherGroup journal, which every co-publisher reads. Sealed to the
//! addressee's account X25519 key (the [`crate::envelope`] sealed box, the
//! clear header as AAD), a removed co-publisher's leaf and any later member
//! cannot read a version not addressed to them. The shape is
//! [`crate::knock_seed`]'s with a channel id in place of a journal id.
//!
//! ## Wire format (v1)
//!
//! ```text
//! ADMISSION_SEED_ENVELOPE_DOMAIN_V1 ("lakko/channel-admission-seed/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ member_account_key(32)
//!   ‖ admission_version(2, BE) ‖ admission_public_key(32) ‖ sealed(80)
//! sealed = eph_pub(32) ‖ ChaCha20-Poly1305(seed(32)) ‖ tag(16), AAD = every byte before it
//! ```
//!
//! Fixed length [`ADMISSION_SEED_ENVELOPE_LEN`] (195). [`compose_admission_seed`]
//! derives the clear public key from the seed it seals, and
//! [`AdmissionSeedEnvelope::open`] refuses a seed that does not derive it.
//!
//! ## The receiver's authority rule
//!
//! A device keeps the envelope only when all of these hold, judged at
//! receipt (C-68: the MLS sender resolved to its account key through the
//! roster of the epoch the message was sealed at):
//!
//! 1. it arrived on the PublisherGroup journal of the channel `channel_id`
//!    names, and this device publishes that channel;
//! 2. `member_account_key` is this device's own account key;
//! 3. the sender is the channel's **owner**: the account key that issued the
//!    co-publisher link this device joined by (a PublisherGroup has no role
//!    table, so the link's issuer is what names the owner);
//! 4. the seed opens and derives `admission_public_key`.
//!
//! Anything else is dropped, never shown. A seed from anyone else at worst
//! opens nothing: tickets are sealed to the public key the links name.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{self, EPH_PUB_LEN, TAG_LEN};

/// 32-byte domain separator prepended to the envelope. Frozen: the
/// dispatcher matches on it.
pub const ADMISSION_SEED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/channel-admission-seed/v1\x00";

/// Schema-version byte carried first after the separator.
pub const ADMISSION_SEED_VERSION_V1: u8 = 0x01;

/// Length of an admission seed and of its public key.
pub const ADMISSION_KEY_LEN: usize = 32;

/// Length of a channel id.
pub const ADMISSION_CHANNEL_ID_LEN: usize = 16;

/// The sealed seed: ephemeral key, ciphertext of the 32-byte seed, tag.
pub const ADMISSION_SEED_SEALED_LEN: usize = EPH_PUB_LEN + ADMISSION_KEY_LEN + TAG_LEN;

/// Bytes after the separator: version, channel id, account key, admission
/// version, admission public key, sealed.
pub const ADMISSION_SEED_PAYLOAD_LEN: usize =
    1 + ADMISSION_CHANNEL_ID_LEN + 32 + 2 + ADMISSION_KEY_LEN + ADMISSION_SEED_SEALED_LEN;

/// The one well-formed length.
pub const ADMISSION_SEED_ENVELOPE_LEN: usize =
    ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len() + ADMISSION_SEED_PAYLOAD_LEN;

const CHANNEL_AT: usize = 1;
const MEMBER_AT: usize = CHANNEL_AT + ADMISSION_CHANNEL_ID_LEN;
const VERSION_AT: usize = MEMBER_AT + 32;
const PUBLIC_KEY_AT: usize = VERSION_AT + 2;
const SEALED_AT: usize = PUBLIC_KEY_AT + ADMISSION_KEY_LEN;

/// The X25519 public key of an admission seed (equal to
/// `lakko_channels::admission_public_key`).
#[must_use]
pub fn admission_seed_public_key(seed: &[u8; ADMISSION_KEY_LEN]) -> [u8; ADMISSION_KEY_LEN] {
    X25519SecretKey::from_bytes(*seed).public_key().to_bytes()
}

/// A decoded admission-seed envelope. The seed is still sealed:
/// [`Self::open`] with the addressee's account X25519 secret recovers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionSeedEnvelope {
    /// The channel the admission key belongs to.
    pub channel_id: [u8; ADMISSION_CHANNEL_ID_LEN],
    /// The addressee's account Ed25519 public key.
    pub member_account_key: [u8; 32],
    /// The admission-key version the seed is for.
    pub admission_version: u16,
    /// `admission_seed_public_key(seed)`, readable without opening.
    pub admission_public_key: [u8; ADMISSION_KEY_LEN],
    /// The sealed 32-byte seed.
    pub sealed: [u8; ADMISSION_SEED_SEALED_LEN],
}

/// Why an admission-seed envelope was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AdmissionSeedError {
    /// No domain separator. Only from [`decode_admission_seed`].
    #[error("plaintext does not carry the admission-seed domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("admission-seed envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`ADMISSION_SEED_VERSION_V1`].
    #[error("admission-seed envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// Sealing the seed failed.
    #[error("admission seed could not be sealed")]
    Seal,
    /// The sealed seed did not open under the given key.
    #[error("admission seed did not open")]
    Open,
    /// The seed opened but does not derive the header's public key.
    #[error("admission seed does not derive the envelope's public key")]
    PublicKeyMismatch,
}

impl AdmissionSeedError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::Seal => "seal",
            Self::Open => "open",
            Self::PublicKeyMismatch => "publicKeyMismatch",
        }
    }
}

fn header(
    channel_id: &[u8; ADMISSION_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
    admission_version: u16,
    admission_public_key: &[u8; ADMISSION_KEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ADMISSION_SEED_ENVELOPE_LEN);
    out.extend_from_slice(ADMISSION_SEED_ENVELOPE_DOMAIN_V1);
    out.push(ADMISSION_SEED_VERSION_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(member_account_key);
    out.extend_from_slice(&admission_version.to_be_bytes());
    out.extend_from_slice(admission_public_key);
    out
}

/// Compose the envelope handed to the MLS application-message send path on
/// the channel's PublisherGroup journal. `member_x25519_pub` is the
/// co-publisher account's X25519 public key (its identity bundle's
/// `x25519_pubkey`).
///
/// # Errors
///
/// [`AdmissionSeedError::Seal`] if the sealed box fails (not expected).
pub fn compose_admission_seed(
    channel_id: &[u8; ADMISSION_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
    member_x25519_pub: &X25519PublicKey,
    admission_version: u16,
    seed: &[u8; ADMISSION_KEY_LEN],
) -> CoreResult<Vec<u8>, AdmissionSeedError> {
    let public_key = admission_seed_public_key(seed);
    let mut out = header(
        channel_id,
        member_account_key,
        admission_version,
        &public_key,
    );
    let sealed =
        envelope::seal(member_x25519_pub, &out, seed).map_err(|_| AdmissionSeedError::Seal)?;
    if sealed.len() != ADMISSION_SEED_SEALED_LEN {
        return Err(AdmissionSeedError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn array_at<const N: usize>(payload: &[u8], at: usize) -> CoreResult<[u8; N], AdmissionSeedError> {
    payload
        .get(at..at + N)
        .and_then(|s| s.try_into().ok())
        .ok_or(AdmissionSeedError::BadLength)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`AdmissionSeedError::UnknownVersion`] (checked before the length) and
/// [`AdmissionSeedError::BadLength`].
pub fn try_unwrap_admission_seed(
    plaintext: &[u8],
) -> CoreResult<Option<AdmissionSeedEnvelope>, AdmissionSeedError> {
    let Some(payload) = plaintext.strip_prefix(ADMISSION_SEED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload.split_first().ok_or(AdmissionSeedError::BadLength)?;
    if version != ADMISSION_SEED_VERSION_V1 {
        return Err(AdmissionSeedError::UnknownVersion(version));
    }
    if payload.len() != ADMISSION_SEED_PAYLOAD_LEN {
        return Err(AdmissionSeedError::BadLength);
    }
    let admission_version: [u8; 2] = array_at(payload, VERSION_AT)?;
    Ok(Some(AdmissionSeedEnvelope {
        channel_id: array_at(payload, CHANNEL_AT)?,
        member_account_key: array_at(payload, MEMBER_AT)?,
        admission_version: u16::from_be_bytes(admission_version),
        admission_public_key: array_at(payload, PUBLIC_KEY_AT)?,
        sealed: array_at(payload, SEALED_AT)?,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`AdmissionSeedError::WrongDomain`] and every error of
/// [`try_unwrap_admission_seed`].
pub fn decode_admission_seed(
    bytes: &[u8],
) -> CoreResult<AdmissionSeedEnvelope, AdmissionSeedError> {
    try_unwrap_admission_seed(bytes)?.ok_or(AdmissionSeedError::WrongDomain)
}

impl AdmissionSeedEnvelope {
    /// Whether this envelope is addressed to `account_key`.
    #[must_use]
    pub fn is_addressed_to(&self, account_key: &[u8; 32]) -> bool {
        self.member_account_key == *account_key
    }

    /// The exact wire bytes (the encoding is canonical: every field is fixed
    /// width).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = header(
            &self.channel_id,
            &self.member_account_key,
            self.admission_version,
            &self.admission_public_key,
        );
        out.extend_from_slice(&self.sealed);
        out
    }

    /// Open the seed with the addressee account's X25519 secret. The header
    /// is the AAD, so another channel, version, addressee or public key
    /// fails to open; the seed must then derive
    /// [`Self::admission_public_key`].
    ///
    /// # Errors
    ///
    /// [`AdmissionSeedError::Open`], [`AdmissionSeedError::PublicKeyMismatch`].
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<[u8; ADMISSION_KEY_LEN], AdmissionSeedError> {
        let aad = header(
            &self.channel_id,
            &self.member_account_key,
            self.admission_version,
            &self.admission_public_key,
        );
        let mut plain = envelope::open(account_x25519_secret, &aad, &self.sealed)
            .map_err(|_| AdmissionSeedError::Open)?;
        let seed: Option<[u8; ADMISSION_KEY_LEN]> = plain.as_slice().try_into().ok();
        plain.fill(0);
        let seed = seed.ok_or(AdmissionSeedError::Open)?;
        if admission_seed_public_key(&seed) != self.admission_public_key {
            return Err(AdmissionSeedError::PublicKeyMismatch);
        }
        Ok(seed)
    }
}

/// The fuzz oracle behind `fuzz_api::admission_seed_decode`: `None` when the
/// decoder refuses; `Some(true)` when what decoded re-encodes to the same
/// bytes and opening it under a fixed key never panicked.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let env = decode_admission_seed(bytes).ok()?;
    let _ = env.open(&X25519SecretKey::from_bytes([0x33; 32]));
    Some(env.to_bytes() == bytes && bytes.len() == ADMISSION_SEED_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; ADMISSION_CHANNEL_ID_LEN] = [0x0C; ADMISSION_CHANNEL_ID_LEN];
    const MEMBER: [u8; 32] = [0x4B; 32];
    const SEED: [u8; 32] = [0x2D; 32];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn wire() -> Vec<u8> {
        let (_, public) = member_keys();
        compose_admission_seed(&CHANNEL, &MEMBER, &public, 7, &SEED).unwrap()
    }

    #[test]
    fn compose_decode_open_round_trips_at_a_fixed_length() {
        let (secret, _) = member_keys();
        let bytes = wire();
        assert_eq!(bytes.len(), ADMISSION_SEED_ENVELOPE_LEN);
        assert_eq!(ADMISSION_SEED_ENVELOPE_LEN, 32 + 163);
        let env = decode_admission_seed(&bytes).unwrap();
        assert_eq!(env.channel_id, CHANNEL);
        assert_eq!(env.admission_version, 7);
        assert_eq!(env.admission_public_key, admission_seed_public_key(&SEED));
        assert!(env.is_addressed_to(&MEMBER));
        assert_eq!(env.open(&secret).unwrap(), SEED);
        assert_eq!(env.to_bytes(), bytes);
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn the_seed_is_not_in_the_clear_and_only_the_addressee_opens_it() {
        let bytes = wire();
        assert!(!bytes.windows(32).any(|w| w == SEED));
        let env = decode_admission_seed(&bytes).unwrap();
        assert_eq!(
            env.open(&X25519SecretKey::from_bytes([0x44; 32])),
            Err(AdmissionSeedError::Open)
        );
    }

    #[test]
    fn the_clear_header_is_bound_to_the_seal() {
        let (secret, _) = member_keys();
        let good = wire();
        let domain = ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len();
        for at in [
            domain + CHANNEL_AT,
            domain + MEMBER_AT,
            domain + VERSION_AT + 1,
            domain + PUBLIC_KEY_AT,
        ] {
            let mut tampered = good.clone();
            tampered[at] ^= 0x01;
            let env = decode_admission_seed(&tampered).unwrap();
            assert_eq!(
                env.open(&secret),
                Err(AdmissionSeedError::Open),
                "byte {at}"
            );
        }
    }

    #[test]
    fn a_seed_that_does_not_derive_the_header_key_is_refused() {
        let (secret, public) = member_keys();
        let wrong = admission_seed_public_key(&[0x77; 32]);
        let mut bytes = header(&CHANNEL, &MEMBER, 7, &wrong);
        let sealed = envelope::seal(&public, &bytes, &SEED).unwrap();
        bytes.extend_from_slice(&sealed);
        let env = decode_admission_seed(&bytes).unwrap();
        assert_eq!(
            env.open(&secret),
            Err(AdmissionSeedError::PublicKeyMismatch)
        );
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = wire();
        assert_eq!(
            try_unwrap_admission_seed(&good[..good.len() - 1]),
            Err(AdmissionSeedError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_admission_seed(&long),
            Err(AdmissionSeedError::BadLength)
        );
        let mut v2 = good;
        v2[ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert_eq!(
            try_unwrap_admission_seed(&v2),
            Err(AdmissionSeedError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_admission_seed(ADMISSION_SEED_ENVELOPE_DOMAIN_V1),
            Err(AdmissionSeedError::BadLength)
        );
        assert_eq!(try_unwrap_admission_seed(b"hello"), Ok(None));
        assert_eq!(
            decode_admission_seed(b"hello"),
            Err(AdmissionSeedError::WrongDomain)
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains_and_dispatched() {
        for d in [
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(
                !d.starts_with(ADMISSION_SEED_ENVELOPE_DOMAIN_V1)
                    && !ADMISSION_SEED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire()),
            Ok(crate::incoming_envelope::IncomingEnvelope::AdmissionSeed(_))
        ));
    }
}
