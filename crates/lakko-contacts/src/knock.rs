// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The sealed knock row: a request to join a group, sealed to the group's
//! knock key ([ADR-0087] section 5, ADR-0078 section 4 moved up).
//!
//! ## Keys
//!
//! ```text
//! knock_seed(v) = BLAKE3-derive-key("lakko/group/knock/v1", journal_root_seed ‖ v(2, BE))
//! ```
//!
//! The seed is an X25519 secret; its public key is published in a public
//! group's record and carried in a link's capability with `v`. The owner
//! derives every version from the ADR-0057 journal root seed and hands
//! `knock_seed(v)` to admins; removing an admin bumps `v`, and older
//! versions stay derivable so older links still open.
//!
//! ## Row
//!
//! ```text
//! row   = 0x02 ‖ v(2, BE) ‖ eph_x25519_pk(32) ‖ ChaCha20-Poly1305(plain)
//! plain = key_package_len(2) ‖ key_package ‖ grant_len(1)=134 ‖ reply_grant
//!         ‖ profile_len(2) ‖ profile ‖ capability_len(2) ‖ capability ‖ zeros
//! AAD   = KNOCK_AAD_DOMAIN ‖ 0x02 ‖ v
//! ```
//!
//! A row is exactly [`KNOCK_SMALL_ROW_LEN`] (4 KiB) or [`KNOCK_LARGE_ROW_LEN`]
//! (16 KiB): the plaintext is padded with zeros to the smaller bucket that
//! holds it, so the Delivery Service, which stores the row opaquely, learns
//! one bit of size. It carries no MLS message and no epoch, so a request
//! survives every commit while it waits in the queue.
//!
//! - **`key_package`** is opaque here (1 to [`KNOCK_MAX_KEY_PACKAGE_LEN`]
//!   bytes). `lakko_mls::Group::retain_join_request` validates it and its
//!   ADR-0050 binding.
//! - **`reply_grant`** is required and must be class `reply`: the Welcome
//!   goes to the joiner's mailbox under it (C-69).
//! - **`profile`** is the joiner's ADR-0048 profile envelope, possibly empty.
//!   A well-framed but invalid profile opens as `None`, as in a contact
//!   request: what the person claims is shown, not trusted.
//! - **`capability`** is the plaintext `/g/` capability the joiner came with,
//!   possibly empty. In a private group a row without a capability of this
//!   group is dropped unread by the admin; this module only frames it.
//!
//! Opening is bounded and fail-closed: a wrong version byte, a length that is
//! not a bucket, an AEAD failure, a field that overruns, a grant that is not
//! `reply`, and non-zero padding are all refused.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey, SecretKey};
use lakko_crypto::hash::blake3_derive_key_from_material;
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};
use crate::profile::{decode_profile, Profile, PROFILE_ENVELOPE_MAX_LEN};
use crate::reach::{ReachClass, ReachGrant, REACH_GRANT_LEN};

/// BLAKE3 derive-key context of a knock seed. Frozen.
pub const KNOCK_SEED_DOMAIN: &str = "lakko/group/knock/v1";

/// AAD domain of a knock row. Frozen.
pub const KNOCK_AAD_DOMAIN: &[u8] = b"lakko/group/knock-row/v1\x00";

/// The row's first byte.
pub const KNOCK_ROW_VERSION: u8 = 0x02;

/// Length of a knock seed and public key.
pub const KNOCK_KEY_LEN: usize = 32;

/// The smaller row size.
pub const KNOCK_SMALL_ROW_LEN: usize = 4 * 1024;

/// The larger row size; the Delivery Service's existing 16 KiB row bound.
pub const KNOCK_LARGE_ROW_LEN: usize = 16 * 1024;

/// Bytes of a row that are not plaintext: version, `v`, the ephemeral key
/// and the AEAD tag.
pub const KNOCK_ROW_OVERHEAD: usize = 1 + 2 + EPH_PUB_LEN + TAG_LEN;

/// Longest key package a knock carries. Chosen, not measured: the
/// `call_invite` bound, far above a Lakko KeyPackage.
pub const KNOCK_MAX_KEY_PACKAGE_LEN: usize = crate::call_invite::MAX_KEY_PACKAGE_LEN;

/// Longest capability a knock carries: the plaintext `/g/` capability.
pub const KNOCK_MAX_CAPABILITY_LEN: usize = crate::group_invite::MAX_WIRE_LEN;

const SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/group/knock/v1/key",
    nonce: b"lakko/group/knock/v1/nonce",
};

/// Derive the knock seed of version `version` from a journal root seed.
#[must_use]
pub fn knock_seed(journal_root_seed: &[u8; KNOCK_KEY_LEN], version: u16) -> [u8; KNOCK_KEY_LEN] {
    let mut material = [0u8; KNOCK_KEY_LEN + 2];
    material[..KNOCK_KEY_LEN].copy_from_slice(journal_root_seed);
    material[KNOCK_KEY_LEN..].copy_from_slice(&version.to_be_bytes());
    let out = blake3_derive_key_from_material(KNOCK_SEED_DOMAIN, &material);
    material.fill(0);
    out
}

/// The X25519 public key a knock seed opens rows for.
#[must_use]
pub fn knock_public_key(seed: &[u8; KNOCK_KEY_LEN]) -> [u8; KNOCK_KEY_LEN] {
    SecretKey::from_bytes(*seed).public_key().to_bytes()
}

/// What a knock carries, opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnockRequest {
    /// The joiner's KeyPackage wire, unvalidated here.
    pub key_package: Vec<u8>,
    /// The joiner's reply grant, class `reply`.
    pub reply_grant: ReachGrant,
    /// The joiner's profile, `None` when absent or invalid.
    pub profile: Option<Profile>,
    /// The plaintext `/g/` capability, when the joiner came by a link.
    pub capability: Option<Vec<u8>>,
}

/// Why a knock row was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KnockError {
    /// The row's first byte is not [`KNOCK_ROW_VERSION`].
    #[error("unknown knock row version {0}")]
    UnknownVersion(u8),
    /// The row is not one of the two sizes.
    #[error("knock row has the wrong length")]
    BadLength,
    /// The AEAD refused (another group's key, a tampered row).
    #[error("knock row did not open")]
    NotOpened,
    /// A field overruns the plaintext, or the padding is not zero.
    #[error("knock plaintext is malformed")]
    BadFraming,
    /// The key package is empty or too long.
    #[error("knock key package has a bad length")]
    BadKeyPackage,
    /// The reply grant is malformed or not class `reply`.
    #[error("knock reply grant is not a reply grant")]
    BadGrant,
    /// The profile or capability is longer than its bound.
    #[error("knock field too long")]
    FieldTooLong,
    /// The content does not fit the larger row.
    #[error("knock content does not fit a row")]
    TooLarge,
    /// The knock public key is not usable.
    #[error("knock public key is not usable")]
    BadKey,
}

impl KnockError {
    /// Stable, language-neutral reason code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadLength => "badLength",
            Self::NotOpened => "notOpened",
            Self::BadFraming => "badFraming",
            Self::BadKeyPackage => "badKeyPackage",
            Self::BadGrant => "badGrant",
            Self::FieldTooLong => "fieldTooLong",
            Self::TooLarge => "tooLarge",
            Self::BadKey => "badKey",
        }
    }
}

fn aad(version: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(KNOCK_AAD_DOMAIN.len() + 3);
    out.extend_from_slice(KNOCK_AAD_DOMAIN);
    out.push(KNOCK_ROW_VERSION);
    out.extend_from_slice(&version.to_be_bytes());
    out
}

/// Whether `len` is one of the two row sizes.
#[must_use]
pub const fn is_knock_row_len(len: usize) -> bool {
    len == KNOCK_SMALL_ROW_LEN || len == KNOCK_LARGE_ROW_LEN
}

/// The knock-key version a row names, without opening it: what an admin
/// reads to pick the seed. `None` when the row is not the right shape.
#[must_use]
pub fn knock_row_version(row: &[u8]) -> Option<u16> {
    if row.first() != Some(&KNOCK_ROW_VERSION) || !is_knock_row_len(row.len()) {
        return None;
    }
    Some(u16::from_be_bytes([*row.get(1)?, *row.get(2)?]))
}

fn frame(request: &KnockRequest, profile_bytes: &[u8]) -> CoreResult<Vec<u8>, KnockError> {
    if request.key_package.is_empty() || request.key_package.len() > KNOCK_MAX_KEY_PACKAGE_LEN {
        return Err(KnockError::BadKeyPackage);
    }
    if request.reply_grant.class != ReachClass::Reply {
        return Err(KnockError::BadGrant);
    }
    let capability = request.capability.as_deref().unwrap_or(&[]);
    if profile_bytes.len() > PROFILE_ENVELOPE_MAX_LEN || capability.len() > KNOCK_MAX_CAPABILITY_LEN
    {
        return Err(KnockError::FieldTooLong);
    }
    let len16 = |n: usize| u16::try_from(n).map_err(|_| KnockError::FieldTooLong);
    let mut out = Vec::new();
    out.extend_from_slice(&len16(request.key_package.len())?.to_be_bytes());
    out.extend_from_slice(&request.key_package);
    out.push(u8::try_from(REACH_GRANT_LEN).map_err(|_| KnockError::BadGrant)?);
    out.extend_from_slice(&request.reply_grant.encode());
    out.extend_from_slice(&len16(profile_bytes.len())?.to_be_bytes());
    out.extend_from_slice(profile_bytes);
    out.extend_from_slice(&len16(capability.len())?.to_be_bytes());
    out.extend_from_slice(capability);
    Ok(out)
}

/// Seal a request to join as a knock row for the group whose knock key of
/// version `version` is `knock_pk`.
///
/// `profile` is the joiner's composed profile envelope (empty for none);
/// `request.profile`, the decoded form an opener gets back, is not read
/// here, so the sealed bytes are exactly the envelope the joiner composed.
///
/// # Errors
///
/// [`KnockError::BadKeyPackage`], [`KnockError::BadGrant`],
/// [`KnockError::FieldTooLong`], [`KnockError::TooLarge`],
/// [`KnockError::BadKey`], [`KnockError::NotOpened`] (the AEAD refused; not
/// expected).
pub fn seal_knock(
    knock_pk: &[u8; KNOCK_KEY_LEN],
    version: u16,
    request: &KnockRequest,
    profile: &[u8],
) -> CoreResult<Vec<u8>, KnockError> {
    let mut plaintext = frame(request, profile)?;
    let bucket = [KNOCK_SMALL_ROW_LEN, KNOCK_LARGE_ROW_LEN]
        .into_iter()
        .map(|row| row - KNOCK_ROW_OVERHEAD)
        .find(|room| plaintext.len() <= *room)
        .ok_or(KnockError::TooLarge)?;
    plaintext.resize(bucket, 0);
    let recipient = PublicKey::from_bytes(knock_pk).map_err(|_| KnockError::BadKey)?;
    let sealed = envelope::seal_labeled(&SEAL_LABELS, &recipient, &aad(version), &plaintext)
        .map_err(|_| KnockError::NotOpened)?;
    plaintext.fill(0);
    let mut row = Vec::with_capacity(3 + sealed.len());
    row.push(KNOCK_ROW_VERSION);
    row.extend_from_slice(&version.to_be_bytes());
    row.extend_from_slice(&sealed);
    Ok(row)
}

/// Open a knock row with the knock seed of the version it names
/// ([`knock_row_version`]).
///
/// # Errors
///
/// [`KnockError::UnknownVersion`], [`KnockError::BadLength`],
/// [`KnockError::NotOpened`], and the parse errors of the plaintext.
pub fn open_knock(seed: &[u8; KNOCK_KEY_LEN], row: &[u8]) -> CoreResult<KnockRequest, KnockError> {
    let (&first, rest) = row.split_first().ok_or(KnockError::BadLength)?;
    if first != KNOCK_ROW_VERSION {
        return Err(KnockError::UnknownVersion(first));
    }
    if !is_knock_row_len(row.len()) {
        return Err(KnockError::BadLength);
    }
    let (version, sealed) = rest.split_at(2);
    let version = u16::from_be_bytes([version[0], version[1]]);
    let mut plaintext = envelope::open_labeled(
        &SEAL_LABELS,
        &SecretKey::from_bytes(*seed),
        &aad(version),
        sealed,
    )
    .map_err(|_| KnockError::NotOpened)?;
    let parsed = parse_plaintext(&plaintext);
    plaintext.fill(0);
    parsed
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], KnockError> {
    if cur.len() < n {
        return Err(KnockError::BadFraming);
    }
    let (head, tail) = cur.split_at(n);
    *cur = tail;
    Ok(head)
}

fn take_u16(cur: &mut &[u8]) -> CoreResult<usize, KnockError> {
    let b = take(cur, 2)?;
    Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

/// Parse an opened plaintext. Exposed to the fuzz harness through
/// `fuzz_api`, which cannot reach it through the AEAD.
fn parse_plaintext(plaintext: &[u8]) -> CoreResult<KnockRequest, KnockError> {
    let mut cur = plaintext;
    let kp_len = take_u16(&mut cur)?;
    if kp_len == 0 || kp_len > KNOCK_MAX_KEY_PACKAGE_LEN {
        return Err(KnockError::BadKeyPackage);
    }
    let key_package = take(&mut cur, kp_len)?.to_vec();
    let grant_len = usize::from(take(&mut cur, 1)?[0]);
    if grant_len != REACH_GRANT_LEN {
        return Err(KnockError::BadGrant);
    }
    let reply_grant =
        ReachGrant::decode(take(&mut cur, REACH_GRANT_LEN)?).map_err(|_| KnockError::BadGrant)?;
    if reply_grant.class != ReachClass::Reply {
        return Err(KnockError::BadGrant);
    }
    let profile_len = take_u16(&mut cur)?;
    if profile_len > PROFILE_ENVELOPE_MAX_LEN {
        return Err(KnockError::FieldTooLong);
    }
    let profile_bytes = take(&mut cur, profile_len)?;
    let profile = if profile_bytes.is_empty() {
        None
    } else {
        match decode_profile(profile_bytes) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(reason = %e.code(), "knock carried an invalid profile; opening without it");
                None
            },
        }
    };
    let capability_len = take_u16(&mut cur)?;
    if capability_len > KNOCK_MAX_CAPABILITY_LEN {
        return Err(KnockError::FieldTooLong);
    }
    let capability = take(&mut cur, capability_len)?;
    if cur.iter().any(|b| *b != 0) {
        return Err(KnockError::BadFraming);
    }
    Ok(KnockRequest {
        key_package,
        reply_grant,
        profile,
        capability: (!capability.is_empty()).then(|| capability.to_vec()),
    })
}

/// The fuzz oracle behind `fuzz_api::sealed_knock_open`: open `row` under a
/// fixed seed; a knock that opens must re-seal and re-open to the same
/// request. `None` when the open refuses.
pub(crate) fn fuzz_open(row: &[u8]) -> Option<bool> {
    let seed = [0x4B; KNOCK_KEY_LEN];
    let _ = knock_row_version(row);
    let request = open_knock(&seed, row).ok()?;
    Some(reopens(&seed, &request))
}

/// The fuzz oracle behind `fuzz_api::knock_plaintext`, which a fuzzer
/// cannot reach through the AEAD. `None` when the parser refuses.
pub(crate) fn fuzz_plaintext(plaintext: &[u8]) -> Option<bool> {
    let request = parse_plaintext(plaintext).ok()?;
    Some(reopens(&[0x4B; KNOCK_KEY_LEN], &request))
}

fn reopens(seed: &[u8; KNOCK_KEY_LEN], request: &KnockRequest) -> bool {
    let profile = request
        .profile
        .as_ref()
        .and_then(|p| crate::profile::compose_profile(p).ok())
        .unwrap_or_default();
    let Ok(row) = seal_knock(&knock_public_key(seed), 1, request, &profile) else {
        return false;
    };
    open_knock(seed, &row).ok().as_ref() == Some(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn grant(class: ReachClass) -> ReachGrant {
        let scope: &[u8] = if class == ReachClass::Reply {
            &[5; 16]
        } else {
            &[]
        };
        ReachGrant::derive(&[0x61; 32], class, scope, 1).expect("grant")
    }

    fn request() -> KnockRequest {
        KnockRequest {
            key_package: vec![0xAA; 600],
            reply_grant: grant(ReachClass::Reply),
            profile: None,
            capability: Some(vec![0xCC; 300]),
        }
    }

    #[test]
    fn a_knock_round_trips_under_its_version_and_pads_to_a_bucket() {
        let seed = knock_seed(&[0x11; 32], 3);
        let row = seal_knock(&knock_public_key(&seed), 3, &request(), &[]).unwrap();
        assert_eq!(row.len(), KNOCK_SMALL_ROW_LEN);
        assert_eq!(knock_row_version(&row), Some(3));
        assert_eq!(open_knock(&seed, &row).unwrap(), request());

        let mut big = request();
        big.key_package = vec![0xAB; 4096];
        let row = seal_knock(&knock_public_key(&seed), 3, &big, &[]).unwrap();
        assert_eq!(row.len(), KNOCK_LARGE_ROW_LEN);
        assert_eq!(open_knock(&seed, &row).unwrap(), big);
    }

    /// ADR-0087 section 18.4: the largest request (a maximal key package,
    /// a reply grant, a maximal profile and a maximal `/g/` capability, the
    /// photo in its preview included) still fits the large row.
    #[test]
    fn the_largest_request_fits_the_large_row() {
        let seed = knock_seed(&[0x11; 32], 1);
        let mut big = request();
        big.key_package = vec![0xAB; KNOCK_MAX_KEY_PACKAGE_LEN];
        big.capability = Some(vec![0xCC; KNOCK_MAX_CAPABILITY_LEN]);
        let profile = vec![0xDD; PROFILE_ENVELOPE_MAX_LEN];
        let row = seal_knock(&knock_public_key(&seed), 1, &big, &profile).unwrap();
        assert_eq!(row.len(), KNOCK_LARGE_ROW_LEN);
        assert_eq!(KNOCK_MAX_CAPABILITY_LEN, 1965);
    }

    #[test]
    fn another_version_or_group_does_not_open_it() {
        let root = [0x11; 32];
        let seed = knock_seed(&root, 1);
        let row = seal_knock(&knock_public_key(&seed), 1, &request(), &[]).unwrap();
        assert_eq!(
            open_knock(&knock_seed(&root, 2), &row),
            Err(KnockError::NotOpened)
        );
        assert_eq!(
            open_knock(&knock_seed(&[0x12; 32], 1), &row),
            Err(KnockError::NotOpened)
        );
        // The version in the clear is bound: rewriting it breaks the AEAD.
        let mut moved = row;
        moved[2] ^= 1;
        assert_eq!(open_knock(&seed, &moved), Err(KnockError::NotOpened));
    }

    #[test]
    fn versions_of_one_root_differ_and_older_ones_stay_derivable() {
        let root = [0x11; 32];
        assert_ne!(knock_seed(&root, 1), knock_seed(&root, 2));
        assert_eq!(knock_seed(&root, 1), knock_seed(&root, 1));
    }

    #[test]
    fn malformed_requests_are_refused_before_sealing() {
        let pk = knock_public_key(&[1; 32]);
        let mut r = request();
        r.reply_grant = grant(ReachClass::Contact);
        assert_eq!(seal_knock(&pk, 1, &r, &[]), Err(KnockError::BadGrant));
        let mut r = request();
        r.key_package.clear();
        assert_eq!(seal_knock(&pk, 1, &r, &[]), Err(KnockError::BadKeyPackage));
        let mut r = request();
        r.capability = Some(vec![0; KNOCK_MAX_CAPABILITY_LEN + 1]);
        assert_eq!(seal_knock(&pk, 1, &r, &[]), Err(KnockError::FieldTooLong));
    }

    #[test]
    fn a_bad_shape_is_refused_before_any_crypto() {
        let seed = [1; 32];
        assert_eq!(open_knock(&seed, &[]), Err(KnockError::BadLength));
        assert_eq!(
            open_knock(&seed, &[0x01; KNOCK_SMALL_ROW_LEN]),
            Err(KnockError::UnknownVersion(1))
        );
        let mut odd = vec![0u8; KNOCK_SMALL_ROW_LEN + 1];
        odd[0] = KNOCK_ROW_VERSION;
        assert_eq!(open_knock(&seed, &odd), Err(KnockError::BadLength));
        assert_eq!(knock_row_version(&odd), None);
    }

    #[test]
    fn non_zero_padding_and_an_invalid_profile_are_handled() {
        let mut plain = frame(&request(), &[]).unwrap();
        plain.resize(100 + plain.len(), 0);
        assert_eq!(parse_plaintext(&plain).unwrap(), request());
        let last = plain.len() - 1;
        plain[last] = 1;
        assert_eq!(parse_plaintext(&plain), Err(KnockError::BadFraming));
        // A well-framed but invalid profile opens as None.
        let plain = frame(&request(), b"not a profile").unwrap();
        assert_eq!(parse_plaintext(&plain).unwrap().profile, None);
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1200)) {
            let _ = parse_plaintext(&bytes);
        }
    }
}
