// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the account's owned-channels list and its locks
//! ([ADR-0101] section 7, settled 2026-11-26; `lakko_channels::owned`).
//! Mirror of `crates/lakko-ffi-wasm/src/owned_channels.rs`.
//!
//! - Lock roots: [`owned_password_lock_root`] (export key and the server
//!   half `S`), [`owned_keys_lock_root`] (the lineage account identity),
//!   [`owned_passkey_lock_root`] (a passkey's `prf` output and `S`). Each
//!   root names an ADR-0064 vault through `vaultKeysFromRoot(root)` and
//!   `VaultRef.ownedChannels`; the list's own root is the list key `R`
//!   ([`generate_owned_channels_list_key`]).
//! - Lock heads: [`encode_owned_list_key_lock`] / [`decode_owned_list_key_lock`]
//!   (password and passkey locks, `R`), [`encode_owned_keys_lock`] /
//!   [`decode_owned_keys_lock`] (`R ‖ S`).
//! - The list: [`encode_owned_channels_list`], [`decode_owned_channels_list`],
//!   [`merge_owned_channels_lists`]. Encode sorts every part first, so a
//!   client may build the record in any order; a repeated item is refused.
//! - The recovery key of one channel is `ChannelRecoveryKey.fromListKey`.
//!
//! The roots, `R`, `S` and the seeds are secrets; they cross this boundary
//! because the client stores and sends them in vault objects. Refusals are
//! [`FfiError::OwnedChannels`] with `"<code>: <reason>"`.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use std::sync::Arc;

use lakko_channels::owned::{
    self, AdmissionSeed, EpochSeed, OwnedChannel, OwnedChannelsError, OwnedChannelsList,
    PasskeyLock, OWNED_CHANNELS_LIST_LEN,
};

use crate::{FfiError, Identity};

fn err(code: &str, reason: impl core::fmt::Display) -> FfiError {
    FfiError::OwnedChannels(format!("{code}: {reason}"))
}

fn list_err(e: OwnedChannelsError) -> FfiError {
    err(e.code(), e)
}

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    bytes.try_into().map_err(|_| {
        err(
            "keyLength",
            format!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

/// The password lock's root (32 bytes).
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `keyLength`.
#[uniffi::export]
pub fn owned_password_lock_root(
    export_key: Vec<u8>,
    server_half: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(owned::password_lock_root(
        &array::<64>(&export_key, "exportKey")?,
        &array::<32>(&server_half, "serverHalf")?,
    )
    .as_bytes()
    .to_vec())
}

/// The keys lock's root (32 bytes) from the lineage account identity (the
/// oldest own predecessor, else the current account identity: the one the
/// DeviceGroup id comes from).
#[uniffi::export]
#[must_use]
pub fn owned_keys_lock_root(lineage_identity: Arc<Identity>) -> Vec<u8> {
    owned::keys_lock_root(&lineage_identity.inner.ed25519_seed_bytes())
        .as_bytes()
        .to_vec()
}

/// A passkey lock's root (32 bytes).
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `keyLength`.
#[uniffi::export]
pub fn owned_passkey_lock_root(
    prf_output: Vec<u8>,
    server_half: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(owned::passkey_lock_root(
        &array::<32>(&prf_output, "prfOutput")?,
        &array::<32>(&server_half, "serverHalf")?,
    )
    .as_bytes()
    .to_vec())
}

/// A fresh random list key `R` (32 bytes), made once per account.
#[uniffi::export]
#[must_use]
pub fn generate_owned_channels_list_key() -> Vec<u8> {
    let mut r = [0u8; 32];
    lakko_crypto::random::fill(&mut r);
    r.to_vec()
}

/// The head of a password or passkey lock (`R`).
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `keyLength`.
#[uniffi::export]
pub fn encode_owned_list_key_lock(list_key: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(owned::encode_list_key_lock(&array::<32>(&list_key, "listKey")?).to_vec())
}

/// Read a password or passkey lock head: `R`.
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `badLength`.
#[uniffi::export]
pub fn decode_owned_list_key_lock(head: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    owned::decode_list_key_lock(&head)
        .map(|r| r.as_bytes().to_vec())
        .map_err(list_err)
}

/// The keys lock's content.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedKeysLock {
    /// The list key `R`.
    pub list_key: Vec<u8>,
    /// The server half `S`.
    pub server_half: Vec<u8>,
}

/// The head of the keys lock (`R ‖ S`).
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `keyLength`.
#[uniffi::export]
pub fn encode_owned_keys_lock(
    list_key: Vec<u8>,
    server_half: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(owned::encode_keys_lock(
        &array::<32>(&list_key, "listKey")?,
        &array::<32>(&server_half, "serverHalf")?,
    )
    .to_vec())
}

/// Read the keys lock head.
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `badLength`.
#[uniffi::export]
pub fn decode_owned_keys_lock(head: Vec<u8>) -> Result<FfiOwnedKeysLock, FfiError> {
    let (r, s) = owned::decode_keys_lock(&head).map_err(list_err)?;
    Ok(FfiOwnedKeysLock {
        list_key: r.as_bytes().to_vec(),
        server_half: s.as_bytes().to_vec(),
    })
}

/// One passkey lock the list names.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedPasskeyLock {
    /// The passkey's 16-byte tag (`resetSealPasskeyTag`).
    pub tag: Vec<u8>,
    /// The lock's 32-byte root.
    pub root: Vec<u8>,
}

/// One admission seed.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedAdmissionSeed {
    /// The admission version.
    pub version: u32,
    /// The 32-byte seed.
    pub seed: Vec<u8>,
}

/// One epoch seed.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedEpochSeed {
    /// The epoch.
    pub epoch_id: u32,
    /// The 32-byte seed.
    pub seed: Vec<u8>,
}

/// One owned channel.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedChannel {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// The 32-byte read key of a public channel.
    pub read_key: Option<Vec<u8>>,
    /// The channel's 32-byte ADR-0087 handle write seed, when it has an
    /// `@name`: a device that retakes the channel after a key reset signs
    /// `rekey` with it to the handle key its new root derives.
    #[uniffi(default = None)]
    pub handle_seed: Option<Vec<u8>>,
    /// At most eight admission seeds.
    pub admission: Vec<FfiOwnedAdmissionSeed>,
    /// At most four epoch seeds.
    pub epochs: Vec<FfiOwnedEpochSeed>,
}

/// The list.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiOwnedChannelsList {
    /// The current password lock's 32-byte root.
    pub password_lock_root: Vec<u8>,
    /// At most eight passkey locks.
    pub passkey_locks: Vec<FfiOwnedPasskeyLock>,
    /// At most 64 channels.
    pub channels: Vec<FfiOwnedChannel>,
}

fn to_core(list: &FfiOwnedChannelsList) -> Result<OwnedChannelsList, FfiError> {
    let mut passkey_locks = list
        .passkey_locks
        .iter()
        .map(|l| {
            Ok(PasskeyLock {
                tag: array::<16>(&l.tag, "tag")?,
                root: array::<32>(&l.root, "root")?,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    passkey_locks.sort_by_key(|l| l.tag);
    let mut channels = list
        .channels
        .iter()
        .map(|c| {
            let mut admission = c
                .admission
                .iter()
                .map(|a| {
                    Ok(AdmissionSeed {
                        version: a.version,
                        seed: array::<32>(&a.seed, "seed")?,
                    })
                })
                .collect::<Result<Vec<_>, FfiError>>()?;
            admission.sort();
            let mut epochs = c
                .epochs
                .iter()
                .map(|e| {
                    Ok(EpochSeed {
                        epoch_id: e.epoch_id,
                        seed: array::<32>(&e.seed, "seed")?,
                    })
                })
                .collect::<Result<Vec<_>, FfiError>>()?;
            epochs.sort_by_key(|e| e.epoch_id);
            Ok(OwnedChannel {
                channel_id: array::<16>(&c.channel_id, "channelId")?,
                read_key: c
                    .read_key
                    .as_deref()
                    .map(|k| array::<32>(k, "readKey"))
                    .transpose()?,
                handle_seed: c
                    .handle_seed
                    .as_deref()
                    .map(|k| array::<32>(k, "handleSeed"))
                    .transpose()?,
                admission,
                epochs,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    channels.sort_by_key(|c| c.channel_id);
    Ok(OwnedChannelsList {
        password_lock_root: array::<32>(&list.password_lock_root, "passwordLockRoot")?,
        passkey_locks,
        channels,
    })
}

fn from_core(list: &OwnedChannelsList) -> FfiOwnedChannelsList {
    FfiOwnedChannelsList {
        password_lock_root: list.password_lock_root.to_vec(),
        passkey_locks: list
            .passkey_locks
            .iter()
            .map(|l| FfiOwnedPasskeyLock {
                tag: l.tag.to_vec(),
                root: l.root.to_vec(),
            })
            .collect(),
        channels: list
            .channels
            .iter()
            .map(|c| FfiOwnedChannel {
                channel_id: c.channel_id.to_vec(),
                read_key: c.read_key.map(|k| k.to_vec()),
                handle_seed: c.handle_seed.map(|k| k.to_vec()),
                admission: c
                    .admission
                    .iter()
                    .map(|a| FfiOwnedAdmissionSeed {
                        version: a.version,
                        seed: a.seed.to_vec(),
                    })
                    .collect(),
                epochs: c
                    .epochs
                    .iter()
                    .map(|e| FfiOwnedEpochSeed {
                        epoch_id: e.epoch_id,
                        seed: e.seed.to_vec(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Encode the list to exactly [`owned_channels_list_len`] bytes, the head
/// plaintext of the list vault. Every part is sorted first.
///
/// # Errors
///
/// [`FfiError::OwnedChannels`]: `keyLength`, `tooMany`, `notCanonical` (a
/// repeated channel, passkey tag, admission seed or epoch).
#[uniffi::export]
pub fn encode_owned_channels_list(list: FfiOwnedChannelsList) -> Result<Vec<u8>, FfiError> {
    to_core(&list)?.encode().map_err(list_err)
}

/// Decode an opened list. Bounded, exact and fail-closed.
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] with the codec's code.
#[uniffi::export]
pub fn decode_owned_channels_list(bytes: Vec<u8>) -> Result<FfiOwnedChannelsList, FfiError> {
    OwnedChannelsList::decode(&bytes)
        .map(|l| from_core(&l))
        .map_err(list_err)
}

/// Merge after a `409 vault-version-conflict`: `mine` is what this device
/// wanted to write, `theirs` what it read back, `deleted` the 16-byte ids of
/// the channels this device deleted (`OwnedChannelsList::merge`).
///
/// # Errors
///
/// [`FfiError::OwnedChannels`] `keyLength` / `notCanonical` for a malformed
/// record.
#[uniffi::export]
pub fn merge_owned_channels_lists(
    mine: FfiOwnedChannelsList,
    theirs: FfiOwnedChannelsList,
    deleted: Vec<Vec<u8>>,
) -> Result<FfiOwnedChannelsList, FfiError> {
    let deleted = deleted
        .iter()
        .map(|d| array::<16>(d, "deleted"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(from_core(
        &to_core(&mine)?.merge(&to_core(&theirs)?, &deleted),
    ))
}

/// The list's fixed length (65 532: the 64 KiB vault bucket less its 4-byte
/// length frame).
#[uniffi::export]
#[must_use]
pub fn owned_channels_list_len() -> u32 {
    u32::try_from(OWNED_CHANNELS_LIST_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn list() -> FfiOwnedChannelsList {
        FfiOwnedChannelsList {
            password_lock_root: vec![1; 32],
            passkey_locks: vec![
                FfiOwnedPasskeyLock {
                    tag: vec![9; 16],
                    root: vec![9; 32],
                },
                FfiOwnedPasskeyLock {
                    tag: vec![2; 16],
                    root: vec![2; 32],
                },
            ],
            channels: vec![
                FfiOwnedChannel {
                    channel_id: vec![5; 16],
                    read_key: None,
                    handle_seed: Some(vec![6; 32]),
                    admission: vec![FfiOwnedAdmissionSeed {
                        version: 2,
                        seed: vec![7; 32],
                    }],
                    epochs: vec![
                        FfiOwnedEpochSeed {
                            epoch_id: 9,
                            seed: vec![1; 32],
                        },
                        FfiOwnedEpochSeed {
                            epoch_id: 3,
                            seed: vec![1; 32],
                        },
                    ],
                },
                FfiOwnedChannel {
                    channel_id: vec![4; 16],
                    read_key: Some(vec![8; 32]),
                    handle_seed: None,
                    admission: vec![],
                    epochs: vec![],
                },
            ],
        }
    }

    #[test]
    fn the_list_and_the_locks_round_trip() {
        let wire = encode_owned_channels_list(list()).unwrap();
        assert_eq!(wire.len(), owned_channels_list_len() as usize);
        let back = decode_owned_channels_list(wire).unwrap();
        assert_eq!(back.channels[0].channel_id, vec![4; 16]);
        assert_eq!(back.passkey_locks[0].tag, vec![2; 16]);
        assert_eq!(back.channels[1].epochs[0].epoch_id, 3);
        assert_eq!(back.channels[0].handle_seed, None);
        assert_eq!(back.channels[1].handle_seed, Some(vec![6; 32]));
        assert_eq!(owned_channels_list_len(), 65_532);
        let merged = merge_owned_channels_lists(back.clone(), back, vec![vec![4; 16]]).unwrap();
        assert_eq!(merged.channels.len(), 1);

        let r = generate_owned_channels_list_key();
        assert_eq!(
            decode_owned_list_key_lock(encode_owned_list_key_lock(r.clone()).unwrap()).unwrap(),
            r
        );
        let keys = decode_owned_keys_lock(encode_owned_keys_lock(r.clone(), vec![3; 32]).unwrap())
            .unwrap();
        assert_eq!((keys.list_key, keys.server_half), (r, vec![3; 32]));
        assert_eq!(
            owned_password_lock_root(vec![1; 64], vec![2; 32])
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            owned_passkey_lock_root(vec![1; 32], vec![2; 32])
                .unwrap()
                .len(),
            32
        );
        assert!(owned_password_lock_root(vec![1; 63], vec![2; 32]).is_err());
        let id = Arc::new(Identity::from_seed(vec![4; 32]).unwrap());
        assert_eq!(owned_keys_lock_root(id).len(), 32);
        let mut dup = list();
        dup.channels[0].channel_id = vec![4; 16];
        assert!(encode_owned_channels_list(dup).is_err());
        let mut short = list();
        short.channels[0].handle_seed = Some(vec![6; 31]);
        assert!(encode_owned_channels_list(short).is_err());
    }
}
