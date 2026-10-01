// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Registrar-seed envelope (ADR-0057): a journal's root hands one member the
//! seed of the registrar key it appointed for that member.
//!
//! ## Why this exists
//!
//! Under ADR-0057 only the root of a group journal and the registrars it
//! appoints register the journal's epoch keys on the Delivery Service. The
//! root derives each registrar seed itself
//! (`lakko_ds_proof::journal_registrar_seed(root_seed, member_account_key)`),
//! registers the public key with a rank, and sends the seed to the member in
//! the group - no round trip from the member.
//!
//! ## Why the seed is sealed, not just sent in the group
//!
//! The envelope rides an MLS application message, so every member of the
//! epoch reads it. A registrar seed in the clear there would hand every
//! member every other member's registrar key - an admin's included - and the
//! ranks would mean nothing. So the seed is **sealed to the addressee's
//! account X25519 key** (the `x25519_pubkey` its identity bundle publishes;
//! every device of that account derives the secret half from the account
//! seed) with the [`crate::envelope`] sealed box, and the clear header is the
//! box's AAD. Other members see only whom it is for, at which rank, and the
//! registrar **public** key.
//!
//! ## Why the registrar public key is in the clear (v2, ADR-0057 amended
//! 2026-10-01)
//!
//! Whoever commits a removal must revoke every registrar key of the removed
//! account in the same registration, so every member needs the mapping
//! account -> registrar public key. The key is public anyway (the Delivery
//! Service holds it), so the envelope carries it in its clear header and
//! every member collects the mapping from the envelopes it reads. The
//! composer cannot send a mismatched one: [`compose_registrar_seed`] derives
//! it from the seed it seals. The header (key included) is the sealed box's
//! AAD, so the key cannot be swapped in transit, and the addressee's
//! [`RegistrarSeedEnvelope::open`] refuses a seed that does not derive the
//! header's key ([`RegistrarSeedError::PublicKeyMismatch`]).
//!
//! ## Wire format (v2)
//!
//! ```text
//! REGISTRAR_SEED_ENVELOPE_DOMAIN_V1 || version(1)=0x02 || journal_id(32)
//!   || member_account_key(32) || rank(1) || registrar_public_key(32)
//!   || sealed(80)
//! ```
//!
//! `registrar_public_key` is the Ed25519 public key of the sealed seed (the
//! key the Delivery Service knows the registrar by; for a root handover, the
//! root's public key). `sealed` is
//! `eph_pub(32) || ChaCha20-Poly1305(seed(32)) || tag(16)`, with AAD = every
//! byte before it. Fixed length: [`REGISTRAR_SEED_ENVELOPE_LEN`] (205).
//! Rank: `1` member, `2` admin (the `lakko_ds_proof::Rank` bytes), `3` a
//! root handover (the outgoing owner's root seed after an ownership
//! transfer; `lakko_ds_proof::statement::ROOT_RANK_BYTE`).
//!
//! v1 (173 bytes, no public key) is refused as an unknown version: a flag
//! day, there is no production data.
//!
//! ## What the receiver still does
//!
//! Keeps the envelope only when `member_account_key` is its own account key
//! and the sender resolves (at receipt, C-68) to the journal's owner - the
//! seed is meaningful only if the root that derived it is the one the DS
//! knows; a seed from anyone else simply fails at the DS. Opening needs the
//! account X25519 secret; a failed open is dropped, never shown. A root
//! handover (rank `3`) is sent by the OUTGOING owner, so its gate is the
//! other way round: keep it only while the live role table names the
//! receiver owner; the DS refuses a `root` statement signed by anything but
//! the current root. Every member, addressee or not, may record
//! `member_account_key -> registrar_public_key` from an envelope whose
//! sender resolves to the owner; that is what an admin revokes on a removal.

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::SigningKey;
use thiserror::Error;

use crate::envelope::{self, EPH_PUB_LEN, TAG_LEN};

/// 27-byte domain separator prepended to the envelope. The trailing `\x00`
/// keeps a UTF-8 free-text payload from ever starting with it.
///
/// **Frozen:** [`crate::incoming_envelope`] dispatches on this exact prefix;
/// the in-payload version byte is the knob that moves.
pub const REGISTRAR_SEED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/ds-registrar-seed/v1\x00";

/// Schema-version byte carried first after the separator. v2 added the
/// clear registrar public key; v1 is refused.
pub const REGISTRAR_SEED_VERSION_V2: u8 = 0x02;

/// A journal id, an account key, a registrar public key, a seed: 32 bytes
/// each.
pub const REGISTRAR_SEED_KEY_LEN: usize = 32;

/// The sealed seed: ephemeral key, ciphertext of the 32-byte seed, tag.
pub const REGISTRAR_SEED_SEALED_LEN: usize = EPH_PUB_LEN + REGISTRAR_SEED_KEY_LEN + TAG_LEN;

/// Bytes after the separator: version, journal id, account key, rank,
/// registrar public key, sealed.
pub const REGISTRAR_SEED_PAYLOAD_LEN: usize = 1
    + REGISTRAR_SEED_KEY_LEN
    + REGISTRAR_SEED_KEY_LEN
    + 1
    + REGISTRAR_SEED_KEY_LEN
    + REGISTRAR_SEED_SEALED_LEN;

/// The one well-formed length.
pub const REGISTRAR_SEED_ENVELOPE_LEN: usize =
    REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len() + REGISTRAR_SEED_PAYLOAD_LEN;

/// The clear header (the sealed box's AAD) length.
const HEADER_LEN: usize = REGISTRAR_SEED_ENVELOPE_LEN - REGISTRAR_SEED_SEALED_LEN;

// Payload offsets (after the separator).
const JOURNAL_AT: usize = 1;
const MEMBER_AT: usize = JOURNAL_AT + REGISTRAR_SEED_KEY_LEN;
const RANK_AT: usize = MEMBER_AT + REGISTRAR_SEED_KEY_LEN;
const PUBLIC_KEY_AT: usize = RANK_AT + 1;
const SEALED_AT: usize = PUBLIC_KEY_AT + REGISTRAR_SEED_KEY_LEN;

/// The rank the root appointed the member at. Byte values match
/// `lakko_ds_proof::Rank`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegistrarSeedRank {
    /// A member that may commit self-service changes.
    Member,
    /// One of the owner's admins.
    Admin,
    /// A root handover: the outgoing owner's journal ROOT seed, sealed to
    /// the new owner after an ADR-0045 ownership transfer lands, so the new
    /// owner can sign the `root` statement naming its own root. The byte is
    /// `lakko_ds_proof::statement::ROOT_RANK_BYTE`. The receiver keeps it
    /// only while the live role table names it owner; the Delivery Service
    /// refuses a seed that is not the current root, so a forged one is
    /// harmless.
    Root,
}

impl RegistrarSeedRank {
    const fn to_byte(self) -> u8 {
        match self {
            Self::Member => 1,
            Self::Admin => 2,
            Self::Root => 3,
        }
    }
}

/// The Ed25519 public key a registrar seed signs as: the key the Delivery
/// Service knows the registrar by (`lakko_ds_proof::StatementSigner`'s
/// `public_key` for the same seed).
#[must_use]
pub fn registrar_public_key(seed: &[u8; REGISTRAR_SEED_KEY_LEN]) -> [u8; REGISTRAR_SEED_KEY_LEN] {
    SigningKey::from_seed(seed).verifying_key().to_bytes()
}

/// A decoded registrar-seed envelope. The seed is still sealed:
/// [`Self::open`] with the addressee's account X25519 secret recovers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrarSeedEnvelope {
    /// The journal (group id) the registrar key is for.
    pub journal_id: [u8; REGISTRAR_SEED_KEY_LEN],
    /// The addressee's account Ed25519 public key.
    pub member_account_key: [u8; REGISTRAR_SEED_KEY_LEN],
    /// The rank the root registered the key at.
    pub rank: RegistrarSeedRank,
    /// The registrar's Ed25519 public key, readable without opening. Bound
    /// to the seal (AAD) and checked against the opened seed by
    /// [`Self::open`]; an unopened envelope's key is trusted only as far as
    /// its MLS sender is (the owner, resolved at receipt).
    pub registrar_public_key: [u8; REGISTRAR_SEED_KEY_LEN],
    /// The sealed 32-byte seed.
    pub sealed: [u8; REGISTRAR_SEED_SEALED_LEN],
}

fn header(
    journal_id: &[u8; REGISTRAR_SEED_KEY_LEN],
    member_account_key: &[u8; REGISTRAR_SEED_KEY_LEN],
    rank: RegistrarSeedRank,
    registrar_public_key: &[u8; REGISTRAR_SEED_KEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(REGISTRAR_SEED_ENVELOPE_LEN);
    out.extend_from_slice(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1);
    out.push(REGISTRAR_SEED_VERSION_V2);
    out.extend_from_slice(journal_id);
    out.extend_from_slice(member_account_key);
    out.push(rank.to_byte());
    out.extend_from_slice(registrar_public_key);
    out
}

/// Compose the envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// `member_x25519_pub` is the addressee account's X25519 public key (its
/// identity bundle's `x25519_pubkey`); only that account can open it. The
/// clear registrar public key is derived from `seed` here, so a composer
/// cannot send a mismatched one.
///
/// # Errors
///
/// [`RegistrarSeedError::Seal`] if the sealed box fails (not expected with
/// valid inputs).
pub fn compose_registrar_seed(
    journal_id: &[u8; REGISTRAR_SEED_KEY_LEN],
    member_account_key: &[u8; REGISTRAR_SEED_KEY_LEN],
    member_x25519_pub: &X25519PublicKey,
    rank: RegistrarSeedRank,
    seed: &[u8; REGISTRAR_SEED_KEY_LEN],
) -> CoreResult<Vec<u8>, RegistrarSeedError> {
    let public_key = registrar_public_key(seed);
    let mut out = header(journal_id, member_account_key, rank, &public_key);
    let sealed =
        envelope::seal(member_x25519_pub, &out, seed).map_err(|_| RegistrarSeedError::Seal)?;
    if sealed.len() != REGISTRAR_SEED_SEALED_LEN {
        return Err(RegistrarSeedError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn array_at(
    payload: &[u8],
    at: usize,
) -> CoreResult<[u8; REGISTRAR_SEED_KEY_LEN], RegistrarSeedError> {
    payload
        .get(at..at + REGISTRAR_SEED_KEY_LEN)
        .and_then(|s| s.try_into().ok())
        .ok_or(RegistrarSeedError::BadLength {
            actual: payload.len(),
        })
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(env))` — the separator matched and the envelope is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed; the caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`RegistrarSeedError::UnknownVersion`] — checked BEFORE the length, so
///   a v1 envelope (or a later revision) reports as a version, not a length.
/// * [`RegistrarSeedError::BadLength`] — the payload is not exactly
///   [`REGISTRAR_SEED_PAYLOAD_LEN`].
/// * [`RegistrarSeedError::UnknownRank`] — the rank byte is neither member,
///   admin nor root.
pub fn try_unwrap_registrar_seed(
    plaintext: &[u8],
) -> CoreResult<Option<RegistrarSeedEnvelope>, RegistrarSeedError> {
    let Some(payload) = plaintext.strip_prefix(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload.split_first().ok_or(RegistrarSeedError::BadLength {
        actual: payload.len(),
    })?;
    if version != REGISTRAR_SEED_VERSION_V2 {
        return Err(RegistrarSeedError::UnknownVersion(version));
    }
    if payload.len() != REGISTRAR_SEED_PAYLOAD_LEN {
        return Err(RegistrarSeedError::BadLength {
            actual: payload.len(),
        });
    }
    let journal_id = array_at(payload, JOURNAL_AT)?;
    let member_account_key = array_at(payload, MEMBER_AT)?;
    let rank = match payload.get(RANK_AT) {
        Some(1) => RegistrarSeedRank::Member,
        Some(2) => RegistrarSeedRank::Admin,
        Some(3) => RegistrarSeedRank::Root,
        Some(&other) => return Err(RegistrarSeedError::UnknownRank(other)),
        None => {
            return Err(RegistrarSeedError::BadLength {
                actual: payload.len(),
            })
        },
    };
    let registrar_public_key = array_at(payload, PUBLIC_KEY_AT)?;
    let sealed: [u8; REGISTRAR_SEED_SEALED_LEN] = payload
        .get(SEALED_AT..)
        .and_then(|s| s.try_into().ok())
        .ok_or(RegistrarSeedError::BadLength {
            actual: payload.len(),
        })?;
    Ok(Some(RegistrarSeedEnvelope {
        journal_id,
        member_account_key,
        rank,
        registrar_public_key,
        sealed,
    }))
}

/// Strict standalone decoder: a plaintext without the separator is a hard
/// error.
///
/// # Errors
///
/// * [`RegistrarSeedError::WrongDomain`] — no separator.
/// * Every error [`try_unwrap_registrar_seed`] returns.
pub fn decode_registrar_seed(
    bytes: &[u8],
) -> CoreResult<RegistrarSeedEnvelope, RegistrarSeedError> {
    try_unwrap_registrar_seed(bytes)?.ok_or(RegistrarSeedError::WrongDomain)
}

impl RegistrarSeedEnvelope {
    /// Whether this envelope is addressed to `account_key`.
    #[must_use]
    pub fn is_addressed_to(&self, account_key: &[u8; REGISTRAR_SEED_KEY_LEN]) -> bool {
        self.member_account_key == *account_key
    }

    /// The exact wire bytes this envelope decoded from (the encoding is
    /// canonical: every field is fixed-width).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = header(
            &self.journal_id,
            &self.member_account_key,
            self.rank,
            &self.registrar_public_key,
        );
        out.extend_from_slice(&self.sealed);
        out
    }

    /// Open the seed with the addressee account's X25519 secret. The clear
    /// header is the AAD, so a header altered in transit - another journal,
    /// another rank, another addressee, another registrar public key - fails
    /// to open. The opened seed must then derive exactly
    /// [`Self::registrar_public_key`].
    ///
    /// # Errors
    ///
    /// * [`RegistrarSeedError::Open`] on a wrong key or any tampering.
    /// * [`RegistrarSeedError::PublicKeyMismatch`] when the seal opens but
    ///   the seed does not derive the header's registrar public key (only a
    ///   hand-built envelope gets here; [`compose_registrar_seed`] derives
    ///   the key from the seed).
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<[u8; REGISTRAR_SEED_KEY_LEN], RegistrarSeedError> {
        let aad = header(
            &self.journal_id,
            &self.member_account_key,
            self.rank,
            &self.registrar_public_key,
        );
        debug_assert_eq!(aad.len(), HEADER_LEN);
        let plain = envelope::open(account_x25519_secret, &aad, &self.sealed)
            .map_err(|_| RegistrarSeedError::Open)?;
        let seed: [u8; REGISTRAR_SEED_KEY_LEN] = plain
            .as_slice()
            .try_into()
            .map_err(|_| RegistrarSeedError::Open)?;
        if registrar_public_key(&seed) != self.registrar_public_key {
            return Err(RegistrarSeedError::PublicKeyMismatch);
        }
        Ok(seed)
    }
}

/// Why a registrar-seed envelope was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RegistrarSeedError {
    /// No domain separator. Only from [`decode_registrar_seed`].
    #[error("plaintext does not carry the registrar-seed domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error(
        "registrar-seed envelope has {actual} payload bytes, expected {REGISTRAR_SEED_PAYLOAD_LEN}"
    )]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The version byte is not [`REGISTRAR_SEED_VERSION_V2`].
    #[error("registrar-seed envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The rank byte is neither member, admin nor root.
    #[error("registrar-seed envelope carries unknown rank {0:#04x}")]
    UnknownRank(u8),
    /// Sealing the seed failed.
    #[error("registrar seed could not be sealed")]
    Seal,
    /// The sealed seed did not open under the given key.
    #[error("registrar seed did not open")]
    Open,
    /// The seed opened but does not derive the header's registrar public
    /// key.
    #[error("registrar seed does not derive the envelope's registrar public key")]
    PublicKeyMismatch,
}

impl RegistrarSeedError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::UnknownRank(_) => "unknownRank",
            Self::Seal => "seal",
            Self::Open => "open",
            Self::PublicKeyMismatch => "publicKeyMismatch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOURNAL: [u8; 32] = [0x4A; 32];
    const MEMBER: [u8; 32] = [0x4B; 32];
    const SEED: [u8; 32] = [0x5C; 32];
    const RANKS: [RegistrarSeedRank; 3] = [
        RegistrarSeedRank::Member,
        RegistrarSeedRank::Admin,
        RegistrarSeedRank::Root,
    ];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn wire(rank: RegistrarSeedRank) -> Vec<u8> {
        let (_, public) = member_keys();
        compose_registrar_seed(&JOURNAL, &MEMBER, &public, rank, &SEED).unwrap()
    }

    const PK_AT: usize = 27 + PUBLIC_KEY_AT;

    #[test]
    fn compose_decode_open_round_trips_at_a_fixed_length() {
        let (secret, _) = member_keys();
        for rank in RANKS {
            let bytes = wire(rank);
            assert_eq!(bytes.len(), REGISTRAR_SEED_ENVELOPE_LEN);
            let env = decode_registrar_seed(&bytes).unwrap();
            assert_eq!(env.journal_id, JOURNAL);
            assert_eq!(env.rank, rank);
            assert!(env.is_addressed_to(&MEMBER));
            assert!(!env.is_addressed_to(&[0; 32]));
            assert_eq!(env.open(&secret).unwrap(), SEED);
            assert_eq!(env.to_bytes(), bytes);
            assert_eq!(try_unwrap_registrar_seed(&bytes).unwrap(), Some(env));
        }
    }

    #[test]
    fn every_rank_carries_the_registrar_public_key_in_the_clear() {
        let expected = SigningKey::from_seed(&SEED).verifying_key().to_bytes();
        assert_eq!(registrar_public_key(&SEED), expected);
        for rank in RANKS {
            let bytes = wire(rank);
            assert_eq!(&bytes[PK_AT..PK_AT + 32], &expected);
            let env = decode_registrar_seed(&bytes).unwrap();
            assert_eq!(env.registrar_public_key, expected, "{rank:?}");
        }
    }

    #[test]
    fn the_registrar_public_key_matches_the_ds_signer_derivation() {
        // The Delivery Service knows a registrar by
        // `SigningKey::from_seed(seed).verifying_key()` (lakko-ds-proof's
        // StatementSigner); a different seed gives a different key.
        assert_ne!(
            registrar_public_key(&SEED),
            registrar_public_key(&[0x5D; 32])
        );
    }

    #[test]
    fn the_seed_is_not_in_the_clear() {
        let bytes = wire(RegistrarSeedRank::Admin);
        assert!(!bytes.windows(SEED.len()).any(|w| w == SEED));
    }

    #[test]
    fn only_the_addressee_opens_it() {
        let env = decode_registrar_seed(&wire(RegistrarSeedRank::Member)).unwrap();
        let other = X25519SecretKey::from_bytes([0x44; 32]);
        assert_eq!(env.open(&other), Err(RegistrarSeedError::Open));
    }

    #[test]
    fn the_clear_header_is_bound_to_the_seal() {
        let (secret, _) = member_keys();
        let good = wire(RegistrarSeedRank::Member);
        let domain = REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len();
        let rank_at = domain + RANK_AT;
        // Another journal, another addressee, another rank, another
        // registrar public key: none opens.
        for at in [
            domain + JOURNAL_AT,
            domain + MEMBER_AT,
            rank_at,
            PK_AT,
            PK_AT + 31,
        ] {
            let mut tampered = good.clone();
            tampered[at] ^= if at == rank_at { 0x03 } else { 0x01 };
            let env = decode_registrar_seed(&tampered).unwrap();
            assert_eq!(
                env.open(&secret),
                Err(RegistrarSeedError::Open),
                "byte {at}"
            );
        }
    }

    #[test]
    fn a_sealed_seed_that_does_not_derive_the_header_key_is_refused() {
        // Built by hand: the header names one key, the correctly sealed
        // seed (AAD = that very header) derives another.
        let (secret, public) = member_keys();
        let wrong_key = registrar_public_key(&[0x77; 32]);
        let mut bytes = header(&JOURNAL, &MEMBER, RegistrarSeedRank::Admin, &wrong_key);
        let sealed = envelope::seal(&public, &bytes, &SEED).unwrap();
        bytes.extend_from_slice(&sealed);
        assert_eq!(bytes.len(), REGISTRAR_SEED_ENVELOPE_LEN);
        let env = decode_registrar_seed(&bytes).unwrap();
        assert_eq!(env.registrar_public_key, wrong_key);
        assert_eq!(
            env.open(&secret),
            Err(RegistrarSeedError::PublicKeyMismatch)
        );
    }

    #[test]
    fn domain_separator_and_length_are_frozen() {
        assert_eq!(
            REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            b"lakko/ds-registrar-seed/v1\x00"
        );
        assert_eq!(REGISTRAR_SEED_PAYLOAD_LEN, 178);
        assert_eq!(REGISTRAR_SEED_ENVELOPE_LEN, 27 + 178);
        assert_eq!(REGISTRAR_SEED_ENVELOPE_LEN, 205);
        let w = wire(RegistrarSeedRank::Admin);
        assert_eq!(w[0], b'l');
        assert_eq!(w[27], REGISTRAR_SEED_VERSION_V2);
        assert_eq!(REGISTRAR_SEED_VERSION_V2, 0x02);
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_decode_refuses_it() {
        assert_eq!(try_unwrap_registrar_seed(b"hello").unwrap(), None);
        assert_eq!(
            decode_registrar_seed(b"hello"),
            Err(RegistrarSeedError::WrongDomain)
        );
        assert!(matches!(
            try_unwrap_registrar_seed(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1),
            Err(RegistrarSeedError::BadLength { actual: 0 })
        ));
    }

    #[test]
    fn a_v1_envelope_is_refused_as_an_unknown_version() {
        // The exact v1 shape (173 bytes: no public key) reports the version,
        // not the length.
        let mut v1 = REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.to_vec();
        v1.push(0x01);
        v1.extend_from_slice(&JOURNAL);
        v1.extend_from_slice(&MEMBER);
        v1.push(1);
        v1.extend_from_slice(&[0xAB; REGISTRAR_SEED_SEALED_LEN]);
        assert_eq!(v1.len(), 173);
        assert_eq!(
            try_unwrap_registrar_seed(&v1),
            Err(RegistrarSeedError::UnknownVersion(0x01))
        );
        // A bare later version too.
        let mut v3 = REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.to_vec();
        v3.push(0x03);
        assert_eq!(
            try_unwrap_registrar_seed(&v3),
            Err(RegistrarSeedError::UnknownVersion(0x03))
        );
    }

    #[test]
    fn length_and_rank_are_checked() {
        let good = wire(RegistrarSeedRank::Member);
        assert!(matches!(
            try_unwrap_registrar_seed(&good[..good.len() - 1]),
            Err(RegistrarSeedError::BadLength { .. })
        ));
        let mut long = good.clone();
        long.push(0);
        assert!(matches!(
            try_unwrap_registrar_seed(&long),
            Err(RegistrarSeedError::BadLength { .. })
        ));
        for bad in [0x00, 0x04, 0xFF] {
            let mut r = good.clone();
            r[REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len() + RANK_AT] = bad;
            assert_eq!(
                try_unwrap_registrar_seed(&r),
                Err(RegistrarSeedError::UnknownRank(bad))
            );
        }
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert!(
                !d.starts_with(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1)
                    && !REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
        // Every sibling kind falls through this decoder.
        assert_eq!(
            try_unwrap_registrar_seed(&crate::committer_beacon::compose_committer_beacon())
                .unwrap(),
            None
        );
    }
}
