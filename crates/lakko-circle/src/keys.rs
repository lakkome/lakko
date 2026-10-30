// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Every derivation of a circle directory (ADR-0086) and the one wrap.
//!
//! ```text
//! owner_seed       = derive("lakko/circle/owner/v1", account_seed)
//! owner signing    = derive("lakko/circle/owner-sign/v1", owner_seed)      Ed25519 seed
//! directory_id     = derive("lakko/circle/directory/v1", owner_pk)
//! leaf key         = derive("lakko/circle/leaf/v1",
//!                           owner_seed || salt || leaf(u32 BE) || stamp(u64 BE))
//! parent, one way  = derive("lakko/circle/tree/up/v1", node key)
//! wrap under node  = derive("lakko/circle/tree/wrap/v1", node key)
//! epoch key        = derive("lakko/circle/epoch/v1", root key || epoch(u64 BE))
//! read key         = derive("lakko/circle/read/v1", epoch key)             Ed25519 seed
//! chain key        = derive("lakko/circle/chain/v1", epoch key)
//! row wrap key     = derive("lakko/circle/row-wrap/v1", epoch key)
//! filter seal key  = derive("lakko/circle/filter-seal/v1", epoch key)
//! filter hash key  = derive("lakko/circle/filter-hash/v1", epoch key)
//! ```
//!
//! `derive` is BLAKE3 `derive_key`. Each context is frozen and used for one
//! purpose only, so a node key that feeds its parent never also keys a wrap,
//! and an epoch key opens nothing directly.
//!
//! **Why the directory id hashes the owner key.** The id then proves itself:
//! the Delivery Service checks a claim by the name, as it does for a mailbox,
//! so nobody can squat a person's directory, and the id is derived from the
//! account seed without the server being able to link it to anything.
//!
//! **Why the salt.** Leaf keys are a pure function of the owner seed, so the
//! tree state a device keeps holds no secret. A tree started over with the
//! same seed and no salt would hand out the leaf keys of the old tree again,
//! and a contact removed long ago would be a member. The salt is fresh for
//! every new tree.

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::SigningKey;
use lakko_crypto::{random, SecretBytes};

use crate::error::CircleError;

/// Length of every derived key.
pub const KEY_LEN: usize = 32;
/// Length of a directory id.
pub const DIRECTORY_ID_LEN: usize = 32;
/// Length of the salt of a tree.
pub const SALT_LEN: usize = 16;
/// Length of a wrapped key: nonce, the 32-byte key, the AEAD tag.
pub const WRAP_LEN: usize = NONCE_LEN + KEY_LEN + TAG_LEN;

/// Context of the owner seed. Frozen.
pub const OWNER_CONTEXT: &str = "lakko/circle/owner/v1";
/// Context of the owner signing seed. Frozen.
pub const OWNER_SIGN_CONTEXT: &str = "lakko/circle/owner-sign/v1";
/// Context of the directory id. Frozen.
pub const DIRECTORY_CONTEXT: &str = "lakko/circle/directory/v1";
/// Context of a leaf key. Frozen.
pub const LEAF_CONTEXT: &str = "lakko/circle/leaf/v1";
/// Context of the one-way step from a node to its parent. Frozen.
pub const UP_CONTEXT: &str = "lakko/circle/tree/up/v1";
/// Context of the key a node wraps its parent under. Frozen.
pub const NODE_WRAP_CONTEXT: &str = "lakko/circle/tree/wrap/v1";
/// Context of an epoch key. Frozen.
pub const EPOCH_CONTEXT: &str = "lakko/circle/epoch/v1";
/// Context of the read key. Frozen.
pub const READ_CONTEXT: &str = "lakko/circle/read/v1";
/// Context of the key the chain wraps under. Frozen.
pub const CHAIN_CONTEXT: &str = "lakko/circle/chain/v1";
/// Context of the key row keys are wrapped under. Frozen.
pub const ROW_WRAP_CONTEXT: &str = "lakko/circle/row-wrap/v1";
/// Context of the key a filter is sealed under. Frozen.
pub const FILTER_SEAL_CONTEXT: &str = "lakko/circle/filter-seal/v1";
/// Context of the key a filter hashes terms under. Frozen.
pub const FILTER_HASH_CONTEXT: &str = "lakko/circle/filter-hash/v1";

/// A derivation output, zeroed on drop.
pub(crate) fn derive(context: &str, material: &[u8]) -> SecretBytes<KEY_LEN> {
    SecretBytes::new(blake3_derive_key_from_material(context, material))
}

/// The owner seed of the account whose identity seed is `account_seed`.
#[must_use]
pub fn owner_seed(account_seed: &[u8; KEY_LEN]) -> SecretBytes<KEY_LEN> {
    derive(OWNER_CONTEXT, account_seed)
}

/// The Ed25519 seed that signs the owner's directory requests.
#[must_use]
pub fn owner_signing_seed(owner_seed: &SecretBytes<KEY_LEN>) -> SecretBytes<KEY_LEN> {
    derive(OWNER_SIGN_CONTEXT, owner_seed.as_bytes())
}

/// The public half of [`owner_signing_seed`].
#[must_use]
pub fn owner_public_key(owner_seed: &SecretBytes<KEY_LEN>) -> [u8; KEY_LEN] {
    SigningKey::from_seed(owner_signing_seed(owner_seed).as_bytes())
        .verifying_key()
        .to_bytes()
}

/// The directory id of the owner key `owner_pk`.
#[must_use]
pub fn directory_id(owner_pk: &[u8; KEY_LEN]) -> [u8; DIRECTORY_ID_LEN] {
    blake3_derive_key_from_material(DIRECTORY_CONTEXT, owner_pk)
}

/// Whether `owner_pk` is the key `id` is named after. What the Delivery
/// Service runs on a claim.
#[must_use]
pub fn directory_owner_matches(id: &[u8; DIRECTORY_ID_LEN], owner_pk: &[u8; KEY_LEN]) -> bool {
    directory_id(owner_pk) == *id
}

/// The key of `leaf` at `stamp` in the tree with this salt.
///
/// The concatenation is built in a [`SecretBytes`] scratch buffer so it is
/// zeroed on drop. That is best effort: the compiler may leave a moved copy
/// on the stack, and the hasher's own state is not this crate's to clear.
pub(crate) fn leaf_key(
    owner_seed: &SecretBytes<KEY_LEN>,
    salt: &[u8; SALT_LEN],
    leaf: u32,
    stamp: u64,
) -> SecretBytes<KEY_LEN> {
    let mut material = [0u8; KEY_LEN + SALT_LEN + 4 + 8];
    let (seed_part, rest) = material.split_at_mut(KEY_LEN);
    seed_part.copy_from_slice(owner_seed.as_bytes());
    let (salt_part, rest) = rest.split_at_mut(SALT_LEN);
    salt_part.copy_from_slice(salt);
    let (leaf_part, stamp_part) = rest.split_at_mut(4);
    leaf_part.copy_from_slice(&leaf.to_be_bytes());
    stamp_part.copy_from_slice(&stamp.to_be_bytes());
    let scratch = SecretBytes::new(material);
    derive(LEAF_CONTEXT, scratch.as_bytes())
}

/// One step up the tree: the parent key when this node is the source of it.
pub(crate) fn up(key: &SecretBytes<KEY_LEN>) -> SecretBytes<KEY_LEN> {
    derive(UP_CONTEXT, key.as_bytes())
}

/// The key `key` wraps its parent under.
pub(crate) fn node_wrap_key(key: &SecretBytes<KEY_LEN>) -> SecretBytes<KEY_LEN> {
    derive(NODE_WRAP_CONTEXT, key.as_bytes())
}

/// Wrap a 32-byte key: `nonce || ChaCha20-Poly1305(wrapping, nonce, aad, key)`.
///
/// The nonce is random. It is never derived, because a key wraps a different
/// parent each time the parent changes, and two devices of one owner racing
/// for one epoch would otherwise seal different plaintexts under one nonce.
pub(crate) fn seal_wrap(
    wrapping: &SecretBytes<KEY_LEN>,
    aad: &[u8],
    key: &SecretBytes<KEY_LEN>,
) -> Result<[u8; WRAP_LEN], CircleError> {
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aead_key: AeadKey = wrapping.clone();
    let sealed = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        aad,
        key.as_bytes(),
    )
    .map_err(|_| CircleError::Seal)?;
    if sealed.len() != KEY_LEN + TAG_LEN {
        return Err(CircleError::Seal);
    }
    let mut out = [0u8; WRAP_LEN];
    out[..NONCE_LEN].copy_from_slice(&nonce);
    out[NONCE_LEN..].copy_from_slice(&sealed);
    Ok(out)
}

/// Open a wrap made by [`seal_wrap`].
pub(crate) fn open_wrap(
    wrapping: &SecretBytes<KEY_LEN>,
    aad: &[u8],
    wrap: &[u8; WRAP_LEN],
) -> Result<SecretBytes<KEY_LEN>, CircleError> {
    let (nonce, sealed) = wrap.split_at(NONCE_LEN);
    let nonce: Nonce = nonce.try_into().map_err(|_| CircleError::Length)?;
    let aead_key: AeadKey = wrapping.clone();
    let opened = aead::open(Algorithm::ChaCha20Poly1305, &aead_key, &nonce, aad, sealed)
        .map_err(|_| CircleError::Authentication)?;
    let key: [u8; KEY_LEN] = opened
        .as_slice()
        .try_into()
        .map_err(|_| CircleError::Length)?;
    Ok(SecretBytes::new(key))
}

/// The digest of a write's body. The Delivery Service keeps the digest of the
/// last write, so a write sent again after a lost answer is recognised and
/// not applied twice.
#[must_use]
pub fn write_digest(body: &[u8]) -> [u8; 32] {
    lakko_crypto::hash::blake3(body)
}

/// Lower-case hex of `bytes`, the id in a route.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_derivation_is_pinned() {
        // A change to a context or an input layout must fail here, not on a
        // live directory whose members all stop opening it.
        let owner = owner_seed(&[0x11; KEY_LEN]);
        let salt = [0x22; SALT_LEN];
        assert_eq!(
            hex(owner.as_bytes()),
            "dcdf47eb67ec1d1f654ecbc615dc44e52ec4e378c80f6d6bd2a3576b0b4aa744"
        );
        assert_eq!(
            hex(owner_signing_seed(&owner).as_bytes()),
            "8664c48cbfceaa2e8c9c2db19b9c317cdee56d0267b5257fe8da4a97fcf549e5"
        );
        assert_eq!(
            hex(&owner_public_key(&owner)),
            "270ab8d654b9e4926215b05fbc4acc040578794154f10d600c7df1dc1cb78a13"
        );
        assert_eq!(
            hex(&directory_id(&owner_public_key(&owner))),
            "645633c9cbe6c2bd137dbf24407ab8c0794e2a85cf9f9e8825141924ada30e3c"
        );
        let leaf = leaf_key(&owner, &salt, 7, 3);
        assert_eq!(
            hex(leaf.as_bytes()),
            "7e44091b9c3819af751bb1b55942f367195bdcf257607e1d98f245b7473e26fc"
        );
        assert_eq!(
            hex(up(&leaf).as_bytes()),
            "a912dad804be17dacb37cdd8dd54cc8b5f14f5e5c60c5232f5cb04fd7a620f3e"
        );
        assert_eq!(
            hex(node_wrap_key(&leaf).as_bytes()),
            "62876c95490ccbbbc372fd384bb9ed93e0d0ec2c52eb6cf28469a638e7f559ea"
        );
    }

    #[test]
    fn contexts_are_distinct() {
        let contexts = [
            OWNER_CONTEXT,
            OWNER_SIGN_CONTEXT,
            DIRECTORY_CONTEXT,
            LEAF_CONTEXT,
            UP_CONTEXT,
            NODE_WRAP_CONTEXT,
            EPOCH_CONTEXT,
            READ_CONTEXT,
            CHAIN_CONTEXT,
            ROW_WRAP_CONTEXT,
            FILTER_SEAL_CONTEXT,
            FILTER_HASH_CONTEXT,
        ];
        let unique: std::collections::BTreeSet<_> = contexts.iter().collect();
        assert_eq!(unique.len(), contexts.len());
    }

    #[test]
    fn the_salt_separates_two_trees_of_one_seed() {
        let owner = owner_seed(&[0x11; KEY_LEN]);
        assert_ne!(
            leaf_key(&owner, &[1; SALT_LEN], 0, 0).as_bytes(),
            leaf_key(&owner, &[2; SALT_LEN], 0, 0).as_bytes()
        );
    }

    #[test]
    fn a_wrap_opens_under_its_key_and_binding_only() {
        let wrapping = SecretBytes::new([3u8; KEY_LEN]);
        let key = SecretBytes::new([4u8; KEY_LEN]);
        let wrap = seal_wrap(&wrapping, b"here", &key).unwrap();
        assert_eq!(open_wrap(&wrapping, b"here", &wrap).unwrap(), key);
        assert_eq!(
            open_wrap(&wrapping, b"there", &wrap),
            Err(CircleError::Authentication)
        );
        let other = SecretBytes::new([5u8; KEY_LEN]);
        assert_eq!(
            open_wrap(&other, b"here", &wrap),
            Err(CircleError::Authentication)
        );
        let mut flipped = wrap;
        flipped[20] ^= 1;
        assert_eq!(
            open_wrap(&wrapping, b"here", &flipped),
            Err(CircleError::Authentication)
        );
    }

    #[test]
    fn a_directory_id_proves_its_owner() {
        let pk = [9u8; KEY_LEN];
        let id = directory_id(&pk);
        assert!(directory_owner_matches(&id, &pk));
        assert!(!directory_owner_matches(&id, &[8u8; KEY_LEN]));
    }
}
