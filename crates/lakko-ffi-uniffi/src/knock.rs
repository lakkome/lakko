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
use lakko_contacts::channel_epoch_seeds::{
    compose_channel_epoch_seeds as rust_compose_channel_epoch_seeds,
    decode_channel_epoch_seeds as rust_decode_channel_epoch_seeds, ChannelEpochSeed,
    ChannelEpochSeedsError, CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN,
    CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN, CHANNEL_EPOCH_SEEDS_MAX,
};
use lakko_contacts::channel_read_key::{
    compose_channel_read_key as rust_compose_channel_read_key,
    decode_channel_read_key as rust_decode_channel_read_key, ChannelReadKeyError,
    CHANNEL_READ_KEY_ENVELOPE_LEN,
};
use lakko_contacts::channel_rotate_request::{
    compose_channel_rotate_request as rust_compose_channel_rotate_request,
    decode_channel_rotate_request as rust_decode_channel_rotate_request, ChannelRotateRequestError,
    CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN,
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

// ============================================================================
// Channel read-key envelope (channel owner -> co-publisher, ADR-0087
// section 7 as amended for phase B)
// ============================================================================

/// A decoded channel read-key envelope; the read key is still sealed.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiChannelReadKeyEnvelope {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// The addressee's 32-byte account key.
    pub member_account_key: Vec<u8>,
}

fn channel_read_key_error(e: ChannelReadKeyError) -> FfiError {
    FfiError::Contacts(format!("channelReadKey.{}: {e}", e.code()))
}

/// Compose a channel read-key envelope for one co-publisher, sealed to that
/// co-publisher's account X25519 key; send it with
/// `PublisherGroup.createApplicationMessage`.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelReadKey.<code>` first, or a wrong
/// length.
#[uniffi::export]
pub fn compose_channel_read_key_envelope(
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
    member_x25519_public_key: Vec<u8>,
    read_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = <[u8; 16]>::try_from(channel_id.as_slice()).map_err(|_| {
        FfiError::Contacts("channelReadKey.badLength: channel id must be 16 bytes".to_string())
    })?;
    rust_compose_channel_read_key(
        &channel,
        &key32("member account key", &member_account_key)?,
        &x25519("member X25519 key", &member_x25519_public_key)?,
        &key32("read key", &read_key)?,
    )
    .map_err(channel_read_key_error)
}

/// Strictly decode a channel read-key envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelReadKey.<code>` first.
#[uniffi::export]
pub fn decode_channel_read_key_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiChannelReadKeyEnvelope, FfiError> {
    let env = rust_decode_channel_read_key(&plaintext).map_err(channel_read_key_error)?;
    Ok(FfiChannelReadKeyEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
    })
}

/// Open a channel read-key envelope with the ACCOUNT identity it is
/// addressed to, returning the 32-byte read key. The caller also checks the
/// MLS sender resolved to the channel's owner.
///
/// # Errors
///
/// [`FfiError::Contacts`] when malformed, addressed to another account, or
/// it does not open.
#[uniffi::export]
pub fn open_channel_read_key_envelope(
    plaintext: Vec<u8>,
    account_identity: &Identity,
) -> Result<Vec<u8>, FfiError> {
    let env = rust_decode_channel_read_key(&plaintext).map_err(channel_read_key_error)?;
    let own = key32(
        "own account key",
        account_identity.inner.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err(FfiError::Contacts(
            "channelReadKey.otherAccount: addressed to another account".to_string(),
        ));
    }
    let secret = X25519SecretKey::from_bytes(account_identity.inner.x25519_secret_bytes());
    env.open(&secret)
        .map(|k| k.to_vec())
        .map_err(channel_read_key_error)
}

/// The one well-formed channel read-key envelope length.
#[uniffi::export]
#[must_use]
pub fn channel_read_key_envelope_len() -> u32 {
    len_u32(CHANNEL_READ_KEY_ENVELOPE_LEN)
}

// ============================================================================
// Earlier channel seeds (channel owner -> new publisher, ADR-0087 section 7
// as amended 2026-11-08)
// ============================================================================

/// One channel epoch's content-key seed.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiChannelEpochSeed {
    /// The channel epoch id the seed serves.
    pub epoch: u32,
    /// The 32-byte content-key seed of that epoch. Secret.
    pub seed: Vec<u8>,
}

impl core::fmt::Debug for FfiChannelEpochSeed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FfiChannelEpochSeed")
            .field("epoch", &self.epoch)
            .field("seed", &"<redacted>")
            .finish()
    }
}

/// A decoded epoch-seeds envelope; the seeds are still sealed.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiChannelEpochSeedsEnvelope {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// The addressee's 32-byte account key.
    pub member_account_key: Vec<u8>,
    /// How many seeds the envelope carries, 1 to `channelEpochSeedsMax()`.
    pub count: u32,
}

fn channel_epoch_seeds_error(e: ChannelEpochSeedsError) -> FfiError {
    FfiError::Contacts(format!("channelEpochSeeds.{}: {e}", e.code()))
}

/// Compose an epoch-seeds envelope for one new publisher, sealed to that
/// publisher's account X25519 key; send it with
/// `PublisherGroup.createApplicationMessage` beside the read key. `seeds`
/// may come in any order; 1 to `channelEpochSeedsMax()` of them, no epoch
/// twice.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelEpochSeeds.<code>` first (`badCount`,
/// `duplicateEpoch`, `badLength` for a seed or channel id of the wrong
/// length), or a wrong key length.
#[uniffi::export]
pub fn compose_channel_epoch_seeds_envelope(
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
    member_x25519_public_key: Vec<u8>,
    seeds: Vec<FfiChannelEpochSeed>,
) -> Result<Vec<u8>, FfiError> {
    let channel = <[u8; 16]>::try_from(channel_id.as_slice()).map_err(|_| {
        FfiError::Contacts("channelEpochSeeds.badLength: channel id must be 16 bytes".to_string())
    })?;
    let seeds = seeds
        .iter()
        .map(|s| {
            <[u8; 32]>::try_from(s.seed.as_slice())
                .map(|seed| ChannelEpochSeed {
                    epoch: s.epoch,
                    seed,
                })
                .map_err(|_| {
                    FfiError::Contacts(
                        "channelEpochSeeds.badLength: a seed must be 32 bytes".to_string(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    rust_compose_channel_epoch_seeds(
        &channel,
        &key32("member account key", &member_account_key)?,
        &x25519("member X25519 key", &member_x25519_public_key)?,
        &seeds,
    )
    .map_err(channel_epoch_seeds_error)
}

/// Strictly decode an epoch-seeds envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelEpochSeeds.<code>` first.
#[uniffi::export]
pub fn decode_channel_epoch_seeds_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiChannelEpochSeedsEnvelope, FfiError> {
    let env = rust_decode_channel_epoch_seeds(&plaintext).map_err(channel_epoch_seeds_error)?;
    Ok(FfiChannelEpochSeedsEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        count: u32::from(env.count),
    })
}

/// Open an epoch-seeds envelope with the ACCOUNT identity it is addressed
/// to, returning the seeds in ascending epoch order. The caller also checks
/// the message arrived on this channel's PublisherGroup and the MLS sender
/// resolved at receipt to the channel's owner, then adds only the seeds of
/// epochs it holds none of.
///
/// # Errors
///
/// [`FfiError::Contacts`] when malformed, addressed to another account
/// (`channelEpochSeeds.otherAccount`), or it does not open.
#[uniffi::export]
pub fn open_channel_epoch_seeds_envelope(
    plaintext: Vec<u8>,
    account_identity: &Identity,
) -> Result<Vec<FfiChannelEpochSeed>, FfiError> {
    let env = rust_decode_channel_epoch_seeds(&plaintext).map_err(channel_epoch_seeds_error)?;
    let own = key32(
        "own account key",
        account_identity.inner.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err(FfiError::Contacts(
            "channelEpochSeeds.otherAccount: addressed to another account".to_string(),
        ));
    }
    let secret = X25519SecretKey::from_bytes(account_identity.inner.x25519_secret_bytes());
    let seeds = env.open(&secret).map_err(channel_epoch_seeds_error)?;
    Ok(seeds
        .iter()
        .map(|s| FfiChannelEpochSeed {
            epoch: s.epoch,
            seed: s.seed.to_vec(),
        })
        .collect())
}

/// Most seeds one epoch-seeds envelope carries (64).
#[uniffi::export]
#[must_use]
pub fn channel_epoch_seeds_max() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_MAX)
}

/// Length of an epoch-seeds envelope carrying one seed.
#[uniffi::export]
#[must_use]
pub fn channel_epoch_seeds_envelope_min_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN)
}

/// Length of an epoch-seeds envelope carrying the most seeds.
#[uniffi::export]
#[must_use]
pub fn channel_epoch_seeds_envelope_max_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN)
}

// ============================================================================
// A publisher asks the owner to rotate (ADR-0087 section 15 as amended
// 2026-11-08)
// ============================================================================

fn channel_rotate_request_error(e: ChannelRotateRequestError) -> FfiError {
    FfiError::Contacts(format!("channelRotateRequest.{}: {e}", e.code()))
}

/// Compose the request a publisher sends on the channel's PublisherGroup
/// with `PublisherGroup.createApplicationMessage` after removing a
/// subscriber: the owner's device rotates at its next publisher pass.
///
/// # Errors
///
/// [`FfiError::Contacts`] `channelRotateRequest.badLength` when the channel
/// id is not 16 bytes.
#[uniffi::export]
pub fn compose_channel_rotate_request_envelope(channel_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let channel = <[u8; 16]>::try_from(channel_id.as_slice()).map_err(|_| {
        FfiError::Contacts(
            "channelRotateRequest.badLength: channel id must be 16 bytes".to_string(),
        )
    })?;
    Ok(rust_compose_channel_rotate_request(&channel))
}

/// Strictly decode a rotate request, returning the 16-byte channel id. Only
/// the channel's owner acts on it, and only when it came on that channel's
/// PublisherGroup and the MLS sender resolved at receipt to a current
/// publisher.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelRotateRequest.<code>` first.
#[uniffi::export]
pub fn decode_channel_rotate_request_envelope(plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    rust_decode_channel_rotate_request(&plaintext)
        .map(|c| c.to_vec())
        .map_err(channel_rotate_request_error)
}

/// The one well-formed rotate-request length (49).
#[uniffi::export]
#[must_use]
pub fn channel_rotate_request_envelope_len() -> u32 {
    len_u32(CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoped_discovery::{derive_reach_grant, FfiReachClass};

    #[test]
    fn a_channel_rotate_request_round_trips() {
        let wire = compose_channel_rotate_request_envelope(vec![0x0C; 16]).unwrap();
        assert_eq!(wire.len(), channel_rotate_request_envelope_len() as usize);
        assert_eq!(channel_rotate_request_envelope_len(), 49);
        assert_eq!(
            decode_channel_rotate_request_envelope(wire.clone()).unwrap(),
            vec![0x0C; 16]
        );
        assert!(compose_channel_rotate_request_envelope(vec![0; 15]).is_err());
        let d = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            d.kind,
            crate::contacts::FfiIncomingEnvelopeKind::ChannelRotateRequest
        );
        assert_eq!(d.payload, vec![0x0C; 16]);
    }

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
    #[test]
    fn channel_epoch_seeds_reach_only_their_publisher_in_order() {
        let member = account();
        let other = Identity::from_seed(vec![0x72; 64]).unwrap();
        let seeds = vec![
            FfiChannelEpochSeed {
                epoch: 4,
                seed: vec![4; 32],
            },
            FfiChannelEpochSeed {
                epoch: 0,
                seed: vec![0; 32],
            },
        ];
        let wire = compose_channel_epoch_seeds_envelope(
            vec![0x0C; 16],
            member.signature_public_key(),
            member.x25519_public_key(),
            seeds,
        )
        .unwrap();
        let len = u32::try_from(wire.len()).unwrap();
        assert!(len > channel_epoch_seeds_envelope_min_len());
        assert!(len <= channel_epoch_seeds_envelope_max_len());
        let env = decode_channel_epoch_seeds_envelope(wire.clone()).unwrap();
        assert_eq!(env.count, 2);
        assert_eq!(env.channel_id, vec![0x0C; 16]);
        let opened = open_channel_epoch_seeds_envelope(wire.clone(), &member).unwrap();
        assert_eq!(
            opened.iter().map(|s| s.epoch).collect::<Vec<_>>(),
            vec![0, 4]
        );
        assert_eq!(opened[1].seed, vec![4; 32]);
        assert!(open_channel_epoch_seeds_envelope(wire, &other).is_err());
        assert_eq!(channel_epoch_seeds_max(), 64);
        assert!(compose_channel_epoch_seeds_envelope(
            vec![0x0C; 16],
            member.signature_public_key(),
            member.x25519_public_key(),
            Vec::new(),
        )
        .is_err());
    }

    #[test]
    fn a_channel_read_key_reaches_only_its_co_publisher() {
        let member = account();
        let member_key = member.inner.signature_public_key().to_vec();
        let x_pub = member.inner.x25519_public_key().to_vec();
        let wire = compose_channel_read_key_envelope(
            vec![0x0C; 16],
            member_key.clone(),
            x_pub,
            vec![0x5A; 32],
        )
        .unwrap();
        assert_eq!(wire.len(), channel_read_key_envelope_len() as usize);
        let env = decode_channel_read_key_envelope(wire.clone()).unwrap();
        assert_eq!(env.channel_id, vec![0x0C; 16]);
        assert_eq!(env.member_account_key, member_key);
        assert_eq!(
            open_channel_read_key_envelope(wire.clone(), &member).unwrap(),
            vec![0x5A; 32]
        );
        let other = Identity::from_seed(vec![0x72; 64]).unwrap();
        assert!(open_channel_read_key_envelope(wire, &other).is_err());
    }
}
