// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for requests to join that survive commits (ADR-0087
//! section 5): the group's knock key, the sealed knock row a joiner posts to
//! the DS knock, the envelope that hands an admin the knock seed, and the
//! envelope that hands a channel's co-publisher the admission seed
//! (section 7).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/knock.rs`. The admin opens a row
//! with the seed of the version the row names (`knockRowVersion`) and hands
//! the KeyPackage to `Group.retainJoinRequest`; the row never enters MLS.

use lakko_contacts::admission_seed::{
    admission_seed_public_key as rust_admission_seed_public_key,
    compose_admission_seed as rust_compose_admission_seed,
    decode_admission_seed as rust_decode_admission_seed, AdmissionSeedError,
    ADMISSION_SEED_ENVELOPE_LEN,
};
use lakko_contacts::knock::{
    is_knock_row_len as rust_is_knock_row_len, knock_public_key as rust_knock_public_key,
    knock_row_version as rust_knock_row_version, knock_seed as rust_knock_seed,
    open_knock as rust_open_knock, seal_knock as rust_seal_knock, KnockError, KnockRequest,
    KNOCK_LARGE_ROW_LEN, KNOCK_SMALL_ROW_LEN,
};
use lakko_contacts::knock_seed::{
    compose_knock_seed as rust_compose_knock_seed, decode_knock_seed as rust_decode_knock_seed,
    KnockSeedError, KNOCK_SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};

use crate::contacts::FfiProfile;
use crate::scoped_discovery::FfiReachGrant;
use crate::{FfiError, Identity};

fn key32(what: &str, bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Contacts(format!(
            "badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

fn x25519(what: &str, bytes: &[u8]) -> Result<X25519PublicKey, FfiError> {
    X25519PublicKey::from_bytes(bytes)
        .map_err(|_| FfiError::Contacts(format!("badKey: {what} is not an X25519 key")))
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn knock_error(e: KnockError) -> FfiError {
    FfiError::Contacts(format!("knock.{}: {e}", e.code()))
}

// ============================================================================
// Knock keys and rows
// ============================================================================

/// The 32-byte knock seed of `version` from the group's 32-byte journal root
/// seed (`dsJournalRootSeed`). Held by the owner and handed to the admins.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the root seed is not 32 bytes.
#[uniffi::export]
pub fn group_knock_seed(journal_root_seed: Vec<u8>, version: u16) -> Result<Vec<u8>, FfiError> {
    Ok(rust_knock_seed(&key32("journal root seed", &journal_root_seed)?, version).to_vec())
}

/// The 32-byte X25519 knock public key of a knock seed: what a `/g/` link
/// and a public group's handle record carry.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the seed is not 32 bytes.
#[uniffi::export]
pub fn group_knock_public_key(knock_seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_knock_public_key(&key32("knock seed", &knock_seed)?).to_vec())
}

/// What a knock row carries, opened.
#[derive(uniffi::Record, Debug, Clone)]
pub struct FfiKnockRequest {
    /// The joiner's KeyPackage wire, unvalidated here
    /// (`Group.retainJoinRequest` validates it).
    pub key_package: Vec<u8>,
    /// The joiner's reply grant, class `reply`. SECRET.
    pub reply_grant: FfiReachGrant,
    /// The joiner's profile, `None` when absent or invalid. The joiner's
    /// claim.
    pub profile: Option<FfiProfile>,
    /// The plaintext `/g/` capability, when the joiner came by a link.
    pub capability: Option<Vec<u8>>,
}

/// Seal a request to join as a knock row for the group whose knock key of
/// `version` is `knock_public_key`. `profile` is the joiner's composed
/// profile envelope (empty for none); `capability` the plaintext `/g/`
/// capability when the joiner came by a link. The row is padded to one of
/// two sizes.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `knock.<code>` first, or a wrong-length key.
#[uniffi::export]
pub fn seal_knock_row(
    knock_public_key: Vec<u8>,
    version: u16,
    key_package: Vec<u8>,
    reply_grant: FfiReachGrant,
    profile: Vec<u8>,
    capability: Option<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let pk = key32("knock public key", &knock_public_key)?;
    let request = KnockRequest {
        key_package,
        reply_grant: reply_grant.try_into()?,
        profile: None,
        capability,
    };
    rust_seal_knock(&pk, version, &request, &profile).map_err(knock_error)
}

/// The knock-key version a row names, without opening it; `None` when the
/// row is not the right shape. The admin picks the seed by it.
#[uniffi::export]
#[must_use]
pub fn knock_row_version(row: Vec<u8>) -> Option<u16> {
    rust_knock_row_version(&row)
}

/// Whether `len` is one of the two knock row sizes.
#[uniffi::export]
#[must_use]
pub fn is_knock_row_len(len: u32) -> bool {
    usize::try_from(len).is_ok_and(rust_is_knock_row_len)
}

/// The smaller knock row size (4 KiB).
#[uniffi::export]
#[must_use]
pub fn knock_small_row_len() -> u32 {
    len_u32(KNOCK_SMALL_ROW_LEN)
}

/// The larger knock row size (16 KiB).
#[uniffi::export]
#[must_use]
pub fn knock_large_row_len() -> u32 {
    len_u32(KNOCK_LARGE_ROW_LEN)
}

/// Open a knock row with the 32-byte knock seed of the version it names.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `knock.<code>` first (`notOpened` for
/// another group's row), or a wrong-length seed.
#[uniffi::export]
pub fn open_knock_row(knock_seed: Vec<u8>, row: Vec<u8>) -> Result<FfiKnockRequest, FfiError> {
    let seed = key32("knock seed", &knock_seed)?;
    let opened = rust_open_knock(&seed, &row).map_err(knock_error)?;
    Ok(FfiKnockRequest {
        key_package: opened.key_package,
        reply_grant: opened.reply_grant.into(),
        profile: opened.profile.map(Into::into),
        capability: opened.capability,
    })
}

// ============================================================================
// Knock-seed envelope (owner -> admin, in the group)
// ============================================================================

/// A decoded knock-seed envelope; the seed is still sealed.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiKnockSeedEnvelope {
    /// The 32-byte journal id the knock key belongs to.
    pub journal_id: Vec<u8>,
    /// The addressee's 32-byte account key.
    pub member_account_key: Vec<u8>,
    /// The knock-key version.
    pub knock_version: u16,
    /// The 32-byte knock public key, readable without opening.
    pub knock_public_key: Vec<u8>,
}

fn knock_seed_error(e: KnockSeedError) -> FfiError {
    FfiError::Contacts(format!("knockSeed.{}: {e}", e.code()))
}

/// Compose a knock-seed envelope for one admin (the owner's job), sealed to
/// that admin account's X25519 key.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length argument or an unusable key.
#[uniffi::export]
pub fn compose_knock_seed_envelope(
    journal_id: Vec<u8>,
    member_account_key: Vec<u8>,
    member_x25519_public_key: Vec<u8>,
    knock_version: u16,
    knock_seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    rust_compose_knock_seed(
        &key32("journal id", &journal_id)?,
        &key32("member account key", &member_account_key)?,
        &x25519("member X25519 key", &member_x25519_public_key)?,
        knock_version,
        &key32("knock seed", &knock_seed)?,
    )
    .map_err(knock_seed_error)
}

/// Strictly decode a knock-seed envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `knockSeed.<code>` first.
#[uniffi::export]
pub fn decode_knock_seed_envelope(plaintext: Vec<u8>) -> Result<FfiKnockSeedEnvelope, FfiError> {
    let env = rust_decode_knock_seed(&plaintext).map_err(knock_seed_error)?;
    Ok(FfiKnockSeedEnvelope {
        journal_id: env.journal_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        knock_version: env.knock_version,
        knock_public_key: env.knock_public_key.to_vec(),
    })
}

/// Open a knock-seed envelope with the ACCOUNT identity it is addressed to,
/// returning the 32-byte knock seed. The caller also checks the MLS sender
/// resolved to the group's owner.
///
/// # Errors
///
/// [`FfiError::Contacts`] when malformed, addressed to another account, or
/// it does not open.
#[uniffi::export]
pub fn open_knock_seed_envelope(
    plaintext: Vec<u8>,
    account_identity: &Identity,
) -> Result<Vec<u8>, FfiError> {
    let env = rust_decode_knock_seed(&plaintext).map_err(knock_seed_error)?;
    let own = key32(
        "own account key",
        account_identity.inner.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err(FfiError::Contacts(
            "knockSeed.otherAccount: addressed to another account".to_string(),
        ));
    }
    let secret = X25519SecretKey::from_bytes(account_identity.inner.x25519_secret_bytes());
    env.open(&secret)
        .map(|s| s.to_vec())
        .map_err(knock_seed_error)
}

/// The one well-formed knock-seed envelope length.
#[uniffi::export]
#[must_use]
pub fn knock_seed_envelope_len() -> u32 {
    len_u32(KNOCK_SEED_ENVELOPE_LEN)
}

// ============================================================================
// Admission-seed envelope (channel owner -> co-publisher, in the
// PublisherGroup)
// ============================================================================

/// A decoded admission-seed envelope; the seed is still sealed.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiAdmissionSeedEnvelope {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// The addressee's 32-byte account key.
    pub member_account_key: Vec<u8>,
    /// The admission-key version.
    pub admission_version: u16,
    /// The 32-byte admission public key, readable without opening.
    pub admission_public_key: Vec<u8>,
}

fn admission_seed_error(e: AdmissionSeedError) -> FfiError {
    FfiError::Contacts(format!("admissionSeed.{}: {e}", e.code()))
}

/// Compose an admission-seed envelope for one co-publisher, sealed to that
/// account's X25519 key. Send it as an application message in the channel's
/// PublisherGroup (`PublisherGroup.createApplicationMessage`).
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length argument or an unusable key.
#[uniffi::export]
pub fn compose_admission_seed_envelope(
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
    member_x25519_public_key: Vec<u8>,
    admission_version: u16,
    admission_seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = <[u8; 16]>::try_from(channel_id.as_slice())
        .map_err(|_| FfiError::Contacts("badLength: channel id must be 16 bytes".to_string()))?;
    rust_compose_admission_seed(
        &channel,
        &key32("member account key", &member_account_key)?,
        &x25519("member X25519 key", &member_x25519_public_key)?,
        admission_version,
        &key32("admission seed", &admission_seed)?,
    )
    .map_err(admission_seed_error)
}

/// Strictly decode an admission-seed envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `admissionSeed.<code>` first.
#[uniffi::export]
pub fn decode_admission_seed_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiAdmissionSeedEnvelope, FfiError> {
    let env = rust_decode_admission_seed(&plaintext).map_err(admission_seed_error)?;
    Ok(FfiAdmissionSeedEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        admission_version: env.admission_version,
        admission_public_key: env.admission_public_key.to_vec(),
    })
}

/// Open an admission-seed envelope with the ACCOUNT identity it is
/// addressed to, returning the 32-byte admission seed. The caller also
/// checks the MLS sender resolved to the channel's owner.
///
/// # Errors
///
/// [`FfiError::Contacts`] when malformed, addressed to another account, or
/// it does not open.
#[uniffi::export]
pub fn open_admission_seed_envelope(
    plaintext: Vec<u8>,
    account_identity: &Identity,
) -> Result<Vec<u8>, FfiError> {
    let env = rust_decode_admission_seed(&plaintext).map_err(admission_seed_error)?;
    let own = key32(
        "own account key",
        account_identity.inner.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err(FfiError::Contacts(
            "admissionSeed.otherAccount: addressed to another account".to_string(),
        ));
    }
    let secret = X25519SecretKey::from_bytes(account_identity.inner.x25519_secret_bytes());
    env.open(&secret)
        .map(|s| s.to_vec())
        .map_err(admission_seed_error)
}

/// The X25519 public key of an admission seed.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the seed is not 32 bytes.
#[uniffi::export]
pub fn admission_seed_public_key(admission_seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_admission_seed_public_key(&key32("admission seed", &admission_seed)?).to_vec())
}

/// The one well-formed admission-seed envelope length.
#[uniffi::export]
#[must_use]
pub fn admission_seed_envelope_len() -> u32 {
    len_u32(ADMISSION_SEED_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoped_discovery::{derive_reach_grant, FfiReachClass};

    fn account() -> Identity {
        Identity::from_seed(vec![0x71; 64]).unwrap()
    }

    #[test]
    fn knock_row_seals_and_opens_under_the_derived_seed() {
        let root = vec![0x11_u8; 32];
        let seed = group_knock_seed(root, 3).unwrap();
        let public = group_knock_public_key(seed.clone()).unwrap();
        let joiner = account();
        let grant = derive_reach_grant(
            joiner.inner.ed25519_seed_bytes().to_vec(),
            FfiReachClass::Reply,
            vec![0x0A; 16],
            0,
        )
        .unwrap();
        let row = seal_knock_row(public, 3, vec![0xAB; 300], grant, Vec::new(), None).unwrap();
        assert!(is_knock_row_len(u32::try_from(row.len()).unwrap()));
        assert_eq!(row.len(), knock_small_row_len() as usize);
        assert_eq!(knock_row_version(row.clone()), Some(3));
        let opened = open_knock_row(seed, row.clone()).unwrap();
        assert_eq!(opened.key_package, vec![0xAB; 300]);
        assert!(opened.profile.is_none());
        let other = group_knock_seed(vec![0x12; 32], 3).unwrap();
        let err = open_knock_row(other, row).unwrap_err();
        assert!(err.to_string().contains("knock.notOpened"), "{err}");
    }

    #[test]
    fn knock_seed_envelope_round_trips_to_its_addressee_only() {
        let admin = account();
        let other = Identity::from_seed(vec![0x72; 64]).unwrap();
        let seed = group_knock_seed(vec![0x11; 32], 1).unwrap();
        let wire = compose_knock_seed_envelope(
            vec![0x4A; 32],
            admin.inner.signature_public_key().to_vec(),
            admin.inner.x25519_public_key().to_vec(),
            1,
            seed.clone(),
        )
        .unwrap();
        assert_eq!(wire.len(), knock_seed_envelope_len() as usize);
        let decoded = decode_knock_seed_envelope(wire.clone()).unwrap();
        assert_eq!(
            decoded.knock_public_key,
            group_knock_public_key(seed.clone()).unwrap()
        );
        assert_eq!(
            open_knock_seed_envelope(wire.clone(), &admin).unwrap(),
            seed
        );
        assert!(open_knock_seed_envelope(wire, &other).is_err());
    }

    #[test]
    fn admission_seed_envelope_round_trips_to_its_addressee_only() {
        let copublisher = account();
        let seed = crate::channel_access::generate_channel_admission_seed();
        let wire = compose_admission_seed_envelope(
            vec![0x0C; 16],
            copublisher.inner.signature_public_key().to_vec(),
            copublisher.inner.x25519_public_key().to_vec(),
            2,
            seed.clone(),
        )
        .unwrap();
        assert_eq!(wire.len(), admission_seed_envelope_len() as usize);
        let decoded = decode_admission_seed_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.admission_version, 2);
        assert_eq!(
            decoded.admission_public_key,
            admission_seed_public_key(seed.clone()).unwrap()
        );
        assert_eq!(
            open_admission_seed_envelope(wire, &copublisher).unwrap(),
            seed
        );
    }
}
