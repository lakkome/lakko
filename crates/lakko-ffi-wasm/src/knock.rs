// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for requests to join that survive commits (ADR-0087
//! section 5): the knock key, the sealed knock row, the knock-seed envelope
//! (owner to admin) and the admission-seed envelope (channel owner to
//! co-publisher, section 7).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/knock.rs`.
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`
//! (`knock.notOpened`, `knockSeed.otherAccount`, …).

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
    CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN, CHANNEL_EPOCH_SEEDS_MAX, CHANNEL_EPOCH_SEED_LEN,
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
    open_knock as rust_open_knock, seal_knock as rust_seal_knock, KnockError,
    KnockRequest as RustKnockRequest, KNOCK_LARGE_ROW_LEN, KNOCK_SMALL_ROW_LEN,
};
use lakko_contacts::knock_seed::{
    compose_knock_seed as rust_compose_knock_seed, decode_knock_seed as rust_decode_knock_seed,
    KnockSeedError, KNOCK_SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use wasm_bindgen::prelude::*;

use crate::contacts::WasmProfile;
use crate::mls::Identity;
use crate::scoped_discovery::ReachGrant;

fn key32(area: &str, what: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        format!(
            "{area}.badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        )
    })
}

fn x25519(area: &str, bytes: &[u8]) -> Result<X25519PublicKey, String> {
    X25519PublicKey::from_bytes(bytes)
        .map_err(|_| format!("{area}.badKey: the member X25519 key is not usable"))
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn knock_error(e: KnockError) -> String {
    format!("knock.{}: {e}", e.code())
}

/// The 32-byte knock seed of `version` from the group's journal root seed.
///
/// # Errors
///
/// Throws `knock.badLength: ...` for a root seed that is not 32 bytes.
#[wasm_bindgen(js_name = groupKnockSeed)]
pub fn group_knock_seed(journal_root_seed: &[u8], version: u16) -> Result<Vec<u8>, String> {
    Ok(rust_knock_seed(
        &key32("knock", "journal root seed", journal_root_seed)?,
        version,
    )
    .to_vec())
}

/// The 32-byte X25519 knock public key of a knock seed.
///
/// # Errors
///
/// Throws `knock.badLength: ...` for a seed that is not 32 bytes.
#[wasm_bindgen(js_name = groupKnockPublicKey)]
pub fn group_knock_public_key(knock_seed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_knock_public_key(&key32("knock", "knock seed", knock_seed)?).to_vec())
}

/// What a knock row carries, opened.
#[wasm_bindgen(js_name = KnockRequest)]
pub struct WasmKnockRequest {
    inner: RustKnockRequest,
}

#[wasm_bindgen(js_class = KnockRequest)]
impl WasmKnockRequest {
    /// The joiner's KeyPackage wire (`Group.retainJoinRequest` validates it).
    #[wasm_bindgen(getter, js_name = keyPackage)]
    #[must_use]
    pub fn key_package(&self) -> Vec<u8> {
        self.inner.key_package.clone()
    }

    /// The joiner's reply grant, class `reply`. SECRET.
    #[wasm_bindgen(getter, js_name = replyGrant)]
    #[must_use]
    pub fn reply_grant(&self) -> ReachGrant {
        ReachGrant::from_rust(self.inner.reply_grant.clone())
    }

    /// The joiner's profile (their claim), or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn profile(&self) -> Option<WasmProfile> {
        self.inner.profile.clone().map(Into::into)
    }

    /// The plaintext `/g/` capability, when the joiner came by a link.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn capability(&self) -> Option<Vec<u8>> {
        self.inner.capability.clone()
    }
}

/// Seal a request to join as a knock row for the group whose knock key of
/// `version` is `knockPublicKey`. `profile` is the joiner's composed profile
/// envelope (empty for none).
///
/// # Errors
///
/// Throws `knock.<code>: ...`.
#[wasm_bindgen(js_name = sealKnockRow)]
pub fn seal_knock_row(
    knock_public_key: &[u8],
    version: u16,
    key_package: &[u8],
    reply_grant: &ReachGrant,
    profile: &[u8],
    capability: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let pk = key32("knock", "knock public key", knock_public_key)?;
    let request = RustKnockRequest {
        key_package: key_package.to_vec(),
        reply_grant: reply_grant.rust().clone(),
        profile: None,
        capability,
    };
    rust_seal_knock(&pk, version, &request, profile).map_err(knock_error)
}

/// The knock-key version a row names, or `undefined`.
#[wasm_bindgen(js_name = knockRowVersion)]
#[must_use]
pub fn knock_row_version(row: &[u8]) -> Option<u16> {
    rust_knock_row_version(row)
}

/// Whether `len` is one of the two knock row sizes.
#[wasm_bindgen(js_name = isKnockRowLen)]
#[must_use]
pub fn is_knock_row_len(len: u32) -> bool {
    usize::try_from(len).is_ok_and(rust_is_knock_row_len)
}

/// The smaller knock row size.
#[wasm_bindgen(js_name = knockSmallRowLen)]
#[must_use]
pub fn knock_small_row_len() -> u32 {
    len_u32(KNOCK_SMALL_ROW_LEN)
}

/// The larger knock row size.
#[wasm_bindgen(js_name = knockLargeRowLen)]
#[must_use]
pub fn knock_large_row_len() -> u32 {
    len_u32(KNOCK_LARGE_ROW_LEN)
}

/// Open a knock row with the knock seed of the version it names.
///
/// # Errors
///
/// Throws `knock.<code>: ...` (`notOpened` for another group's row).
#[wasm_bindgen(js_name = openKnockRow)]
pub fn open_knock_row(knock_seed: &[u8], row: &[u8]) -> Result<WasmKnockRequest, String> {
    let seed = key32("knock", "knock seed", knock_seed)?;
    rust_open_knock(&seed, row)
        .map(|inner| WasmKnockRequest { inner })
        .map_err(knock_error)
}

// ============================================================================
// Knock-seed envelope
// ============================================================================

/// A decoded knock-seed envelope; the seed stays sealed.
#[wasm_bindgen(js_name = KnockSeedEnvelope)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmKnockSeedEnvelope {
    journal_id: Vec<u8>,
    member_account_key: Vec<u8>,
    knock_version: u16,
    knock_public_key: Vec<u8>,
}

#[wasm_bindgen(js_class = KnockSeedEnvelope)]
impl WasmKnockSeedEnvelope {
    /// The 32-byte journal id.
    #[wasm_bindgen(getter, js_name = journalId)]
    #[must_use]
    pub fn journal_id(&self) -> Vec<u8> {
        self.journal_id.clone()
    }

    /// The addressee's 32-byte account key.
    #[wasm_bindgen(getter, js_name = memberAccountKey)]
    #[must_use]
    pub fn member_account_key(&self) -> Vec<u8> {
        self.member_account_key.clone()
    }

    /// The knock-key version.
    #[wasm_bindgen(getter, js_name = knockVersion)]
    #[must_use]
    pub fn knock_version(&self) -> u16 {
        self.knock_version
    }

    /// The 32-byte knock public key.
    #[wasm_bindgen(getter, js_name = knockPublicKey)]
    #[must_use]
    pub fn knock_public_key(&self) -> Vec<u8> {
        self.knock_public_key.clone()
    }
}

fn knock_seed_error(e: KnockSeedError) -> String {
    format!("knockSeed.{}: {e}", e.code())
}

/// Compose a knock-seed envelope for one admin, sealed to that account's
/// X25519 key.
///
/// # Errors
///
/// Throws `knockSeed.<code>: ...`.
#[wasm_bindgen(js_name = composeKnockSeedEnvelope)]
pub fn compose_knock_seed_envelope(
    journal_id: &[u8],
    member_account_key: &[u8],
    member_x25519_public_key: &[u8],
    knock_version: u16,
    knock_seed: &[u8],
) -> Result<Vec<u8>, String> {
    rust_compose_knock_seed(
        &key32("knockSeed", "journal id", journal_id)?,
        &key32("knockSeed", "member account key", member_account_key)?,
        &x25519("knockSeed", member_x25519_public_key)?,
        knock_version,
        &key32("knockSeed", "knock seed", knock_seed)?,
    )
    .map_err(knock_seed_error)
}

/// Strictly decode a knock-seed envelope.
///
/// # Errors
///
/// Throws `knockSeed.<code>: ...`.
#[wasm_bindgen(js_name = decodeKnockSeedEnvelope)]
pub fn decode_knock_seed_envelope(plaintext: &[u8]) -> Result<WasmKnockSeedEnvelope, String> {
    let env = rust_decode_knock_seed(plaintext).map_err(knock_seed_error)?;
    Ok(WasmKnockSeedEnvelope {
        journal_id: env.journal_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        knock_version: env.knock_version,
        knock_public_key: env.knock_public_key.to_vec(),
    })
}

/// Open a knock-seed envelope with the ACCOUNT identity it is addressed to,
/// returning the 32-byte knock seed. Also check the MLS sender is the owner.
///
/// # Errors
///
/// Throws `knockSeed.<code>: ...` (`otherAccount` when addressed elsewhere).
#[wasm_bindgen(js_name = openKnockSeedEnvelope)]
pub fn open_knock_seed_envelope(
    plaintext: &[u8],
    account_identity: &Identity,
) -> Result<Vec<u8>, String> {
    let env = rust_decode_knock_seed(plaintext).map_err(knock_seed_error)?;
    let id = account_identity.mls_identity();
    let own = key32("knockSeed", "own account key", id.signature_public_key())?;
    if !env.is_addressed_to(&own) {
        return Err("knockSeed.otherAccount: addressed to another account".to_string());
    }
    env.open(&X25519SecretKey::from_bytes(id.x25519_secret_bytes()))
        .map(|s| s.to_vec())
        .map_err(knock_seed_error)
}

/// The one well-formed knock-seed envelope length.
#[wasm_bindgen(js_name = knockSeedEnvelopeLen)]
#[must_use]
pub fn knock_seed_envelope_len() -> u32 {
    len_u32(KNOCK_SEED_ENVELOPE_LEN)
}

// ============================================================================
// Admission-seed envelope
// ============================================================================

/// A decoded admission-seed envelope; the seed stays sealed.
#[wasm_bindgen(js_name = AdmissionSeedEnvelope)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmAdmissionSeedEnvelope {
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
    admission_version: u16,
    admission_public_key: Vec<u8>,
}

#[wasm_bindgen(js_class = AdmissionSeedEnvelope)]
impl WasmAdmissionSeedEnvelope {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.channel_id.clone()
    }

    /// The addressee's 32-byte account key.
    #[wasm_bindgen(getter, js_name = memberAccountKey)]
    #[must_use]
    pub fn member_account_key(&self) -> Vec<u8> {
        self.member_account_key.clone()
    }

    /// The admission-key version.
    #[wasm_bindgen(getter, js_name = admissionVersion)]
    #[must_use]
    pub fn admission_version(&self) -> u16 {
        self.admission_version
    }

    /// The 32-byte admission public key.
    #[wasm_bindgen(getter, js_name = admissionPublicKey)]
    #[must_use]
    pub fn admission_public_key(&self) -> Vec<u8> {
        self.admission_public_key.clone()
    }
}

fn admission_seed_error(e: AdmissionSeedError) -> String {
    format!("admissionSeed.{}: {e}", e.code())
}

/// Compose an admission-seed envelope for one co-publisher; send it with
/// `PublisherGroup.createApplicationMessage`.
///
/// # Errors
///
/// Throws `admissionSeed.<code>: ...`.
#[wasm_bindgen(js_name = composeAdmissionSeedEnvelope)]
pub fn compose_admission_seed_envelope(
    channel_id: &[u8],
    member_account_key: &[u8],
    member_x25519_public_key: &[u8],
    admission_version: u16,
    admission_seed: &[u8],
) -> Result<Vec<u8>, String> {
    let channel = <[u8; 16]>::try_from(channel_id)
        .map_err(|_| "admissionSeed.badLength: channel id must be 16 bytes".to_string())?;
    rust_compose_admission_seed(
        &channel,
        &key32("admissionSeed", "member account key", member_account_key)?,
        &x25519("admissionSeed", member_x25519_public_key)?,
        admission_version,
        &key32("admissionSeed", "admission seed", admission_seed)?,
    )
    .map_err(admission_seed_error)
}

/// Strictly decode an admission-seed envelope.
///
/// # Errors
///
/// Throws `admissionSeed.<code>: ...`.
#[wasm_bindgen(js_name = decodeAdmissionSeedEnvelope)]
pub fn decode_admission_seed_envelope(
    plaintext: &[u8],
) -> Result<WasmAdmissionSeedEnvelope, String> {
    let env = rust_decode_admission_seed(plaintext).map_err(admission_seed_error)?;
    Ok(WasmAdmissionSeedEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        admission_version: env.admission_version,
        admission_public_key: env.admission_public_key.to_vec(),
    })
}

/// Open an admission-seed envelope with the ACCOUNT identity it is
/// addressed to, returning the 32-byte admission seed. Also check the MLS
/// sender is the channel's owner.
///
/// # Errors
///
/// Throws `admissionSeed.<code>: ...`.
#[wasm_bindgen(js_name = openAdmissionSeedEnvelope)]
pub fn open_admission_seed_envelope(
    plaintext: &[u8],
    account_identity: &Identity,
) -> Result<Vec<u8>, String> {
    let env = rust_decode_admission_seed(plaintext).map_err(admission_seed_error)?;
    let id = account_identity.mls_identity();
    let own = key32(
        "admissionSeed",
        "own account key",
        id.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err("admissionSeed.otherAccount: addressed to another account".to_string());
    }
    env.open(&X25519SecretKey::from_bytes(id.x25519_secret_bytes()))
        .map(|s| s.to_vec())
        .map_err(admission_seed_error)
}

/// The X25519 public key of an admission seed.
///
/// # Errors
///
/// Throws `admissionSeed.badLength: ...`.
#[wasm_bindgen(js_name = admissionSeedPublicKey)]
pub fn admission_seed_public_key(admission_seed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(
        rust_admission_seed_public_key(&key32("admissionSeed", "admission seed", admission_seed)?)
            .to_vec(),
    )
}

/// The one well-formed admission-seed envelope length.
#[wasm_bindgen(js_name = admissionSeedEnvelopeLen)]
#[must_use]
pub fn admission_seed_envelope_len() -> u32 {
    len_u32(ADMISSION_SEED_ENVELOPE_LEN)
}

// ============================================================================
// Channel read-key envelope (ADR-0087 section 7, phase B)
// ============================================================================

/// A decoded channel read-key envelope; the read key stays sealed.
#[wasm_bindgen(js_name = ChannelReadKeyEnvelope)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmChannelReadKeyEnvelope {
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
}

#[wasm_bindgen(js_class = ChannelReadKeyEnvelope)]
impl WasmChannelReadKeyEnvelope {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.channel_id.clone()
    }

    /// The addressee's 32-byte account key.
    #[wasm_bindgen(getter, js_name = memberAccountKey)]
    #[must_use]
    pub fn member_account_key(&self) -> Vec<u8> {
        self.member_account_key.clone()
    }
}

fn channel_read_key_error(e: ChannelReadKeyError) -> String {
    format!("channelReadKey.{}: {e}", e.code())
}

/// Compose a channel read-key envelope for one co-publisher, sealed to that
/// co-publisher's account X25519 key; send it with
/// `PublisherGroup.createApplicationMessage`.
///
/// # Errors
///
/// Throws `channelReadKey.<code>: ...`.
#[wasm_bindgen(js_name = composeChannelReadKeyEnvelope)]
pub fn compose_channel_read_key_envelope(
    channel_id: &[u8],
    member_account_key: &[u8],
    member_x25519_public_key: &[u8],
    read_key: &[u8],
) -> Result<Vec<u8>, String> {
    let channel = <[u8; 16]>::try_from(channel_id)
        .map_err(|_| "channelReadKey.badLength: channel id must be 16 bytes".to_string())?;
    rust_compose_channel_read_key(
        &channel,
        &key32("channelReadKey", "member account key", member_account_key)?,
        &x25519("channelReadKey", member_x25519_public_key)?,
        &key32("channelReadKey", "read key", read_key)?,
    )
    .map_err(channel_read_key_error)
}

/// Strictly decode a channel read-key envelope.
///
/// # Errors
///
/// Throws `channelReadKey.<code>: ...`.
#[wasm_bindgen(js_name = decodeChannelReadKeyEnvelope)]
pub fn decode_channel_read_key_envelope(
    plaintext: &[u8],
) -> Result<WasmChannelReadKeyEnvelope, String> {
    let env = rust_decode_channel_read_key(plaintext).map_err(channel_read_key_error)?;
    Ok(WasmChannelReadKeyEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
    })
}

/// Open a channel read-key envelope with the ACCOUNT identity it is
/// addressed to, returning the 32-byte read key. Also check the MLS sender
/// is the channel's owner.
///
/// # Errors
///
/// Throws `channelReadKey.<code>: ...`.
#[wasm_bindgen(js_name = openChannelReadKeyEnvelope)]
pub fn open_channel_read_key_envelope(
    plaintext: &[u8],
    account_identity: &Identity,
) -> Result<Vec<u8>, String> {
    let env = rust_decode_channel_read_key(plaintext).map_err(channel_read_key_error)?;
    let id = account_identity.mls_identity();
    let own = key32(
        "channelReadKey",
        "own account key",
        id.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err("channelReadKey.otherAccount: addressed to another account".to_string());
    }
    env.open(&X25519SecretKey::from_bytes(id.x25519_secret_bytes()))
        .map(|k| k.to_vec())
        .map_err(channel_read_key_error)
}

/// The one well-formed channel read-key envelope length.
#[wasm_bindgen(js_name = channelReadKeyEnvelopeLen)]
#[must_use]
pub fn channel_read_key_envelope_len() -> u32 {
    len_u32(CHANNEL_READ_KEY_ENVELOPE_LEN)
}

// ============================================================================
// Earlier channel seeds (channel owner -> new publisher, ADR-0087 section 7
// as amended 2026-11-08)
// ============================================================================

/// A decoded epoch-seeds envelope; the seeds stay sealed.
#[wasm_bindgen(js_name = ChannelEpochSeedsEnvelope)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmChannelEpochSeedsEnvelope {
    channel_id: Vec<u8>,
    member_account_key: Vec<u8>,
    count: u32,
}

#[wasm_bindgen(js_class = ChannelEpochSeedsEnvelope)]
impl WasmChannelEpochSeedsEnvelope {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.channel_id.clone()
    }

    /// The addressee's 32-byte account key.
    #[wasm_bindgen(getter, js_name = memberAccountKey)]
    #[must_use]
    pub fn member_account_key(&self) -> Vec<u8> {
        self.member_account_key.clone()
    }

    /// How many seeds the envelope carries.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }
}

/// Opened seeds, in ascending epoch order: `epochs[i]` is served by
/// `seeds[32 * i .. 32 * (i + 1)]`. Secret.
#[wasm_bindgen(js_name = ChannelEpochSeeds)]
pub struct WasmChannelEpochSeeds {
    epochs: Vec<u32>,
    seeds: Vec<u8>,
}

#[wasm_bindgen(js_class = ChannelEpochSeeds)]
impl WasmChannelEpochSeeds {
    /// The channel epoch ids, ascending.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn epochs(&self) -> Vec<u32> {
        self.epochs.clone()
    }

    /// The 32-byte seeds, concatenated in the order of `epochs`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seeds(&self) -> Vec<u8> {
        self.seeds.clone()
    }
}

impl Drop for WasmChannelEpochSeeds {
    fn drop(&mut self) {
        self.seeds.fill(0);
    }
}

fn channel_epoch_seeds_error(e: ChannelEpochSeedsError) -> String {
    format!("channelEpochSeeds.{}: {e}", e.code())
}

/// Compose an epoch-seeds envelope for one new publisher, sealed to that
/// publisher's account X25519 key; send it with
/// `PublisherGroup.createApplicationMessage` beside the read key. `epochs`
/// and `seeds` are parallel: `seeds` is `32 * epochs.length` bytes, the seed
/// of `epochs[i]` at `32 * i`. Any order; 1 to `channelEpochSeedsMax()`
/// entries, no epoch twice.
///
/// # Errors
///
/// Throws `channelEpochSeeds.<code>: ...` (`badCount`, `duplicateEpoch`,
/// `badLength`).
#[wasm_bindgen(js_name = composeChannelEpochSeedsEnvelope)]
pub fn compose_channel_epoch_seeds_envelope(
    channel_id: &[u8],
    member_account_key: &[u8],
    member_x25519_public_key: &[u8],
    epochs: Vec<u32>,
    seeds: &[u8],
) -> Result<Vec<u8>, String> {
    let channel = <[u8; 16]>::try_from(channel_id)
        .map_err(|_| "channelEpochSeeds.badLength: channel id must be 16 bytes".to_string())?;
    if epochs.len().checked_mul(CHANNEL_EPOCH_SEED_LEN) != Some(seeds.len()) {
        return Err("channelEpochSeeds.badLength: seeds must be 32 bytes per epoch".to_string());
    }
    let list = epochs
        .iter()
        .zip(seeds.chunks_exact(CHANNEL_EPOCH_SEED_LEN))
        .map(|(&epoch, chunk)| {
            <[u8; CHANNEL_EPOCH_SEED_LEN]>::try_from(chunk)
                .map(|seed| ChannelEpochSeed { epoch, seed })
                .map_err(|_| "channelEpochSeeds.badLength: a seed must be 32 bytes".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    rust_compose_channel_epoch_seeds(
        &channel,
        &key32(
            "channelEpochSeeds",
            "member account key",
            member_account_key,
        )?,
        &x25519("channelEpochSeeds", member_x25519_public_key)?,
        &list,
    )
    .map_err(channel_epoch_seeds_error)
}

/// Strictly decode an epoch-seeds envelope.
///
/// # Errors
///
/// Throws `channelEpochSeeds.<code>: ...`.
#[wasm_bindgen(js_name = decodeChannelEpochSeedsEnvelope)]
pub fn decode_channel_epoch_seeds_envelope(
    plaintext: &[u8],
) -> Result<WasmChannelEpochSeedsEnvelope, String> {
    let env = rust_decode_channel_epoch_seeds(plaintext).map_err(channel_epoch_seeds_error)?;
    Ok(WasmChannelEpochSeedsEnvelope {
        channel_id: env.channel_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        count: u32::from(env.count),
    })
}

/// Open an epoch-seeds envelope with the ACCOUNT identity it is addressed
/// to. The caller also checks it came on this channel's PublisherGroup and
/// the MLS sender resolved at receipt to the channel's owner, then adds only
/// the seeds of epochs it holds none of.
///
/// # Errors
///
/// Throws `channelEpochSeeds.<code>: ...` (`otherAccount` when addressed to
/// another account).
#[wasm_bindgen(js_name = openChannelEpochSeedsEnvelope)]
pub fn open_channel_epoch_seeds_envelope(
    plaintext: &[u8],
    account_identity: &Identity,
) -> Result<WasmChannelEpochSeeds, String> {
    let env = rust_decode_channel_epoch_seeds(plaintext).map_err(channel_epoch_seeds_error)?;
    let id = account_identity.mls_identity();
    let own = key32(
        "channelEpochSeeds",
        "own account key",
        id.signature_public_key(),
    )?;
    if !env.is_addressed_to(&own) {
        return Err("channelEpochSeeds.otherAccount: addressed to another account".to_string());
    }
    let opened = env
        .open(&X25519SecretKey::from_bytes(id.x25519_secret_bytes()))
        .map_err(channel_epoch_seeds_error)?;
    let mut seeds = Vec::with_capacity(opened.len() * CHANNEL_EPOCH_SEED_LEN);
    for s in &opened {
        seeds.extend_from_slice(&s.seed);
    }
    Ok(WasmChannelEpochSeeds {
        epochs: opened.iter().map(|s| s.epoch).collect(),
        seeds,
    })
}

/// Most seeds one epoch-seeds envelope carries (64).
#[wasm_bindgen(js_name = channelEpochSeedsMax)]
#[must_use]
pub fn channel_epoch_seeds_max() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_MAX)
}

/// Length of an epoch-seeds envelope carrying one seed.
#[wasm_bindgen(js_name = channelEpochSeedsEnvelopeMinLen)]
#[must_use]
pub fn channel_epoch_seeds_envelope_min_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN)
}

/// Length of an epoch-seeds envelope carrying the most seeds.
#[wasm_bindgen(js_name = channelEpochSeedsEnvelopeMaxLen)]
#[must_use]
pub fn channel_epoch_seeds_envelope_max_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN)
}

// ============================================================================
// A publisher asks the owner to rotate (ADR-0087 section 15 as amended
// 2026-11-08)
// ============================================================================

fn channel_rotate_request_error(e: ChannelRotateRequestError) -> String {
    format!("channelRotateRequest.{}: {e}", e.code())
}

/// Compose the request a publisher sends on the channel's PublisherGroup
/// with `PublisherGroup.createApplicationMessage` after removing a
/// subscriber: the owner's device rotates at its next publisher pass.
///
/// # Errors
///
/// Throws `channelRotateRequest.badLength: ...` when the channel id is not
/// 16 bytes.
#[wasm_bindgen(js_name = composeChannelRotateRequestEnvelope)]
pub fn compose_channel_rotate_request_envelope(channel_id: &[u8]) -> Result<Vec<u8>, String> {
    let channel = <[u8; 16]>::try_from(channel_id)
        .map_err(|_| "channelRotateRequest.badLength: channel id must be 16 bytes".to_string())?;
    Ok(rust_compose_channel_rotate_request(&channel))
}

/// Strictly decode a rotate request, returning the 16-byte channel id. Only
/// the channel's owner acts on it, and only when it came on that channel's
/// PublisherGroup and the MLS sender is a current publisher.
///
/// # Errors
///
/// Throws `channelRotateRequest.<code>: ...`.
#[wasm_bindgen(js_name = decodeChannelRotateRequestEnvelope)]
pub fn decode_channel_rotate_request_envelope(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    rust_decode_channel_rotate_request(plaintext)
        .map(|c| c.to_vec())
        .map_err(channel_rotate_request_error)
}

/// The one well-formed rotate-request length (49).
#[wasm_bindgen(js_name = channelRotateRequestEnvelopeLen)]
#[must_use]
pub fn channel_rotate_request_envelope_len() -> u32 {
    len_u32(CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::reach::{ReachClass, ReachGrant as RustReachGrant};

    #[test]
    fn channel_rotate_request_round_trips() {
        let wire = compose_channel_rotate_request_envelope(&[0x0C; 16]).unwrap();
        assert_eq!(wire.len(), channel_rotate_request_envelope_len() as usize);
        assert_eq!(
            decode_channel_rotate_request_envelope(&wire).unwrap(),
            vec![0x0C; 16]
        );
        assert!(compose_channel_rotate_request_envelope(&[0; 15])
            .is_err_and(|e| e.starts_with("channelRotateRequest.badLength")));
    }

    #[test]
    fn channel_epoch_seeds_round_trip_flat() {
        let member = Identity::from_seed(&[0x71; 64]).unwrap();
        let id = member.mls_identity();
        let mut seeds = vec![7_u8; 32];
        seeds.extend_from_slice(&[1; 32]);
        let wire = compose_channel_epoch_seeds_envelope(
            &[0x0C; 16],
            id.signature_public_key(),
            &X25519SecretKey::from_bytes(id.x25519_secret_bytes())
                .public_key()
                .to_bytes(),
            vec![7, 1],
            &seeds,
        )
        .unwrap();
        assert_eq!(
            decode_channel_epoch_seeds_envelope(&wire).unwrap().count(),
            2
        );
        let opened = open_channel_epoch_seeds_envelope(&wire, &member).unwrap();
        assert_eq!(opened.epochs(), vec![1, 7]);
        assert_eq!(&opened.seeds()[..32], &[1; 32]);
        assert_eq!(&opened.seeds()[32..], &[7; 32]);
        let other = Identity::from_seed(&[0x72; 64]).unwrap();
        assert!(open_channel_epoch_seeds_envelope(&wire, &other)
            .is_err_and(|e| e.starts_with("channelEpochSeeds.otherAccount")));
        assert!(compose_channel_epoch_seeds_envelope(
            &[0x0C; 16],
            id.signature_public_key(),
            &[9; 32],
            vec![1],
            &[0; 31],
        )
        .is_err_and(|e| e.starts_with("channelEpochSeeds.badLength")));
    }

    #[test]
    fn knock_row_and_seed_envelopes_round_trip() {
        let seed = group_knock_seed(&[0x11; 32], 3).unwrap();
        let public = group_knock_public_key(&seed).unwrap();
        let grant = ReachGrant::from_rust(
            RustReachGrant::derive(&[0x71; 32], ReachClass::Reply, &[0x0A; 16], 0).unwrap(),
        );
        let row = seal_knock_row(&public, 3, &[0xAB; 300], &grant, &[], None).unwrap();
        assert_eq!(knock_row_version(&row), Some(3));
        assert!(is_knock_row_len(u32::try_from(row.len()).unwrap()));
        assert_eq!(
            open_knock_row(&seed, &row).unwrap().key_package(),
            vec![0xAB; 300]
        );
        let other = group_knock_seed(&[0x12; 32], 3).unwrap();
        assert!(open_knock_row(&other, &row).is_err_and(|e| e.starts_with("knock.notOpened")));

        let admin = Identity::from_seed(&[0x71; 64]).unwrap();
        let id = admin.mls_identity();
        let wire = compose_knock_seed_envelope(
            &[0x4A; 32],
            id.signature_public_key(),
            &id.x25519_public_key(),
            1,
            &seed,
        )
        .unwrap();
        assert_eq!(wire.len(), knock_seed_envelope_len() as usize);
        assert_eq!(
            decode_knock_seed_envelope(&wire)
                .unwrap()
                .knock_public_key(),
            public
        );
        assert_eq!(open_knock_seed_envelope(&wire, &admin).unwrap(), seed);

        let admission = [0x5C_u8; 32];
        let wire = compose_admission_seed_envelope(
            &[0x0C; 16],
            id.signature_public_key(),
            &id.x25519_public_key(),
            2,
            &admission,
        )
        .unwrap();
        assert_eq!(
            decode_admission_seed_envelope(&wire)
                .unwrap()
                .admission_version(),
            2
        );
        assert_eq!(
            open_admission_seed_envelope(&wire, &admin).unwrap(),
            admission.to_vec()
        );
        assert_eq!(wire.len(), admission_seed_envelope_len() as usize);

        let read_key = [0x5A_u8; 32];
        let wire = compose_channel_read_key_envelope(
            &[0x0C; 16],
            id.signature_public_key(),
            &id.x25519_public_key(),
            &read_key,
        )
        .unwrap();
        assert_eq!(wire.len(), channel_read_key_envelope_len() as usize);
        assert_eq!(
            decode_channel_read_key_envelope(&wire)
                .unwrap()
                .channel_id(),
            vec![0x0C; 16]
        );
        assert_eq!(
            open_channel_read_key_envelope(&wire, &admin).unwrap(),
            read_key.to_vec()
        );
        let other = Identity::from_seed(&[0x72; 64]).unwrap();
        assert!(open_channel_read_key_envelope(&wire, &other)
            .is_err_and(|e| e.starts_with("channelReadKey.otherAccount")));
    }
}
