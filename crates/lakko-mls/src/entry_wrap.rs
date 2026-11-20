// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! An external-entry commit as it reaches the Delivery Service: wrapped
//! under a key every member already holds ([ADR-0078] section 4, last
//! bullet; G-55).
//!
//! An external commit is a `PublicMessage` (RFC 9420): its update-path leaf,
//! and with it the entering device's ADR-0050 binding naming the owner's
//! account key, is readable by anyone who reads the journal row. A
//! PublisherGroup journal id is derived to hide that account (ADR-0078 phase
//! 6), so the row must not carry it in the clear. The row is therefore
//!
//! ```text
//! WRAPPED_ENTRY_MAGIC ‖ version(1) ‖ epoch(8 BE) ‖ nonce(12) ‖ AEAD(commit)
//! ```
//!
//! ChaCha20-Poly1305 under
//! `BLAKE3-derive-key("lakko/ds/entry-wrap/v1", ds_access_seed_N)`, where
//! `ds_access_seed_N` is the Delivery Service access seed of the epoch the
//! commit leaves (`export_secret("lakko/ds/access/v1", group_id, 32)`). The
//! entering device holds that seed from the vault head it entered from; every
//! member derives it from its exporter. The AAD binds the magic, the version,
//! the group id and the epoch, so a row cannot be replayed into another group
//! or read as another epoch's entry.
//!
//! The epoch rides in the clear: every other journal row carries it in its
//! MLS framing already, and a joiner reading the journal head needs it
//! ([`crate::epoch_of_message`] reads it from a wrapped row too).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use lakko_crypto::aead::{self, AeadKey, Algorithm, NONCE_LEN, TAG_LEN};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// The MLS exporter label of the Delivery Service access seed (ADR-0053):
/// `export_secret(DS_ACCESS_EXPORTER_LABEL, group_id, 32)`. The same string as
/// `lakko_ds_proof::ACCESS_KEY_LABEL` (the FFI crates pin the equality).
pub const DS_ACCESS_EXPORTER_LABEL: &str = "lakko/ds/access/v1";

/// BLAKE3 derive-key context of the entry wrap key. Frozen.
pub const ENTRY_WRAP_KEY_CONTEXT: &str = "lakko/ds/entry-wrap/v1";

/// First bytes of a wrapped entry row. Frozen. An MLS message starts with its
/// protocol version (`0x00 0x01`), so the two never collide.
pub const WRAPPED_ENTRY_MAGIC: &[u8] = b"lakko/wrapped-entry/v1\0";

/// The one wrapped-entry version.
pub const WRAPPED_ENTRY_VERSION: u8 = 0x01;

const HEADER_LEN: usize = WRAPPED_ENTRY_MAGIC.len() + 1 + 8;

/// Shortest well-formed wrapped row: the header, the nonce and a tag over an
/// empty commit.
pub const WRAPPED_ENTRY_MIN_LEN: usize = HEADER_LEN + NONCE_LEN + TAG_LEN;

/// The wrap key of `access_seed` (the epoch-N Delivery Service access seed).
fn wrap_key(access_seed: &[u8; 32]) -> AeadKey {
    AeadKey::new(lakko_crypto::hash::blake3_derive_key_from_material(
        ENTRY_WRAP_KEY_CONTEXT,
        access_seed,
    ))
}

fn aad(group_id: &[u8], epoch: u64) -> Result<Vec<u8>> {
    let len = u8::try_from(group_id.len())
        .map_err(|_| Error::Codec("wrapped entry: group id longer than 255 bytes".into()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + 1 + group_id.len());
    out.extend_from_slice(WRAPPED_ENTRY_MAGIC);
    out.push(WRAPPED_ENTRY_VERSION);
    out.push(len);
    out.extend_from_slice(group_id);
    out.extend_from_slice(&epoch.to_be_bytes());
    Ok(out)
}

/// `true` when `bytes` start with [`WRAPPED_ENTRY_MAGIC`]: a row the caller
/// must unwrap before processing (and must not process raw).
#[must_use]
pub fn is_wrapped_entry(bytes: &[u8]) -> bool {
    bytes.starts_with(WRAPPED_ENTRY_MAGIC)
}

/// The epoch the wrapped commit leaves, read from the clear header; `None`
/// when `bytes` are not a well-formed wrapped row of the known version.
#[must_use]
pub fn wrapped_entry_epoch(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < WRAPPED_ENTRY_MIN_LEN || !is_wrapped_entry(bytes) {
        return None;
    }
    let rest = bytes.get(WRAPPED_ENTRY_MAGIC.len()..)?;
    if *rest.first()? != WRAPPED_ENTRY_VERSION {
        return None;
    }
    let epoch: [u8; 8] = rest.get(1..9)?.try_into().ok()?;
    Some(u64::from_be_bytes(epoch))
}

/// Wrap `commit` (the external commit leaving `epoch` of `group_id`) under the
/// key derived from `access_seed`, the Delivery Service access seed of
/// `epoch`.
///
/// # Errors
///
/// [`Error::Codec`] for a group id longer than 255 bytes; [`Error::Group`] on
/// an AEAD failure.
pub fn wrap_entry_commit(
    access_seed: &[u8; 32],
    group_id: &[u8],
    epoch: u64,
    commit: &[u8],
) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    let sealed = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &wrap_key(access_seed),
        &nonce,
        &aad(group_id, epoch)?,
        commit,
    )
    .map_err(|e| Error::Group(format!("wrapped entry: seal: {e}")))?;
    let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + sealed.len());
    out.extend_from_slice(WRAPPED_ENTRY_MAGIC);
    out.push(WRAPPED_ENTRY_VERSION);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open a wrapped entry row read from `group_id`'s journal by a member at
/// `expected_epoch`, under the key derived from that epoch's access seed.
/// Returns the inner commit, still to be processed through the group's gate.
///
/// # Errors
///
/// [`Error::Message`] when the row is not a wrapped entry of the known
/// version, names another epoch, or does not open (another key, another group,
/// tampered bytes). Never returns the bytes unopened.
pub fn unwrap_entry_commit(
    access_seed: &[u8; 32],
    group_id: &[u8],
    expected_epoch: u64,
    bytes: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let epoch = wrapped_entry_epoch(bytes)
        .ok_or_else(|| Error::Message("wrapped entry: malformed row".into()))?;
    if epoch != expected_epoch {
        return Err(Error::Message(format!(
            "wrapped entry: the row leaves epoch {epoch}, this group is at {expected_epoch}"
        )));
    }
    let nonce: [u8; NONCE_LEN] = bytes
        .get(HEADER_LEN..HEADER_LEN + NONCE_LEN)
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| Error::Message("wrapped entry: malformed row".into()))?;
    let sealed = bytes
        .get(HEADER_LEN + NONCE_LEN..)
        .ok_or_else(|| Error::Message("wrapped entry: malformed row".into()))?;
    aead::open(
        Algorithm::ChaCha20Poly1305,
        &wrap_key(access_seed),
        &nonce,
        &aad(group_id, epoch)?,
        sealed,
    )
    .map(Zeroizing::new)
    .map_err(|_| {
        Error::Message("wrapped entry: the row does not open under this epoch's key".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn constants_are_pinned() {
        assert_eq!(DS_ACCESS_EXPORTER_LABEL, "lakko/ds/access/v1");
        assert_eq!(ENTRY_WRAP_KEY_CONTEXT, "lakko/ds/entry-wrap/v1");
        assert_eq!(WRAPPED_ENTRY_MAGIC, b"lakko/wrapped-entry/v1\0");
        assert_eq!(WRAPPED_ENTRY_MIN_LEN, 23 + 1 + 8 + 12 + 16);
    }

    #[test]
    fn short_or_foreign_rows_have_no_epoch() {
        assert_eq!(wrapped_entry_epoch(&[]), None);
        assert_eq!(wrapped_entry_epoch(&[0, 1, 0, 2]), None);
        assert_eq!(wrapped_entry_epoch(WRAPPED_ENTRY_MAGIC), None);
        let mut wrong_version = WRAPPED_ENTRY_MAGIC.to_vec();
        wrong_version.push(0x02);
        wrong_version.resize(WRAPPED_ENTRY_MIN_LEN, 0);
        assert_eq!(wrapped_entry_epoch(&wrong_version), None);
    }

    proptest! {
        #[test]
        fn round_trip_and_every_binding(
            seed in any::<[u8; 32]>(),
            other_seed in any::<[u8; 32]>(),
            group in proptest::collection::vec(any::<u8>(), 1..64),
            epoch in any::<u64>(),
            commit in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let row = wrap_entry_commit(&seed, &group, epoch, &commit).unwrap();
            prop_assert_eq!(wrapped_entry_epoch(&row), Some(epoch));
            let opened = unwrap_entry_commit(&seed, &group, epoch, &row).unwrap();
            prop_assert_eq!(opened.as_slice(), commit.as_slice());
            if other_seed != seed {
                prop_assert!(unwrap_entry_commit(&other_seed, &group, epoch, &row).is_err());
            }
            prop_assert!(unwrap_entry_commit(&seed, &group, epoch.wrapping_add(1), &row).is_err());
            let mut other_group = group.clone();
            other_group[0] ^= 1;
            prop_assert!(unwrap_entry_commit(&seed, &other_group, epoch, &row).is_err());
            // The clear epoch is bound: rewriting it does not open.
            let mut moved = row.clone();
            let at = WRAPPED_ENTRY_MAGIC.len() + 1;
            moved[at + 7] ^= 1;
            let moved_epoch = wrapped_entry_epoch(&moved).unwrap();
            prop_assert!(unwrap_entry_commit(&seed, &group, moved_epoch, &moved).is_err());
        }

        #[test]
        fn the_fuzz_entry_point_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = crate::fuzz_api::open_wrapped_entry(&bytes);
            let _ = crate::fuzz_api::open_rejoin_knock(&bytes);
        }

        #[test]
        fn arbitrary_bytes_never_open(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let mut row = WRAPPED_ENTRY_MAGIC.to_vec();
            row.push(WRAPPED_ENTRY_VERSION);
            row.extend_from_slice(&bytes);
            let epoch = wrapped_entry_epoch(&row).unwrap_or(0);
            prop_assert!(unwrap_entry_commit(&[7u8; 32], b"g", epoch, &row).is_err());
        }
    }
}
