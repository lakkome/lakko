// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Knock-seed envelope ([ADR-0087] section 5): an owner or admin hands
//! another admin the seed of one version of the group's knock key, so that
//! admin can open the sealed knock rows (`crate::knock`) and answer requests
//! to join.
//!
//! ## Why it exists
//!
//! `knock_seed(v)` is derived from the ADR-0057 journal root seed, which only
//! the owner holds. An admin who must read the request queue therefore gets
//! each version's seed in the group, the way an admin gets its registrar seed
//! (`crate::registrar_seed`, the shape this envelope copies). Removing an
//! admin bumps `v`; the new version's seed goes to the remaining admins only.
//! Older versions stay derivable, so older links still open: an admin keeps
//! every version it was handed.
//!
//! ## Why the seed is sealed
//!
//! The envelope rides an MLS application message, so every member reads it.
//! In the clear it would hand the knock seed (and with it every requester's
//! KeyPackage, reply grant and profile) to every member. So the seed is
//! **sealed to the addressee's account X25519 key** with the
//! [`crate::envelope`] sealed box, the clear header as AAD. Other members see
//! whom it is for, which version, and the knock **public** key, which every
//! link already carries.
//!
//! ## Wire format (v1)
//!
//! ```text
//! KNOCK_SEED_ENVELOPE_DOMAIN_V1 ("lakko/group-knock-seed/v1\0")
//!   ‖ version(1)=0x01 ‖ journal_id(32) ‖ member_account_key(32)
//!   ‖ knock_version(2, BE) ‖ knock_public_key(32) ‖ sealed(80)
//! sealed = eph_pub(32) ‖ ChaCha20-Poly1305(seed(32)) ‖ tag(16), AAD = every byte before it
//! ```
//!
//! Fixed length [`KNOCK_SEED_ENVELOPE_LEN`] (205). [`compose_knock_seed`]
//! derives the clear public key from the seed it seals, and
//! [`KnockSeedEnvelope::open`] refuses a seed that does not derive it.
//!
//! ## The receiver's authority rule
//!
//! A device keeps the envelope only when all of these hold, judged at
//! receipt (C-68: the MLS sender resolved to its account key through the
//! roster of the epoch the message was sealed at):
//!
//! 1. `member_account_key` is this device's own account key;
//! 2. `journal_id` is the group the message arrived in;
//! 3. the sender is the **owner or an admin** of the live role table (an
//!    admin may forward a version it holds, so a newly promoted admin is not
//!    stuck waiting for an owner who is offline);
//! 4. the receiving account is itself the owner or an admin of that table
//!    (a plain member has no use for the seed and must not keep one);
//! 5. the seed opens and derives `knock_public_key`.
//!
//! Anything else is dropped, never shown. A seed from anyone else at worst
//! opens nothing: knock rows are sealed to the public key the links name.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};
use crate::knock::{knock_public_key, KNOCK_KEY_LEN};

/// The sealed-box labels of this envelope: its own, so the box opens as no
/// other sealed-to-account envelope and no contact request.
pub(crate) const SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/sealed/knock-seed/v1/key",
    nonce: b"lakko/sealed/knock-seed/v1/nonce",
};

/// 26-byte domain separator prepended to the envelope. Frozen: the
/// dispatcher matches on it.
pub const KNOCK_SEED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-knock-seed/v1\x00";

/// Schema-version byte carried first after the separator.
pub const KNOCK_SEED_VERSION_V1: u8 = 0x01;

/// The sealed seed: ephemeral key, ciphertext of the 32-byte seed, tag.
pub const KNOCK_SEED_SEALED_LEN: usize = EPH_PUB_LEN + KNOCK_KEY_LEN + TAG_LEN;

/// Bytes after the separator: version, journal id, account key, knock
/// version, knock public key, sealed.
pub const KNOCK_SEED_PAYLOAD_LEN: usize = 1 + 32 + 32 + 2 + KNOCK_KEY_LEN + KNOCK_SEED_SEALED_LEN;

/// The one well-formed length.
pub const KNOCK_SEED_ENVELOPE_LEN: usize =
    KNOCK_SEED_ENVELOPE_DOMAIN_V1.len() + KNOCK_SEED_PAYLOAD_LEN;

const JOURNAL_AT: usize = 1;
const MEMBER_AT: usize = JOURNAL_AT + 32;
const VERSION_AT: usize = MEMBER_AT + 32;
const PUBLIC_KEY_AT: usize = VERSION_AT + 2;
const SEALED_AT: usize = PUBLIC_KEY_AT + KNOCK_KEY_LEN;

/// A decoded knock-seed envelope. The seed is still sealed: [`Self::open`]
/// with the addressee's account X25519 secret recovers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnockSeedEnvelope {
    /// The group (journal id) the knock key belongs to.
    pub journal_id: [u8; 32],
    /// The addressee's account Ed25519 public key.
    pub member_account_key: [u8; 32],
    /// The knock-key version the seed is for.
    pub knock_version: u16,
    /// `knock_public_key(seed)`, readable without opening.
    pub knock_public_key: [u8; KNOCK_KEY_LEN],
    /// The sealed 32-byte seed.
    pub sealed: [u8; KNOCK_SEED_SEALED_LEN],
}

/// Why a knock-seed envelope was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KnockSeedError {
    /// No domain separator. Only from [`decode_knock_seed`].
    #[error("plaintext does not carry the knock-seed domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("knock-seed envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`KNOCK_SEED_VERSION_V1`].
    #[error("knock-seed envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// Sealing the seed failed.
    #[error("knock seed could not be sealed")]
    Seal,
    /// The sealed seed did not open under the given key.
    #[error("knock seed did not open")]
    Open,
    /// The seed opened but does not derive the header's public key.
    #[error("knock seed does not derive the envelope's knock public key")]
    PublicKeyMismatch,
}

impl KnockSeedError {
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
    journal_id: &[u8; 32],
    member_account_key: &[u8; 32],
    knock_version: u16,
    knock_public_key: &[u8; KNOCK_KEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(KNOCK_SEED_ENVELOPE_LEN);
    out.extend_from_slice(KNOCK_SEED_ENVELOPE_DOMAIN_V1);
    out.push(KNOCK_SEED_VERSION_V1);
    out.extend_from_slice(journal_id);
    out.extend_from_slice(member_account_key);
    out.extend_from_slice(&knock_version.to_be_bytes());
    out.extend_from_slice(knock_public_key);
    out
}

/// Compose the envelope handed to the MLS application-message send path on
/// the group's journal. `member_x25519_pub` is the addressee account's X25519
/// public key (its identity bundle's `x25519_pubkey`).
///
/// # Errors
///
/// [`KnockSeedError::Seal`] if the sealed box fails (not expected).
pub fn compose_knock_seed(
    journal_id: &[u8; 32],
    member_account_key: &[u8; 32],
    member_x25519_pub: &X25519PublicKey,
    knock_version: u16,
    seed: &[u8; KNOCK_KEY_LEN],
) -> CoreResult<Vec<u8>, KnockSeedError> {
    let public_key = knock_public_key(seed);
    let mut out = header(journal_id, member_account_key, knock_version, &public_key);
    let sealed = envelope::seal_labeled(&SEAL_LABELS, member_x25519_pub, &out, seed)
        .map_err(|_| KnockSeedError::Seal)?;
    if sealed.len() != KNOCK_SEED_SEALED_LEN {
        return Err(KnockSeedError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn array_at<const N: usize>(payload: &[u8], at: usize) -> CoreResult<[u8; N], KnockSeedError> {
    payload
        .get(at..at + N)
        .and_then(|s| s.try_into().ok())
        .ok_or(KnockSeedError::BadLength)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`KnockSeedError::UnknownVersion`] (checked before the length) and
/// [`KnockSeedError::BadLength`].
pub fn try_unwrap_knock_seed(
    plaintext: &[u8],
) -> CoreResult<Option<KnockSeedEnvelope>, KnockSeedError> {
    let Some(payload) = plaintext.strip_prefix(KNOCK_SEED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload.split_first().ok_or(KnockSeedError::BadLength)?;
    if version != KNOCK_SEED_VERSION_V1 {
        return Err(KnockSeedError::UnknownVersion(version));
    }
    if payload.len() != KNOCK_SEED_PAYLOAD_LEN {
        return Err(KnockSeedError::BadLength);
    }
    let knock_version: [u8; 2] = array_at(payload, VERSION_AT)?;
    Ok(Some(KnockSeedEnvelope {
        journal_id: array_at(payload, JOURNAL_AT)?,
        member_account_key: array_at(payload, MEMBER_AT)?,
        knock_version: u16::from_be_bytes(knock_version),
        knock_public_key: array_at(payload, PUBLIC_KEY_AT)?,
        sealed: array_at(payload, SEALED_AT)?,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`KnockSeedError::WrongDomain`] and every error of
/// [`try_unwrap_knock_seed`].
pub fn decode_knock_seed(bytes: &[u8]) -> CoreResult<KnockSeedEnvelope, KnockSeedError> {
    try_unwrap_knock_seed(bytes)?.ok_or(KnockSeedError::WrongDomain)
}

impl KnockSeedEnvelope {
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
            &self.journal_id,
            &self.member_account_key,
            self.knock_version,
            &self.knock_public_key,
        );
        out.extend_from_slice(&self.sealed);
        out
    }

    /// Open the seed with the addressee account's X25519 secret. The header
    /// is the AAD, so another journal, version, addressee or public key fails
    /// to open; the seed must then derive [`Self::knock_public_key`].
    ///
    /// # Errors
    ///
    /// [`KnockSeedError::Open`], [`KnockSeedError::PublicKeyMismatch`].
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<[u8; KNOCK_KEY_LEN], KnockSeedError> {
        let aad = header(
            &self.journal_id,
            &self.member_account_key,
            self.knock_version,
            &self.knock_public_key,
        );
        let mut plain =
            envelope::open_labeled(&SEAL_LABELS, account_x25519_secret, &aad, &self.sealed)
                .map_err(|_| KnockSeedError::Open)?;
        let seed: Option<[u8; KNOCK_KEY_LEN]> = plain.as_slice().try_into().ok();
        plain.fill(0);
        let seed = seed.ok_or(KnockSeedError::Open)?;
        if knock_public_key(&seed) != self.knock_public_key {
            return Err(KnockSeedError::PublicKeyMismatch);
        }
        Ok(seed)
    }
}

/// The fuzz oracle behind `fuzz_api::knock_seed_decode`: `None` when the
/// decoder refuses; `Some(true)` when what decoded re-encodes to the same
/// bytes and opening it under a fixed key never panicked.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let env = decode_knock_seed(bytes).ok()?;
    let _ = env.open(&X25519SecretKey::from_bytes([0x33; 32]));
    Some(env.to_bytes() == bytes && bytes.len() == KNOCK_SEED_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knock::knock_seed;

    const JOURNAL: [u8; 32] = [0x4A; 32];
    const MEMBER: [u8; 32] = [0x4B; 32];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn seed() -> [u8; 32] {
        knock_seed(&[0x11; 32], 4)
    }

    fn wire() -> Vec<u8> {
        let (_, public) = member_keys();
        compose_knock_seed(&JOURNAL, &MEMBER, &public, 4, &seed()).unwrap()
    }

    #[test]
    fn compose_decode_open_round_trips_at_a_fixed_length() {
        let (secret, _) = member_keys();
        let bytes = wire();
        assert_eq!(bytes.len(), KNOCK_SEED_ENVELOPE_LEN);
        assert_eq!(KNOCK_SEED_ENVELOPE_LEN, 26 + 179);
        let env = decode_knock_seed(&bytes).unwrap();
        assert_eq!(env.journal_id, JOURNAL);
        assert_eq!(env.knock_version, 4);
        assert_eq!(env.knock_public_key, knock_public_key(&seed()));
        assert!(env.is_addressed_to(&MEMBER));
        assert_eq!(env.open(&secret).unwrap(), seed());
        assert_eq!(env.to_bytes(), bytes);
        assert_eq!(crate::knock_seed::fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn the_seed_is_not_in_the_clear_and_only_the_addressee_opens_it() {
        let bytes = wire();
        assert!(!bytes.windows(32).any(|w| w == seed()));
        let env = decode_knock_seed(&bytes).unwrap();
        assert_eq!(
            env.open(&X25519SecretKey::from_bytes([0x44; 32])),
            Err(KnockSeedError::Open)
        );
    }

    #[test]
    fn the_clear_header_is_bound_to_the_seal() {
        let (secret, _) = member_keys();
        let good = wire();
        let domain = KNOCK_SEED_ENVELOPE_DOMAIN_V1.len();
        for at in [
            domain + JOURNAL_AT,
            domain + MEMBER_AT,
            domain + VERSION_AT + 1,
            domain + PUBLIC_KEY_AT,
        ] {
            let mut tampered = good.clone();
            tampered[at] ^= 0x01;
            let env = decode_knock_seed(&tampered).unwrap();
            assert_eq!(env.open(&secret), Err(KnockSeedError::Open), "byte {at}");
        }
    }

    #[test]
    fn a_seed_that_does_not_derive_the_header_key_is_refused() {
        let (secret, public) = member_keys();
        let wrong = knock_public_key(&[0x77; 32]);
        let mut bytes = header(&JOURNAL, &MEMBER, 4, &wrong);
        let sealed = envelope::seal_labeled(&SEAL_LABELS, &public, &bytes, &seed()).unwrap();
        bytes.extend_from_slice(&sealed);
        let env = decode_knock_seed(&bytes).unwrap();
        assert_eq!(env.open(&secret), Err(KnockSeedError::PublicKeyMismatch));
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = wire();
        assert_eq!(
            try_unwrap_knock_seed(&good[..good.len() - 1]),
            Err(KnockSeedError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(try_unwrap_knock_seed(&long), Err(KnockSeedError::BadLength));
        let mut v2 = good;
        v2[KNOCK_SEED_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert_eq!(
            try_unwrap_knock_seed(&v2),
            Err(KnockSeedError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_knock_seed(KNOCK_SEED_ENVELOPE_DOMAIN_V1),
            Err(KnockSeedError::BadLength)
        );
        assert_eq!(try_unwrap_knock_seed(b"hello"), Ok(None));
        assert_eq!(
            decode_knock_seed(b"hello"),
            Err(KnockSeedError::WrongDomain)
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        for d in [
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(
                !d.starts_with(KNOCK_SEED_ENVELOPE_DOMAIN_V1)
                    && !KNOCK_SEED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire()),
            Ok(crate::incoming_envelope::IncomingEnvelope::KnockSeed(_))
        ));
    }
}
