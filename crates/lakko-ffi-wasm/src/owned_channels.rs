// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the account's owned-channels list and its locks
//! ([ADR-0101] section 7, settled 2026-11-26; `lakko_channels::owned`).
//! Mirror of `crates/lakko-ffi-uniffi/src/owned_channels.rs`.
//!
//! - Lock roots: `ownedPasswordLockRoot(exportKey, serverHalf)`,
//!   `ownedKeysLockRoot(lineageIdentity)`,
//!   `ownedPasskeyLockRoot(prfOutput, serverHalf)`; each names a vault through
//!   `vaultKeysFromRoot(root)` and `VaultRef.ownedChannels()`. The list's own
//!   root is the list key `R` (`generateOwnedChannelsListKey()`).
//! - Lock heads: `encodeOwnedListKeyLock` / `decodeOwnedListKeyLock` (`R`),
//!   `encodeOwnedKeysLock` / `decodeOwnedKeysLock` (`R ‖ S`, 64 bytes; the
//!   decoder returns the 64 bytes: `R` first, then `S`).
//! - The list: `new OwnedChannelsList(passwordLockRoot)`, `addPasskeyLock`,
//!   `addChannel`, `removeChannel`, the getters, `encode()`,
//!   `OwnedChannelsList.decode(bytes)`, `merge(theirs, deletedIds)`. Every
//!   part is kept sorted, so a client may add in any order; a repeated item
//!   is refused at `encode()`.
//!
//! Every refusal throws a string `"ownedChannels.<code>: <reason>"`.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use lakko_channels::owned::{
    self, AdmissionSeed, EpochSeed, OwnedChannel, OwnedChannelsError,
    OwnedChannelsList as RustList, PasskeyLock, OWNED_CHANNELS_LIST_LEN,
};
use wasm_bindgen::prelude::*;

fn err(code: &str, reason: impl core::fmt::Display) -> String {
    format!("ownedChannels.{code}: {reason}")
}

fn list_err(e: OwnedChannelsError) -> String {
    err(e.code(), e)
}

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
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
/// `ownedChannels.keyLength`.
#[wasm_bindgen(js_name = ownedPasswordLockRoot)]
pub fn owned_password_lock_root(export_key: &[u8], server_half: &[u8]) -> Result<Vec<u8>, String> {
    Ok(owned::password_lock_root(
        &array::<64>(export_key, "exportKey")?,
        &array::<32>(server_half, "serverHalf")?,
    )
    .as_bytes()
    .to_vec())
}

/// The keys lock's root (32 bytes) from the lineage account identity.
#[wasm_bindgen(js_name = ownedKeysLockRoot)]
#[must_use]
pub fn owned_keys_lock_root(lineage_identity: &crate::mls::Identity) -> Vec<u8> {
    owned::keys_lock_root(&lineage_identity.mls_identity().ed25519_seed_bytes())
        .as_bytes()
        .to_vec()
}

/// A passkey lock's root (32 bytes).
///
/// # Errors
///
/// `ownedChannels.keyLength`.
#[wasm_bindgen(js_name = ownedPasskeyLockRoot)]
pub fn owned_passkey_lock_root(prf_output: &[u8], server_half: &[u8]) -> Result<Vec<u8>, String> {
    Ok(owned::passkey_lock_root(
        &array::<32>(prf_output, "prfOutput")?,
        &array::<32>(server_half, "serverHalf")?,
    )
    .as_bytes()
    .to_vec())
}

/// A fresh random list key `R` (32 bytes).
#[wasm_bindgen(js_name = generateOwnedChannelsListKey)]
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
/// `ownedChannels.keyLength`.
#[wasm_bindgen(js_name = encodeOwnedListKeyLock)]
pub fn encode_owned_list_key_lock(list_key: &[u8]) -> Result<Vec<u8>, String> {
    Ok(owned::encode_list_key_lock(&array::<32>(list_key, "listKey")?).to_vec())
}

/// Read a password or passkey lock head: `R`.
///
/// # Errors
///
/// `ownedChannels.badLength`.
#[wasm_bindgen(js_name = decodeOwnedListKeyLock)]
pub fn decode_owned_list_key_lock(head: &[u8]) -> Result<Vec<u8>, String> {
    owned::decode_list_key_lock(head)
        .map(|r| r.as_bytes().to_vec())
        .map_err(list_err)
}

/// The head of the keys lock (`R ‖ S`).
///
/// # Errors
///
/// `ownedChannels.keyLength`.
#[wasm_bindgen(js_name = encodeOwnedKeysLock)]
pub fn encode_owned_keys_lock(list_key: &[u8], server_half: &[u8]) -> Result<Vec<u8>, String> {
    Ok(owned::encode_keys_lock(
        &array::<32>(list_key, "listKey")?,
        &array::<32>(server_half, "serverHalf")?,
    )
    .to_vec())
}

/// Read the keys lock head: 64 bytes, `R` then `S`.
///
/// # Errors
///
/// `ownedChannels.badLength`.
#[wasm_bindgen(js_name = decodeOwnedKeysLock)]
pub fn decode_owned_keys_lock(head: &[u8]) -> Result<Vec<u8>, String> {
    let (r, s) = owned::decode_keys_lock(head).map_err(list_err)?;
    Ok([r.as_bytes().as_slice(), s.as_bytes().as_slice()].concat())
}

/// The list's fixed length (32 764).
#[wasm_bindgen(js_name = ownedChannelsListLen)]
#[must_use]
pub fn owned_channels_list_len() -> u32 {
    u32::try_from(OWNED_CHANNELS_LIST_LEN).unwrap_or(u32::MAX)
}

/// The owned-channels list.
#[wasm_bindgen]
pub struct OwnedChannelsList {
    inner: RustList,
}

fn seed_rows<T>(
    keys: &[u32],
    seeds: &[u8],
    make: impl Fn(u32, [u8; 32]) -> T,
) -> Result<Vec<T>, String> {
    if seeds.len() != keys.len() * 32 {
        return Err(err("keyLength", "seeds must hold 32 bytes per entry"));
    }
    keys.iter()
        .enumerate()
        .map(|(i, k)| Ok(make(*k, array::<32>(&seeds[i * 32..(i + 1) * 32], "seed")?)))
        .collect()
}

#[wasm_bindgen]
impl OwnedChannelsList {
    /// An empty list naming the current password lock's root.
    ///
    /// # Errors
    ///
    /// `ownedChannels.keyLength`.
    #[wasm_bindgen(constructor)]
    pub fn new(password_lock_root: &[u8]) -> Result<OwnedChannelsList, String> {
        Ok(Self {
            inner: RustList {
                password_lock_root: array::<32>(password_lock_root, "passwordLockRoot")?,
                passkey_locks: Vec::new(),
                channels: Vec::new(),
            },
        })
    }

    /// Decode an opened list.
    ///
    /// # Errors
    ///
    /// `ownedChannels.<code>` of the codec.
    pub fn decode(bytes: &[u8]) -> Result<OwnedChannelsList, String> {
        RustList::decode(bytes)
            .map(|inner| Self { inner })
            .map_err(list_err)
    }

    /// Encode to exactly `ownedChannelsListLen()` bytes.
    ///
    /// # Errors
    ///
    /// `ownedChannels.tooMany` / `ownedChannels.notCanonical`.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.inner.encode().map_err(list_err)
    }

    /// Replace the password lock's root (after a password change).
    ///
    /// # Errors
    ///
    /// `ownedChannels.keyLength`.
    #[wasm_bindgen(js_name = setPasswordLockRoot)]
    pub fn set_password_lock_root(&mut self, root: &[u8]) -> Result<(), String> {
        self.inner.password_lock_root = array::<32>(root, "root")?;
        Ok(())
    }

    /// Add (or replace, by tag) a passkey lock.
    ///
    /// # Errors
    ///
    /// `ownedChannels.keyLength`.
    #[wasm_bindgen(js_name = addPasskeyLock)]
    pub fn add_passkey_lock(&mut self, tag: &[u8], root: &[u8]) -> Result<(), String> {
        let lock = PasskeyLock {
            tag: array::<16>(tag, "tag")?,
            root: array::<32>(root, "root")?,
        };
        self.inner.passkey_locks.retain(|l| l.tag != lock.tag);
        self.inner.passkey_locks.push(lock);
        self.inner.passkey_locks.sort_by_key(|l| l.tag);
        Ok(())
    }

    /// Remove the passkey lock with `tag`.
    #[wasm_bindgen(js_name = removePasskeyLock)]
    pub fn remove_passkey_lock(&mut self, tag: &[u8]) {
        self.inner.passkey_locks.retain(|l| l.tag.as_slice() != tag);
    }

    /// Add (or replace, by id) a channel. `admissionSeeds` and `epochSeeds`
    /// hold 32 bytes per version / epoch.
    ///
    /// # Errors
    ///
    /// `ownedChannels.keyLength`.
    #[wasm_bindgen(js_name = addChannel)]
    pub fn add_channel(
        &mut self,
        channel_id: &[u8],
        read_key: Option<Vec<u8>>,
        admission_versions: &[u32],
        admission_seeds: &[u8],
        epoch_ids: &[u32],
        epoch_seeds: &[u8],
    ) -> Result<(), String> {
        let mut admission = seed_rows(admission_versions, admission_seeds, |version, seed| {
            AdmissionSeed { version, seed }
        })?;
        admission.sort();
        let mut epochs = seed_rows(epoch_ids, epoch_seeds, |epoch_id, seed| EpochSeed {
            epoch_id,
            seed,
        })?;
        epochs.sort_by_key(|e| e.epoch_id);
        let channel = OwnedChannel {
            channel_id: array::<16>(channel_id, "channelId")?,
            read_key: read_key
                .as_deref()
                .map(|k| array::<32>(k, "readKey"))
                .transpose()?,
            admission,
            epochs,
        };
        self.inner
            .channels
            .retain(|c| c.channel_id != channel.channel_id);
        self.inner.channels.push(channel);
        self.inner.channels.sort_by_key(|c| c.channel_id);
        Ok(())
    }

    /// Remove the channel `channelId`.
    #[wasm_bindgen(js_name = removeChannel)]
    pub fn remove_channel(&mut self, channel_id: &[u8]) {
        self.inner
            .channels
            .retain(|c| c.channel_id.as_slice() != channel_id);
    }

    /// Merge after a `409`: `this` is what this device wanted to write,
    /// `theirs` what it read back, `deletedIds` the 16-byte ids of the
    /// channels this device deleted, concatenated.
    ///
    /// # Errors
    ///
    /// `ownedChannels.keyLength` when `deletedIds` is not a multiple of 16.
    pub fn merge(
        &self,
        theirs: &OwnedChannelsList,
        deleted_ids: &[u8],
    ) -> Result<OwnedChannelsList, String> {
        if deleted_ids.len() % 16 != 0 {
            return Err(err("keyLength", "deletedIds must hold 16 bytes per id"));
        }
        let deleted: Vec<[u8; 16]> = deleted_ids
            .chunks_exact(16)
            .map(|c| array::<16>(c, "deleted"))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            inner: self.inner.merge(&theirs.inner, &deleted),
        })
    }

    /// The password lock's root.
    #[wasm_bindgen(getter, js_name = passwordLockRoot)]
    #[must_use]
    pub fn password_lock_root(&self) -> Vec<u8> {
        self.inner.password_lock_root.to_vec()
    }

    /// The passkey locks' tags, 16 bytes each, concatenated.
    #[wasm_bindgen(getter, js_name = passkeyLockTags)]
    #[must_use]
    pub fn passkey_lock_tags(&self) -> Vec<u8> {
        self.inner
            .passkey_locks
            .iter()
            .flat_map(|l| l.tag)
            .collect()
    }

    /// The passkey locks' roots, 32 bytes each, concatenated.
    #[wasm_bindgen(getter, js_name = passkeyLockRoots)]
    #[must_use]
    pub fn passkey_lock_roots(&self) -> Vec<u8> {
        self.inner
            .passkey_locks
            .iter()
            .flat_map(|l| l.root)
            .collect()
    }

    /// How many channels are listed.
    #[wasm_bindgen(getter, js_name = channelCount)]
    #[must_use]
    pub fn channel_count(&self) -> u32 {
        u32::try_from(self.inner.channels.len()).unwrap_or(u32::MAX)
    }

    fn at(&self, index: u32) -> Result<&OwnedChannel, String> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.inner.channels.get(i))
            .ok_or_else(|| err("index", format!("no channel at {index}")))
    }

    /// The id of channel `index`.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = channelId)]
    pub fn channel_id(&self, index: u32) -> Result<Vec<u8>, String> {
        Ok(self.at(index)?.channel_id.to_vec())
    }

    /// The read key of channel `index`, when public.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = readKey)]
    pub fn read_key(&self, index: u32) -> Result<Option<Vec<u8>>, String> {
        Ok(self.at(index)?.read_key.map(|k| k.to_vec()))
    }

    /// The admission versions of channel `index`.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = admissionVersions)]
    pub fn admission_versions(&self, index: u32) -> Result<Vec<u32>, String> {
        Ok(self
            .at(index)?
            .admission
            .iter()
            .map(|a| a.version)
            .collect())
    }

    /// The admission seeds of channel `index`, 32 bytes each.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = admissionSeeds)]
    pub fn admission_seeds(&self, index: u32) -> Result<Vec<u8>, String> {
        Ok(self
            .at(index)?
            .admission
            .iter()
            .flat_map(|a| a.seed)
            .collect())
    }

    /// The epochs of channel `index`.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = epochIds)]
    pub fn epoch_ids(&self, index: u32) -> Result<Vec<u32>, String> {
        Ok(self.at(index)?.epochs.iter().map(|e| e.epoch_id).collect())
    }

    /// The epoch seeds of channel `index`, 32 bytes each.
    ///
    /// # Errors
    ///
    /// `ownedChannels.index`.
    #[wasm_bindgen(js_name = epochSeeds)]
    pub fn epoch_seeds(&self, index: u32) -> Result<Vec<u8>, String> {
        Ok(self.at(index)?.epochs.iter().flat_map(|e| e.seed).collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_list_round_trips_over_the_wasm_shape() {
        let mut list = OwnedChannelsList::new(&[1; 32]).unwrap();
        list.add_passkey_lock(&[9; 16], &[9; 32]).unwrap();
        list.add_channel(
            &[5; 16],
            None,
            &[2],
            &[7; 32],
            &[9, 3],
            &[[1u8; 32], [2; 32]].concat(),
        )
        .unwrap();
        list.add_channel(&[4; 16], Some(vec![8; 32]), &[], &[], &[], &[])
            .unwrap();
        let wire = list.encode().unwrap();
        assert_eq!(wire.len(), owned_channels_list_len() as usize);
        let back = OwnedChannelsList::decode(&wire).unwrap();
        assert_eq!(back.channel_count(), 2);
        assert_eq!(back.channel_id(0).unwrap(), vec![4; 16]);
        assert_eq!(back.epoch_ids(1).unwrap(), vec![3, 9]);
        assert_eq!(back.epoch_seeds(1).unwrap()[..32], [2; 32]);
        let merged = back.merge(&back, &[4; 16]).unwrap();
        assert_eq!(merged.channel_count(), 1);
        assert!(list
            .add_channel(&[1; 16], None, &[1], &[0; 31], &[], &[])
            .is_err());
        let keys =
            decode_owned_keys_lock(&encode_owned_keys_lock(&[1; 32], &[2; 32]).unwrap()).unwrap();
        assert_eq!(keys.len(), 64);
        assert_eq!(
            owned_password_lock_root(&[1; 64], &[2; 32]).unwrap().len(),
            32
        );
        assert!(owned_passkey_lock_root(&[1; 31], &[2; 32]).is_err());
    }
}
