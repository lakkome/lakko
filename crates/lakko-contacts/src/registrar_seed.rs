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
//! box's AAD. Other members see only whom it is for and at which rank.
//!
//! ## Wire format (v1)
//!
//! ```text
//! REGISTRAR_SEED_ENVELOPE_DOMAIN_V1 || version(1) || journal_id(32)
//!   || member_account_key(32) || rank(1) || sealed(80)
//! ```
//!
//! `sealed` is `eph_pub(32) || ChaCha20-Poly1305(seed(32)) || tag(16)`, with
//! AAD = every byte before it. Fixed length: [`REGISTRAR_SEED_ENVELOPE_LEN`].
//! Rank: `1` member, `2` admin (the `lakko_ds_proof::Rank` bytes).
//!
//! ## What the receiver still does
//!
//! Keeps the envelope only when `member_account_key` is its own account key
//! and the sender resolves (at receipt, C-68) to the journal's owner - the
//! seed is meaningful only if the root that derived it is the one the DS
//! knows; a seed from anyone else simply fails at the DS. Opening needs the
//! account X25519 secret; a failed open is dropped, never shown.

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{self, EPH_PUB_LEN, TAG_LEN};

/// 27-byte domain separator prepended to the envelope. The trailing `\x00`
/// keeps a UTF-8 free-text payload from ever starting with it.
///
/// **Frozen:** [`crate::incoming_envelope`] dispatches on this exact prefix;
/// the in-payload version byte is the knob that moves.
pub const REGISTRAR_SEED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/ds-registrar-seed/v1\x00";

/// Schema-version byte carried first after the separator.
pub const REGISTRAR_SEED_VERSION_V1: u8 = 0x01;

/// A journal id, an account key, a seed: 32 bytes each.
pub const REGISTRAR_SEED_KEY_LEN: usize = 32;

/// The sealed seed: ephemeral key, ciphertext of the 32-byte seed, tag.
pub const REGISTRAR_SEED_SEALED_LEN: usize = EPH_PUB_LEN + REGISTRAR_SEED_KEY_LEN + TAG_LEN;

/// Bytes after the separator: version, journal id, account key, rank, sealed.
pub const REGISTRAR_SEED_PAYLOAD_LEN: usize =
    1 + REGISTRAR_SEED_KEY_LEN + REGISTRAR_SEED_KEY_LEN + 1 + REGISTRAR_SEED_SEALED_LEN;

/// The one well-formed length.
pub const REGISTRAR_SEED_ENVELOPE_LEN: usize =
    REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len() + REGISTRAR_SEED_PAYLOAD_LEN;

/// The clear header (the sealed box's AAD) length.
const HEADER_LEN: usize = REGISTRAR_SEED_ENVELOPE_LEN - REGISTRAR_SEED_SEALED_LEN;

/// The rank the root appointed the member at. Byte values match
/// `lakko_ds_proof::Rank`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegistrarSeedRank {
    /// A member that may commit self-service changes.
    Member,
    /// One of the owner's admins.
    Admin,
}

impl RegistrarSeedRank {
    const fn to_byte(self) -> u8 {
        match self {
            Self::Member => 1,
            Self::Admin => 2,
        }
    }
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
    /// The sealed 32-byte seed.
    pub sealed: [u8; REGISTRAR_SEED_SEALED_LEN],
}

fn header(
    journal_id: &[u8; REGISTRAR_SEED_KEY_LEN],
    member_account_key: &[u8; REGISTRAR_SEED_KEY_LEN],
    rank: RegistrarSeedRank,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(REGISTRAR_SEED_ENVELOPE_LEN);
    out.extend_from_slice(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1);
    out.push(REGISTRAR_SEED_VERSION_V1);
    out.extend_from_slice(journal_id);
    out.extend_from_slice(member_account_key);
    out.push(rank.to_byte());
    out
}

/// Compose the envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// `member_x25519_pub` is the addressee account's X25519 public key (its
/// identity bundle's `x25519_pubkey`); only that account can open it.
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
    let mut out = header(journal_id, member_account_key, rank);
    let sealed =
        envelope::seal(member_x25519_pub, &out, seed).map_err(|_| RegistrarSeedError::Seal)?;
    if sealed.len() != REGISTRAR_SEED_SEALED_LEN {
        return Err(RegistrarSeedError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
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
///   a later revision that changes the length reports as a version.
/// * [`RegistrarSeedError::BadLength`] — the payload is not exactly
///   [`REGISTRAR_SEED_PAYLOAD_LEN`].
/// * [`RegistrarSeedError::UnknownRank`] — the rank byte is neither member
///   nor admin.
pub fn try_unwrap_registrar_seed(
    plaintext: &[u8],
) -> CoreResult<Option<RegistrarSeedEnvelope>, RegistrarSeedError> {
    let Some(payload) = plaintext.strip_prefix(REGISTRAR_SEED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload.split_first().ok_or(RegistrarSeedError::BadLength {
        actual: payload.len(),
    })?;
    if version != REGISTRAR_SEED_VERSION_V1 {
        return Err(RegistrarSeedError::UnknownVersion(version));
    }
    if payload.len() != REGISTRAR_SEED_PAYLOAD_LEN {
        return Err(RegistrarSeedError::BadLength {
            actual: payload.len(),
        });
    }
    let bad_length = || RegistrarSeedError::BadLength {
        actual: payload.len(),
    };
    let journal_id: [u8; REGISTRAR_SEED_KEY_LEN] =
        payload[1..33].try_into().map_err(|_| bad_length())?;
    let member_account_key: [u8; REGISTRAR_SEED_KEY_LEN] =
        payload[33..65].try_into().map_err(|_| bad_length())?;
    let rank = match payload[65] {
        1 => RegistrarSeedRank::Member,
        2 => RegistrarSeedRank::Admin,
        other => return Err(RegistrarSeedError::UnknownRank(other)),
    };
    let sealed: [u8; REGISTRAR_SEED_SEALED_LEN] =
        payload[66..].try_into().map_err(|_| bad_length())?;
    Ok(Some(RegistrarSeedEnvelope {
        journal_id,
        member_account_key,
        rank,
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

    /// Open the seed with the addressee account's X25519 secret. The clear
    /// header is the AAD, so a header altered in transit - another journal,
    /// another rank, another addressee - fails to open.
    ///
    /// # Errors
    ///
    /// [`RegistrarSeedError::Open`] on a wrong key or any tampering.
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<[u8; REGISTRAR_SEED_KEY_LEN], RegistrarSeedError> {
        let aad = header(&self.journal_id, &self.member_account_key, self.rank);
        debug_assert_eq!(aad.len(), HEADER_LEN);
        let plain = envelope::open(account_x25519_secret, &aad, &self.sealed)
            .map_err(|_| RegistrarSeedError::Open)?;
        plain
            .as_slice()
            .try_into()
            .map_err(|_| RegistrarSeedError::Open)
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
    /// The version byte is not [`REGISTRAR_SEED_VERSION_V1`].
    #[error("registrar-seed envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The rank byte is neither member nor admin.
    #[error("registrar-seed envelope carries unknown rank {0:#04x}")]
    UnknownRank(u8),
    /// Sealing the seed failed.
    #[error("registrar seed could not be sealed")]
    Seal,
    /// The sealed seed did not open under the given key.
    #[error("registrar seed did not open")]
    Open,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOURNAL: [u8; 32] = [0x4A; 32];
    const MEMBER: [u8; 32] = [0x4B; 32];
    const SEED: [u8; 32] = [0x5C; 32];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn wire(rank: RegistrarSeedRank) -> Vec<u8> {
        let (_, public) = member_keys();
        compose_registrar_seed(&JOURNAL, &MEMBER, &public, rank, &SEED).unwrap()
    }

    #[test]
    fn compose_decode_open_round_trips_at_a_fixed_length() {
        let (secret, _) = member_keys();
        for rank in [RegistrarSeedRank::Member, RegistrarSeedRank::Admin] {
            let bytes = wire(rank);
            assert_eq!(bytes.len(), REGISTRAR_SEED_ENVELOPE_LEN);
            let env = decode_registrar_seed(&bytes).unwrap();
            assert_eq!(env.journal_id, JOURNAL);
            assert_eq!(env.rank, rank);
            assert!(env.is_addressed_to(&MEMBER));
            assert!(!env.is_addressed_to(&[0; 32]));
            assert_eq!(env.open(&secret).unwrap(), SEED);
            assert_eq!(try_unwrap_registrar_seed(&bytes).unwrap(), Some(env));
        }
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
        // Another journal, another addressee, another rank: none opens.
        for at in [domain + 1, domain + 33, domain + 65] {
            let mut tampered = good.clone();
            tampered[at] ^= if at == domain + 65 { 0x03 } else { 0x01 };
            let env = decode_registrar_seed(&tampered).unwrap();
            assert_eq!(
                env.open(&secret),
                Err(RegistrarSeedError::Open),
                "byte {at}"
            );
        }
    }

    #[test]
    fn domain_separator_and_length_are_frozen() {
        assert_eq!(
            REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            b"lakko/ds-registrar-seed/v1\x00"
        );
        assert_eq!(REGISTRAR_SEED_ENVELOPE_LEN, 27 + 146);
        assert_eq!(wire(RegistrarSeedRank::Admin)[0], b'l');
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
    fn version_before_length_and_rank_are_checked() {
        let mut v2 = REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(
            try_unwrap_registrar_seed(&v2),
            Err(RegistrarSeedError::UnknownVersion(0x02))
        );
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
        for bad in [0x00, 0x03, 0xFF] {
            let mut r = good.clone();
            r[REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len() + 65] = bad;
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
