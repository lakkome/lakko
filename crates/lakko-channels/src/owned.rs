// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's list of the channels it owns, and the locks that keep its
//! list key ([ADR-0101] section 7, rewritten and settled 2026-11-26).
//!
//! The owner key of a channel is derived from the account seed, so a key
//! reset loses it. The account therefore keeps, on the storage service, a
//! **list** of the channels it owns, sealed under a random 32-byte **list
//! key** `R`, and each channel's recovery key comes from `R`
//! ([`crate::recovery::ChannelRecoveryKey::from_list_key`]). `R` is kept under
//! locks, each an ADR-0064 vault named by a root of its own
//! (`lakko_vault::VaultRoot::from_root` over reference tag `0x04`):
//!
//! ```text
//! password lock  root = BLAKE3-derive-key("lakko/channels/owned-lock-password/v1", export_key(64) ‖ S(32))
//! keys lock      root = BLAKE3-derive-key("lakko/channels/owned-lock-keys/v1",     lineage_seed(32))
//! passkey lock   root = BLAKE3-derive-key("lakko/channels/owned-lock-passkey/v1",  prf_output(32) ‖ S(32))
//! list           root = R
//! ```
//!
//! `export_key` is the OPAQUE export key of the account's password, `S` the
//! server half `services/auth` releases only at sign-up and against a reset
//! factor grant, `lineage_seed` the seed ADR-0078 phase 6 names the
//! DeviceGroup with, `prf_output` the passkey's WebAuthn `prf` output (the
//! one its reset seal key comes from).
//!
//! ## Lock heads
//!
//! ```text
//! password lock, passkey lock   R(32)
//! keys lock                     R(32) ‖ S(32)
//! ```
//!
//! Exact lengths, no version byte: the vault object around them carries the
//! version and binds the vault ([ADR-0101] section 7 item 4, as written).
//!
//! ## The list
//!
//! ```text
//! list    = version(1)=0x02 ‖ password_lock_root(32)
//!           ‖ passkey_lock_count(1, 0..=8) ‖ passkey_lock_count × (tag(16) ‖ root(32))
//!           ‖ count(1, 0..=64) ‖ count × entry ‖ zero padding
//! entry   = channel_id(16) ‖ read_key_len(1, 0|32) ‖ read_key
//!           ‖ handle_seed_len(1, 0|32) ‖ handle_seed
//!           ‖ admission_count(1, 0..=8) ‖ admission_count × (admission_version(4 BE) ‖ seed(32))
//!           ‖ epoch_count(1, 0..=4) ‖ epoch_count × (epoch_id(4 BE) ‖ seed(32))
//! ```
//!
//! **Chosen 2026-11-26 (recorded in the ADR's as-built note):**
//!
//! - The list is always exactly [`OWNED_CHANNELS_LIST_LEN`] = 65 532 bytes,
//!   the content followed by zero bytes, so that with the vault's 4-byte
//!   length frame it fills the 64 KiB bucket exactly: every list is sealed to
//!   the one size the ADR names. Decode refuses any other length and any
//!   non-zero padding byte. (Version 1 was 32 764 bytes for the 32 KiB
//!   bucket; the handle seed, decided 2026-11-27, does not fit there: the
//!   fullest list is 33 443 bytes.)
//! - Each entry carries the channel's ADR-0087 handle write seed
//!   (`handle_seed = BLAKE3-derive-key("lakko/handle/write/v1",
//!   journal_root_seed)` of the account that owned the channel when it was
//!   listed), so a device that retakes the channel after a key reset can sign
//!   `rekey` from the old handle key to the one its new root derives. Absent
//!   for a channel with no `@name`.
//! - Canonical order, refused otherwise: passkey locks strictly ascending by
//!   tag, entries strictly ascending by channel id, admission seeds strictly
//!   ascending by `(admission_version, seed)` (ADR-0090 lets two seeds share
//!   a version), epoch seeds strictly ascending by `epoch_id`.
//! - [`OwnedChannelsList::merge`] is the ADR's merge after a `409`: by
//!   channel id, newer epochs and admission versions kept, a deleted channel
//!   dropped; on a tie the writer's own value wins, and past 64 entries the
//!   writer's own are kept first.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use lakko_crypto::hash;
use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of the password lock root. Frozen.
pub const OWNED_LOCK_PASSWORD_CONTEXT: &str = "lakko/channels/owned-lock-password/v1";
/// BLAKE3 derive-key context of the keys lock root. Frozen.
pub const OWNED_LOCK_KEYS_CONTEXT: &str = "lakko/channels/owned-lock-keys/v1";
/// BLAKE3 derive-key context of a passkey lock root. Frozen.
pub const OWNED_LOCK_PASSKEY_CONTEXT: &str = "lakko/channels/owned-lock-passkey/v1";

/// Length of an OPAQUE export key.
pub const EXPORT_KEY_LEN: usize = 64;
/// Length of the server half `S`, the list key `R`, a lock root, a seed.
pub const KEY_LEN: usize = 32;
/// Length of a passkey lock's tag (the passkey's tag in the seal
/// announcement).
pub const PASSKEY_TAG_LEN: usize = 16;

/// The list's version byte. Version 1 (no handle seed, 32 764 bytes) is
/// refused; nothing was deployed with it.
pub const OWNED_CHANNELS_VERSION: u8 = 0x02;
/// Most passkey locks.
pub const MAX_PASSKEY_LOCKS: usize = 8;
/// Most channels listed.
pub const MAX_OWNED_CHANNELS: usize = 64;
/// Most admission seeds per channel.
pub const MAX_ADMISSION_SEEDS: usize = 8;
/// Most epoch seeds per channel (the ADR-0031 retention).
pub const MAX_EPOCH_SEEDS: usize = 4;

/// Length of a password or passkey lock head (`R`).
pub const LIST_KEY_LOCK_LEN: usize = KEY_LEN;
/// Length of the keys lock head (`R ‖ S`).
pub const KEYS_LOCK_LEN: usize = 2 * KEY_LEN;

/// The exact length of an encoded list: 64 KiB less the vault's 4-byte
/// length frame, so the sealed object is the 64 KiB bucket (the largest
/// power-of-two bucket of `lakko_vault::seal`). Chosen.
pub const OWNED_CHANNELS_LIST_LEN: usize = 64 * 1024 - 4;

const PASSKEY_LOCK_LEN: usize = PASSKEY_TAG_LEN + KEY_LEN;
const SEED_ROW_LEN: usize = 4 + KEY_LEN;
/// Longest entry: 516 bytes.
pub const MAX_ENTRY_LEN: usize = CHANNEL_ID_LEN
    + 1
    + KEY_LEN
    + 1
    + KEY_LEN
    + 1
    + MAX_ADMISSION_SEEDS * SEED_ROW_LEN
    + 1
    + MAX_EPOCH_SEEDS * SEED_ROW_LEN;
/// Longest content before the padding.
pub const MAX_CONTENT_LEN: usize =
    1 + KEY_LEN + 1 + MAX_PASSKEY_LOCKS * PASSKEY_LOCK_LEN + 1 + MAX_OWNED_CHANNELS * MAX_ENTRY_LEN;

const _: () = assert!(MAX_ENTRY_LEN == 516);
const _: () = assert!(MAX_CONTENT_LEN <= OWNED_CHANNELS_LIST_LEN);

/// Why a list or a lock head did not encode or decode.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum OwnedChannelsError {
    /// Not the exact length the format has.
    #[error("wrong length")]
    BadLength,
    /// The version byte is not `2`.
    #[error("unknown version")]
    UnknownVersion,
    /// A count is above its bound.
    #[error("a count is above its bound")]
    TooMany,
    /// A read key length is neither 0 nor 32.
    #[error("read key length is neither 0 nor 32")]
    BadReadKey,
    /// A handle seed length is neither 0 nor 32.
    #[error("handle seed length is neither 0 nor 32")]
    BadHandleSeed,
    /// Items are out of order or repeated.
    #[error("items out of order or repeated")]
    NotCanonical,
    /// The content runs past the list, or a padding byte is not zero.
    #[error("bad padding")]
    BadPadding,
}

impl OwnedChannelsError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion => "unknownVersion",
            Self::TooMany => "tooMany",
            Self::BadReadKey => "badReadKey",
            Self::BadHandleSeed => "badHandleSeed",
            Self::NotCanonical => "notCanonical",
            Self::BadPadding => "badPadding",
        }
    }
}

fn derive(context: &str, parts: &[&[u8]]) -> SecretBytes<KEY_LEN> {
    let mut material = [0u8; EXPORT_KEY_LEN + KEY_LEN];
    let mut len = 0usize;
    for part in parts {
        for (slot, byte) in material[len..].iter_mut().zip(part.iter()) {
            *slot = *byte;
        }
        len = len.saturating_add(part.len()).min(material.len());
    }
    let scratch = SecretBytes::new(material);
    SecretBytes::new(hash::blake3_derive_key_from_material(
        context,
        &scratch.as_bytes()[..len],
    ))
}

/// The password lock's root from the password's OPAQUE export key and the
/// server half `S`.
#[must_use]
pub fn password_lock_root(
    export_key: &[u8; EXPORT_KEY_LEN],
    server_half: &[u8; KEY_LEN],
) -> SecretBytes<KEY_LEN> {
    derive(OWNED_LOCK_PASSWORD_CONTEXT, &[export_key, server_half])
}

/// The keys lock's root from the account's lineage seed.
#[must_use]
pub fn keys_lock_root(lineage_seed: &[u8; KEY_LEN]) -> SecretBytes<KEY_LEN> {
    derive(OWNED_LOCK_KEYS_CONTEXT, &[lineage_seed])
}

/// A passkey lock's root from the passkey's WebAuthn `prf` output and the
/// server half `S`.
#[must_use]
pub fn passkey_lock_root(
    prf_output: &[u8; KEY_LEN],
    server_half: &[u8; KEY_LEN],
) -> SecretBytes<KEY_LEN> {
    derive(OWNED_LOCK_PASSKEY_CONTEXT, &[prf_output, server_half])
}

/// The head of a password or passkey lock: `R`.
#[must_use]
pub fn encode_list_key_lock(list_key: &[u8; KEY_LEN]) -> [u8; LIST_KEY_LOCK_LEN] {
    *list_key
}

/// Read a password or passkey lock head.
///
/// # Errors
///
/// [`OwnedChannelsError::BadLength`].
pub fn decode_list_key_lock(bytes: &[u8]) -> Result<SecretBytes<KEY_LEN>, OwnedChannelsError> {
    let r: [u8; KEY_LEN] = bytes
        .try_into()
        .map_err(|_| OwnedChannelsError::BadLength)?;
    Ok(SecretBytes::new(r))
}

/// The head of the keys lock: `R ‖ S`.
#[must_use]
pub fn encode_keys_lock(
    list_key: &[u8; KEY_LEN],
    server_half: &[u8; KEY_LEN],
) -> [u8; KEYS_LOCK_LEN] {
    let mut out = [0u8; KEYS_LOCK_LEN];
    out[..KEY_LEN].copy_from_slice(list_key);
    out[KEY_LEN..].copy_from_slice(server_half);
    out
}

/// Read the keys lock head: `(R, S)`.
///
/// # Errors
///
/// [`OwnedChannelsError::BadLength`].
pub fn decode_keys_lock(
    bytes: &[u8],
) -> Result<(SecretBytes<KEY_LEN>, SecretBytes<KEY_LEN>), OwnedChannelsError> {
    if bytes.len() != KEYS_LOCK_LEN {
        return Err(OwnedChannelsError::BadLength);
    }
    let mut r = [0u8; KEY_LEN];
    let mut s = [0u8; KEY_LEN];
    r.copy_from_slice(&bytes[..KEY_LEN]);
    s.copy_from_slice(&bytes[KEY_LEN..]);
    Ok((SecretBytes::new(r), SecretBytes::new(s)))
}

/// One passkey lock the list names.
#[derive(Clone, PartialEq, Eq)]
pub struct PasskeyLock {
    /// The passkey's tag in the seal announcement.
    pub tag: [u8; PASSKEY_TAG_LEN],
    /// The lock's root.
    pub root: [u8; KEY_LEN],
}

/// One admission seed of a private channel (ADR-0090 decision A).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdmissionSeed {
    /// The admission version the seed belongs to.
    pub version: u32,
    /// The seed.
    pub seed: [u8; KEY_LEN],
}

/// One retained epoch seed (ADR-0031).
#[derive(Clone, PartialEq, Eq)]
pub struct EpochSeed {
    /// The epoch.
    pub epoch_id: u32,
    /// The seed.
    pub seed: [u8; KEY_LEN],
}

/// One owned channel.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnedChannel {
    /// The channel id.
    pub channel_id: [u8; CHANNEL_ID_LEN],
    /// The read key of a public channel.
    pub read_key: Option<[u8; KEY_LEN]>,
    /// The channel's ADR-0087 handle write seed, when it has an `@name`.
    pub handle_seed: Option<[u8; KEY_LEN]>,
    /// Admission seeds, ascending by `(version, seed)`, at most eight.
    pub admission: Vec<AdmissionSeed>,
    /// Epoch seeds, ascending by epoch, at most four.
    pub epochs: Vec<EpochSeed>,
}

/// The decoded list.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnedChannelsList {
    /// The current password lock's root.
    pub password_lock_root: [u8; KEY_LEN],
    /// The passkey locks, ascending by tag, at most eight.
    pub passkey_locks: Vec<PasskeyLock>,
    /// The channels, ascending by id, at most 64.
    pub channels: Vec<OwnedChannel>,
}

impl core::fmt::Debug for PasskeyLock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PasskeyLock")
            .field("tag", &self.tag)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for AdmissionSeed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AdmissionSeed")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for EpochSeed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EpochSeed")
            .field("epoch_id", &self.epoch_id)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for OwnedChannel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedChannel")
            .field("channel_id", &self.channel_id)
            .field("has_read_key", &self.read_key.is_some())
            .field("has_handle_seed", &self.handle_seed.is_some())
            .field("admission", &self.admission)
            .field("epochs", &self.epochs)
            .finish()
    }
}

impl core::fmt::Debug for OwnedChannelsList {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedChannelsList")
            .field("passkey_locks", &self.passkey_locks)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

fn strictly_ascending<T, K: Ord>(items: &[T], key: impl Fn(&T) -> K) -> bool {
    items.windows(2).all(|w| key(&w[0]) < key(&w[1]))
}

impl OwnedChannel {
    fn check(&self) -> Result<(), OwnedChannelsError> {
        if self.admission.len() > MAX_ADMISSION_SEEDS || self.epochs.len() > MAX_EPOCH_SEEDS {
            return Err(OwnedChannelsError::TooMany);
        }
        if !strictly_ascending(&self.admission, |a| (a.version, a.seed))
            || !strictly_ascending(&self.epochs, |e| e.epoch_id)
        {
            return Err(OwnedChannelsError::NotCanonical);
        }
        Ok(())
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_id);
        match &self.read_key {
            Some(k) => {
                out.push(32);
                out.extend_from_slice(k);
            },
            None => out.push(0),
        }
        match &self.handle_seed {
            Some(k) => {
                out.push(32);
                out.extend_from_slice(k);
            },
            None => out.push(0),
        }
        // `check` bounds both counts before this runs.
        out.push(u8::try_from(self.admission.len()).unwrap_or(u8::MAX));
        for a in &self.admission {
            out.extend_from_slice(&a.version.to_be_bytes());
            out.extend_from_slice(&a.seed);
        }
        out.push(u8::try_from(self.epochs.len()).unwrap_or(u8::MAX));
        for e in &self.epochs {
            out.extend_from_slice(&e.epoch_id.to_be_bytes());
            out.extend_from_slice(&e.seed);
        }
    }
}

impl OwnedChannelsList {
    fn check(&self) -> Result<(), OwnedChannelsError> {
        if self.passkey_locks.len() > MAX_PASSKEY_LOCKS || self.channels.len() > MAX_OWNED_CHANNELS
        {
            return Err(OwnedChannelsError::TooMany);
        }
        if !strictly_ascending(&self.passkey_locks, |l| l.tag)
            || !strictly_ascending(&self.channels, |c| c.channel_id)
        {
            return Err(OwnedChannelsError::NotCanonical);
        }
        for c in &self.channels {
            c.check()?;
        }
        Ok(())
    }

    /// Encode to exactly [`OWNED_CHANNELS_LIST_LEN`] bytes.
    ///
    /// # Errors
    ///
    /// [`OwnedChannelsError::TooMany`] or [`OwnedChannelsError::NotCanonical`]
    /// (sort the items first; [`Self::merge`] returns a sorted list).
    pub fn encode(&self) -> Result<Vec<u8>, OwnedChannelsError> {
        self.check()?;
        let mut out = Vec::with_capacity(OWNED_CHANNELS_LIST_LEN);
        out.push(OWNED_CHANNELS_VERSION);
        out.extend_from_slice(&self.password_lock_root);
        out.push(u8::try_from(self.passkey_locks.len()).unwrap_or(u8::MAX));
        for l in &self.passkey_locks {
            out.extend_from_slice(&l.tag);
            out.extend_from_slice(&l.root);
        }
        out.push(u8::try_from(self.channels.len()).unwrap_or(u8::MAX));
        for c in &self.channels {
            c.encode_into(&mut out);
        }
        out.resize(OWNED_CHANNELS_LIST_LEN, 0);
        Ok(out)
    }

    /// Decode an opened list. Bounded, exact and fail-closed.
    ///
    /// # Errors
    ///
    /// Every [`OwnedChannelsError`].
    pub fn decode(bytes: &[u8]) -> Result<Self, OwnedChannelsError> {
        if bytes.len() != OWNED_CHANNELS_LIST_LEN {
            return Err(OwnedChannelsError::BadLength);
        }
        let mut cur = Reader { bytes, at: 0 };
        if cur.byte()? != OWNED_CHANNELS_VERSION {
            return Err(OwnedChannelsError::UnknownVersion);
        }
        let password_lock_root = cur.array::<KEY_LEN>()?;
        let lock_count = usize::from(cur.byte()?);
        if lock_count > MAX_PASSKEY_LOCKS {
            return Err(OwnedChannelsError::TooMany);
        }
        let mut passkey_locks = Vec::with_capacity(lock_count);
        for _ in 0..lock_count {
            passkey_locks.push(PasskeyLock {
                tag: cur.array()?,
                root: cur.array()?,
            });
        }
        let count = usize::from(cur.byte()?);
        if count > MAX_OWNED_CHANNELS {
            return Err(OwnedChannelsError::TooMany);
        }
        let mut channels = Vec::with_capacity(count);
        for _ in 0..count {
            let channel_id = cur.array::<CHANNEL_ID_LEN>()?;
            let read_key = match cur.byte()? {
                0 => None,
                32 => Some(cur.array::<KEY_LEN>()?),
                _ => return Err(OwnedChannelsError::BadReadKey),
            };
            let handle_seed = match cur.byte()? {
                0 => None,
                32 => Some(cur.array::<KEY_LEN>()?),
                _ => return Err(OwnedChannelsError::BadHandleSeed),
            };
            let n = usize::from(cur.byte()?);
            if n > MAX_ADMISSION_SEEDS {
                return Err(OwnedChannelsError::TooMany);
            }
            let mut admission = Vec::with_capacity(n);
            for _ in 0..n {
                admission.push(AdmissionSeed {
                    version: cur.u32()?,
                    seed: cur.array()?,
                });
            }
            let n = usize::from(cur.byte()?);
            if n > MAX_EPOCH_SEEDS {
                return Err(OwnedChannelsError::TooMany);
            }
            let mut epochs = Vec::with_capacity(n);
            for _ in 0..n {
                epochs.push(EpochSeed {
                    epoch_id: cur.u32()?,
                    seed: cur.array()?,
                });
            }
            channels.push(OwnedChannel {
                channel_id,
                read_key,
                handle_seed,
                admission,
                epochs,
            });
        }
        if bytes[cur.at..].iter().any(|b| *b != 0) {
            return Err(OwnedChannelsError::BadPadding);
        }
        let list = Self {
            password_lock_root,
            passkey_locks,
            channels,
        };
        list.check()?;
        Ok(list)
    }

    /// The channel `channel_id`, when listed.
    #[must_use]
    pub fn channel(&self, channel_id: &[u8; CHANNEL_ID_LEN]) -> Option<&OwnedChannel> {
        self.channels.iter().find(|c| &c.channel_id == channel_id)
    }

    /// Merge after a `409 vault-version-conflict`: `self` is what this device
    /// wanted to write, `theirs` what it read back, `deleted` the channels
    /// this device deleted. By channel id: admission seeds unioned, the eight
    /// highest kept; epoch seeds unioned by epoch (this device's seed wins on
    /// the same epoch), the four highest kept; a read key and a handle seed
    /// each kept from either side, this device's first. The password lock root is this device's;
    /// passkey locks unioned by tag, this device's first, at most eight.
    /// Past 64 channels this device's are kept first. The result is sorted.
    #[must_use]
    pub fn merge(&self, theirs: &Self, deleted: &[[u8; CHANNEL_ID_LEN]]) -> Self {
        let mut locks: Vec<PasskeyLock> = self.passkey_locks.clone();
        for l in &theirs.passkey_locks {
            if !locks.iter().any(|m| m.tag == l.tag) {
                locks.push(l.clone());
            }
        }
        locks.truncate(MAX_PASSKEY_LOCKS);
        locks.sort_by_key(|l| l.tag);

        let mut channels: Vec<OwnedChannel> = Vec::new();
        let kept = |c: &OwnedChannel| !deleted.contains(&c.channel_id);
        for mine in self.channels.iter().filter(|c| kept(c)) {
            let merged = theirs
                .channel(&mine.channel_id)
                .map_or_else(|| mine.clone(), |other| merge_channel(mine, other));
            channels.push(merged);
        }
        for other in theirs.channels.iter().filter(|c| kept(c)) {
            if channels.len() >= MAX_OWNED_CHANNELS {
                break;
            }
            if !channels.iter().any(|c| c.channel_id == other.channel_id) {
                channels.push(other.clone());
            }
        }
        channels.truncate(MAX_OWNED_CHANNELS);
        channels.sort_by_key(|c| c.channel_id);
        Self {
            password_lock_root: self.password_lock_root,
            passkey_locks: locks,
            channels,
        }
    }
}

fn merge_channel(mine: &OwnedChannel, theirs: &OwnedChannel) -> OwnedChannel {
    let mut admission: Vec<AdmissionSeed> = mine.admission.clone();
    for a in &theirs.admission {
        if !admission.contains(a) {
            admission.push(a.clone());
        }
    }
    admission.sort();
    let skip = admission.len().saturating_sub(MAX_ADMISSION_SEEDS);
    let admission = admission.split_off(skip);

    let mut epochs: Vec<EpochSeed> = mine.epochs.clone();
    for e in &theirs.epochs {
        if !epochs.iter().any(|m| m.epoch_id == e.epoch_id) {
            epochs.push(e.clone());
        }
    }
    epochs.sort_by_key(|e| e.epoch_id);
    let skip = epochs.len().saturating_sub(MAX_EPOCH_SEEDS);
    let epochs = epochs.split_off(skip);

    OwnedChannel {
        channel_id: mine.channel_id,
        read_key: mine.read_key.or(theirs.read_key),
        handle_seed: mine.handle_seed.or(theirs.handle_seed),
        admission,
        epochs,
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], OwnedChannelsError> {
        let end = self
            .at
            .checked_add(n)
            .ok_or(OwnedChannelsError::BadPadding)?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(OwnedChannelsError::BadPadding)?;
        self.at = end;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, OwnedChannelsError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], OwnedChannelsError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, OwnedChannelsError> {
        Ok(u32::from_be_bytes(self.array::<4>()?))
    }
}

/// Fuzz oracle: decode `bytes` as a list; when it decodes, it must re-encode
/// to the identical bytes. `Some(true)` when that holds, `None` when it did
/// not decode.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let list = OwnedChannelsList::decode(bytes).ok()?;
    Some(list.encode().ok().as_deref() == Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn full_channel(id: u8) -> OwnedChannel {
        OwnedChannel {
            channel_id: [id; 16],
            read_key: Some([id; 32]),
            handle_seed: Some([id ^ 0x55; 32]),
            admission: (0..8u32)
                .map(|v| AdmissionSeed {
                    version: v,
                    seed: [id; 32],
                })
                .collect(),
            epochs: (0..4u32)
                .map(|e| EpochSeed {
                    epoch_id: e,
                    seed: [id; 32],
                })
                .collect(),
        }
    }

    fn full_list() -> OwnedChannelsList {
        OwnedChannelsList {
            password_lock_root: [0xA1; 32],
            passkey_locks: (0..8u8)
                .map(|n| PasskeyLock {
                    tag: [n; 16],
                    root: [n; 32],
                })
                .collect(),
            channels: (0..64u8).map(full_channel).collect(),
        }
    }

    #[test]
    fn contexts_and_sizes_are_pinned() {
        assert_eq!(
            OWNED_LOCK_PASSWORD_CONTEXT,
            "lakko/channels/owned-lock-password/v1"
        );
        assert_eq!(OWNED_LOCK_KEYS_CONTEXT, "lakko/channels/owned-lock-keys/v1");
        assert_eq!(
            OWNED_LOCK_PASSKEY_CONTEXT,
            "lakko/channels/owned-lock-passkey/v1"
        );
        assert_eq!(OWNED_CHANNELS_VERSION, 2);
        assert_eq!(OWNED_CHANNELS_LIST_LEN, 65_532);
        // 33 024 bytes of entries, 417 of lock roots, 33 443 at most in all,
        // which does not fit the 32 KiB bucket version 1 used.
        assert_eq!(MAX_OWNED_CHANNELS * MAX_ENTRY_LEN, 33_024);
        assert_eq!(KEY_LEN + 1 + MAX_PASSKEY_LOCKS * PASSKEY_LOCK_LEN, 417);
        assert_eq!(MAX_CONTENT_LEN, 33_443);
        const { assert!(MAX_CONTENT_LEN > 32 * 1024 - 4) };
        // Every list seals to the 64 KiB bucket, and one byte more would not.
        let overhead = lakko_vault::seal::OBJECT_OVERHEAD_BYTES;
        assert_eq!(
            lakko_vault::seal::sealed_len(OWNED_CHANNELS_LIST_LEN),
            Some(64 * 1024 + overhead)
        );
        assert_ne!(
            lakko_vault::seal::sealed_len(OWNED_CHANNELS_LIST_LEN + 1),
            Some(64 * 1024 + overhead)
        );
    }

    #[test]
    fn the_lock_roots_are_the_documented_derivations_and_differ() {
        let export = [0x11; 64];
        let s = [0x22; 32];
        let mut material = export.to_vec();
        material.extend_from_slice(&s);
        assert_eq!(
            password_lock_root(&export, &s).as_bytes(),
            &hash::blake3_derive_key_from_material(OWNED_LOCK_PASSWORD_CONTEXT, &material)
        );
        assert_eq!(
            keys_lock_root(&[0x33; 32]).as_bytes(),
            &hash::blake3_derive_key_from_material(OWNED_LOCK_KEYS_CONTEXT, &[0x33; 32])
        );
        let mut pk_material = [0x44u8; 32].to_vec();
        pk_material.extend_from_slice(&s);
        assert_eq!(
            passkey_lock_root(&[0x44; 32], &s).as_bytes(),
            &hash::blake3_derive_key_from_material(OWNED_LOCK_PASSKEY_CONTEXT, &pk_material)
        );
        // Without S, or with another S, the password lock is another lock.
        assert_ne!(
            password_lock_root(&export, &s).as_bytes(),
            password_lock_root(&export, &[0x23; 32]).as_bytes()
        );
        // The same 32 bytes as a prf output and as half of a keys input
        // never give one root.
        assert_ne!(
            passkey_lock_root(&[0x33; 32], &[0x33; 32]).as_bytes(),
            keys_lock_root(&[0x33; 32]).as_bytes()
        );
    }

    #[test]
    fn lock_heads_round_trip_and_refuse_other_lengths() {
        let head = encode_list_key_lock(&[7; 32]);
        assert_eq!(decode_list_key_lock(&head).unwrap().as_bytes(), &[7; 32]);
        assert_eq!(
            decode_list_key_lock(&[0; 31]).err(),
            Some(OwnedChannelsError::BadLength)
        );
        let keys = encode_keys_lock(&[1; 32], &[2; 32]);
        let (r, s) = decode_keys_lock(&keys).unwrap();
        assert_eq!((r.as_bytes(), s.as_bytes()), (&[1; 32], &[2; 32]));
        assert_eq!(
            decode_keys_lock(&head).err(),
            Some(OwnedChannelsError::BadLength)
        );
    }

    #[test]
    fn the_fullest_list_fits_and_round_trips() {
        let list = full_list();
        let wire = list.encode().unwrap();
        assert_eq!(wire.len(), OWNED_CHANNELS_LIST_LEN);
        // The fullest list uses exactly the worst-case content length.
        let content_end = wire.iter().rposition(|b| *b != 0).unwrap() + 1;
        assert_eq!(content_end, MAX_CONTENT_LEN);
        assert_eq!(OwnedChannelsList::decode(&wire).unwrap(), list);
        assert_eq!(list.channels[5].handle_seed, Some([5 ^ 0x55; 32]));
        let empty = OwnedChannelsList {
            password_lock_root: [0; 32],
            passkey_locks: vec![],
            channels: vec![],
        };
        let wire = empty.encode().unwrap();
        assert_eq!(wire.len(), OWNED_CHANNELS_LIST_LEN);
        assert_eq!(OwnedChannelsList::decode(&wire).unwrap(), empty);
    }

    #[test]
    fn decode_refuses_every_bad_shape() {
        let good = full_list().encode().unwrap();
        assert_eq!(
            OwnedChannelsList::decode(&good[1..]),
            Err(OwnedChannelsError::BadLength)
        );
        // One byte over the fixed length is refused, even a zero byte.
        let mut over = good.clone();
        over.push(0);
        assert_eq!(
            OwnedChannelsList::decode(&over),
            Err(OwnedChannelsError::BadLength)
        );
        // Version 1 (no handle seed) is refused.
        let mut v = good.clone();
        v[0] = 1;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::UnknownVersion)
        );
        let mut v = good.clone();
        v[33] = 9;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::TooMany)
        );
        // Swap two passkey tags: out of order.
        let mut v = good.clone();
        v[34] = 0xFF;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::NotCanonical)
        );
        // A read key length of 7.
        let first_entry = 1 + 32 + 1 + 8 * 48 + 1;
        let mut v = good.clone();
        v[first_entry + 16] = 7;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::BadReadKey)
        );
        // A handle seed length of 31.
        let mut v = good.clone();
        v[first_entry + 16 + 1 + 32] = 31;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::BadHandleSeed)
        );
        // A non-zero padding byte.
        let mut v = good.clone();
        let last = v.len() - 1;
        v[last] = 1;
        assert_eq!(
            OwnedChannelsList::decode(&v),
            Err(OwnedChannelsError::BadPadding)
        );
        // A channel count of 65.
        let mut v = full_list();
        v.channels.truncate(0);
        let mut w = v.encode().unwrap();
        w[1 + 32 + 1 + 8 * 48] = 65;
        assert_eq!(
            OwnedChannelsList::decode(&w),
            Err(OwnedChannelsError::TooMany)
        );
        // Encoding refuses an unsorted list.
        let mut v = full_list();
        v.channels.swap(0, 1);
        assert_eq!(v.encode(), Err(OwnedChannelsError::NotCanonical));
    }

    #[test]
    fn debug_never_prints_a_seed() {
        let list = full_list();
        let text = format!("{list:?}");
        assert!(text.contains("has_handle_seed: true"));
        // The handle seed of channel 0 is 0x55 repeated; neither it nor the
        // read key bytes appear.
        assert!(!text.contains("85, 85, 85"));
        assert!(!text.contains("seed: ["));
    }

    #[test]
    fn two_admission_seeds_of_one_version_coexist() {
        let mut c = full_channel(1);
        c.admission = vec![
            AdmissionSeed {
                version: 3,
                seed: [1; 32],
            },
            AdmissionSeed {
                version: 3,
                seed: [2; 32],
            },
        ];
        let list = OwnedChannelsList {
            password_lock_root: [0; 32],
            passkey_locks: vec![],
            channels: vec![c],
        };
        assert_eq!(
            OwnedChannelsList::decode(&list.encode().unwrap()).unwrap(),
            list
        );
    }

    #[test]
    fn merge_keeps_the_newest_and_drops_the_deleted() {
        let mine = OwnedChannelsList {
            password_lock_root: [1; 32],
            passkey_locks: vec![PasskeyLock {
                tag: [5; 16],
                root: [5; 32],
            }],
            channels: vec![
                OwnedChannel {
                    channel_id: [1; 16],
                    read_key: None,
                    handle_seed: None,
                    admission: vec![],
                    epochs: vec![EpochSeed {
                        epoch_id: 7,
                        seed: [0xAA; 32],
                    }],
                },
                OwnedChannel {
                    channel_id: [3; 16],
                    read_key: None,
                    handle_seed: None,
                    admission: vec![],
                    epochs: vec![],
                },
            ],
        };
        let theirs = OwnedChannelsList {
            password_lock_root: [2; 32],
            passkey_locks: vec![PasskeyLock {
                tag: [4; 16],
                root: [4; 32],
            }],
            channels: vec![
                OwnedChannel {
                    channel_id: [1; 16],
                    read_key: Some([9; 32]),
                    handle_seed: Some([8; 32]),
                    admission: vec![AdmissionSeed {
                        version: 1,
                        seed: [1; 32],
                    }],
                    epochs: (3..8u32)
                        .map(|e| EpochSeed {
                            epoch_id: e,
                            seed: [0xBB; 32],
                        })
                        .collect(),
                },
                OwnedChannel {
                    channel_id: [2; 16],
                    read_key: None,
                    handle_seed: None,
                    admission: vec![],
                    epochs: vec![],
                },
            ],
        };
        let merged = mine.merge(&theirs, &[[2; 16]]);
        assert_eq!(merged.password_lock_root, [1; 32]);
        assert_eq!(
            merged
                .passkey_locks
                .iter()
                .map(|l| l.tag[0])
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(
            merged
                .channels
                .iter()
                .map(|c| c.channel_id[0])
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        let one = &merged.channels[0];
        assert_eq!(one.read_key, Some([9; 32]));
        assert_eq!(one.handle_seed, Some([8; 32]));
        assert_eq!(one.admission.len(), 1);
        assert_eq!(
            one.epochs.iter().map(|e| e.epoch_id).collect::<Vec<_>>(),
            vec![4, 5, 6, 7]
        );
        assert_eq!(one.epochs[3].seed, [0xAA; 32]);
        assert!(merged.encode().is_ok());
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut wire = bytes;
            wire.resize(OWNED_CHANNELS_LIST_LEN, 0);
            if let Some(same) = fuzz_decode(&wire) {
                prop_assert!(same);
            }
        }

        #[test]
        fn any_list_round_trips(
            ids in proptest::collection::btree_set(any::<[u8; 16]>(), 0..=64),
            versions in proptest::collection::btree_set(any::<u32>(), 0..=8),
            epochs in proptest::collection::btree_set(any::<u32>(), 0..=4),
            read in any::<bool>(),
            handle in any::<bool>(),
        ) {
            let channels: Vec<OwnedChannel> = ids.into_iter().map(|id| OwnedChannel {
                channel_id: id,
                read_key: read.then_some([3; 32]),
                handle_seed: handle.then_some([4; 32]),
                admission: versions.iter().map(|v| AdmissionSeed { version: *v, seed: [1; 32] }).collect(),
                epochs: epochs.iter().map(|e| EpochSeed { epoch_id: *e, seed: [2; 32] }).collect(),
            }).collect();
            let list = OwnedChannelsList { password_lock_root: [7; 32], passkey_locks: vec![], channels };
            let wire = list.encode().unwrap();
            prop_assert_eq!(wire.len(), OWNED_CHANNELS_LIST_LEN);
            prop_assert_eq!(OwnedChannelsList::decode(&wire).unwrap(), list);
        }
    }
}
