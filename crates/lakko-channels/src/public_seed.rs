// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The public seed envelope of a public channel ([ADR-0087] section 6).
//!
//! A public channel's publisher writes **one** envelope per epoch instead of
//! one per subscriber: the epoch's content-key seed sealed under the
//! channel's 32-byte **read key**, which the channel's public record carries.
//! Anyone who reads the record, the operator included, can open it; that is
//! what public means, and it is no weaker than today, where any registered
//! key is sealed to automatically. A subscriber registers and reads at once,
//! with no publisher online.
//!
//! ## The read key
//!
//! ```text
//! read_key = BLAKE3-derive-key("lakko/channels/read-key/v1", account_seed ‖ channel_id)
//! ```
//!
//! derived from the owner's account seed like the ADR-0059 owner key, so
//! every device of the owner has it with nothing to sync. A co-publisher,
//! which cannot derive it, reads it from the public record.
//!
//! ## Wire (64 bytes, fixed)
//!
//! ```text
//! epoch(4, BE) ‖ nonce(12) ‖ ChaCha20-Poly1305(read_key, seed(32)) ‖ tag(16)
//! AAD = channel_id(16) ‖ epoch(4, BE)
//! ```
//!
//! The epoch in the clear is the one the AAD binds, so an envelope moved to
//! another epoch's path, or another channel, does not open. The service
//! stores it opaquely under `PUT .../public-seed/{epoch}` behind a publisher
//! proof (a subscriber holds the read key, so only the route keeps it from
//! replacing one), and serves `GET .../public-seed/{epoch}` openly.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_crypto::aead::{self, AeadKey, Algorithm, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash;
use lakko_crypto::random;

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::{ContentKeySeed, CONTENT_KEY_SEED_LEN};
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of the read key. Frozen.
pub const READ_KEY_DOMAIN: &str = "lakko/channels/read-key/v1";

/// Length of the read key.
pub const READ_KEY_LEN: usize = 32;

/// Length of a public seed envelope.
pub const PUBLIC_SEED_ENVELOPE_LEN: usize =
    EPOCH_ID_LEN + NONCE_LEN + CONTENT_KEY_SEED_LEN + TAG_LEN;

const ALG: Algorithm = Algorithm::ChaCha20Poly1305;

/// Derive a channel's read key from its owner's account seed.
#[must_use]
pub fn channel_read_key(
    account_seed: &[u8; 32],
    channel_id: &[u8; CHANNEL_ID_LEN],
) -> [u8; READ_KEY_LEN] {
    let mut material = [0u8; 32 + CHANNEL_ID_LEN];
    material[..32].copy_from_slice(account_seed);
    material[32..].copy_from_slice(channel_id);
    let out = hash::blake3_derive_key_from_material(READ_KEY_DOMAIN, &material);
    material.fill(0);
    out
}

fn aad(channel_id: &[u8; CHANNEL_ID_LEN], epoch: EpochId) -> [u8; CHANNEL_ID_LEN + EPOCH_ID_LEN] {
    let mut out = [0u8; CHANNEL_ID_LEN + EPOCH_ID_LEN];
    out[..CHANNEL_ID_LEN].copy_from_slice(channel_id);
    out[CHANNEL_ID_LEN..].copy_from_slice(&epoch.to_be_bytes());
    out
}

/// Seal `seed` for `epoch` under the read key.
///
/// # Errors
///
/// [`ChannelError::AeadSeal`] (not expected with valid inputs).
pub fn seal_public_seed(
    read_key: &[u8; READ_KEY_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    epoch: EpochId,
    seed: &ContentKeySeed,
) -> Result<[u8; PUBLIC_SEED_ENVELOPE_LEN]> {
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let key = AeadKey::from(*read_key);
    let ciphertext = aead::seal(
        ALG,
        &key,
        &nonce,
        &aad(channel_id, epoch),
        seed.as_inner_bytes(),
    )
    .map_err(|_| ChannelError::AeadSeal)?;
    let mut out = [0u8; PUBLIC_SEED_ENVELOPE_LEN];
    out[..EPOCH_ID_LEN].copy_from_slice(&epoch.to_be_bytes());
    out[EPOCH_ID_LEN..EPOCH_ID_LEN + NONCE_LEN].copy_from_slice(&nonce);
    if ciphertext.len() != PUBLIC_SEED_ENVELOPE_LEN - EPOCH_ID_LEN - NONCE_LEN {
        return Err(ChannelError::AeadSeal);
    }
    out[EPOCH_ID_LEN + NONCE_LEN..].copy_from_slice(&ciphertext);
    Ok(out)
}

/// The epoch an envelope names, without opening it. `None` when the length
/// is wrong.
#[must_use]
pub fn public_seed_epoch(wire: &[u8]) -> Option<EpochId> {
    if wire.len() != PUBLIC_SEED_ENVELOPE_LEN {
        return None;
    }
    EpochId::from_be_bytes(&wire[..EPOCH_ID_LEN]).ok()
}

/// Open a public seed envelope fetched for `expected_epoch`.
///
/// # Errors
///
/// * [`ChannelError::Wire`] — not [`PUBLIC_SEED_ENVELOPE_LEN`] bytes, or it
///   names another epoch than the one asked for.
/// * [`ChannelError::AeadOpen`] — another read key, another channel, or a
///   tampered envelope.
pub fn open_public_seed(
    read_key: &[u8; READ_KEY_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    expected_epoch: EpochId,
    wire: &[u8],
) -> Result<ContentKeySeed> {
    let epoch = public_seed_epoch(wire).ok_or(ChannelError::Wire(
        "public seed envelope has the wrong length",
    ))?;
    if epoch != expected_epoch {
        return Err(ChannelError::Wire(
            "public seed envelope names another epoch",
        ));
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&wire[EPOCH_ID_LEN..EPOCH_ID_LEN + NONCE_LEN]);
    let key = AeadKey::from(*read_key);
    let mut plain = aead::open(
        ALG,
        &key,
        &nonce,
        &aad(channel_id, epoch),
        &wire[EPOCH_ID_LEN + NONCE_LEN..],
    )
    .map_err(|_| ChannelError::AeadOpen)?;
    let seed = ContentKeySeed::from_exporter_output(&plain);
    plain.fill(0);
    seed
}

/// Fuzz oracle behind `fuzz_api::public_seed_open`: open `wire` under a
/// fixed key for the epoch it names. `None` when it refuses (a fuzzer cannot
/// forge the tag); never panics.
pub(crate) fn fuzz_open(wire: &[u8]) -> Option<bool> {
    let epoch = public_seed_epoch(wire)?;
    let opened = open_public_seed(&[0x52; READ_KEY_LEN], &[0x0C; CHANNEL_ID_LEN], epoch, wire);
    Some(opened.is_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];

    fn seed() -> ContentKeySeed {
        ContentKeySeed::from_bytes_for_test([0x9A; CONTENT_KEY_SEED_LEN])
    }

    #[test]
    fn a_public_seed_round_trips_at_64_bytes() {
        assert_eq!(PUBLIC_SEED_ENVELOPE_LEN, 64);
        let key = channel_read_key(&[1; 32], &CHANNEL);
        let wire = seal_public_seed(&key, &CHANNEL, EpochId::new(7), &seed()).unwrap();
        assert_eq!(public_seed_epoch(&wire), Some(EpochId::new(7)));
        let opened = open_public_seed(&key, &CHANNEL, EpochId::new(7), &wire).unwrap();
        assert_eq!(
            opened.copy_to_bytes_for_export(),
            seed().copy_to_bytes_for_export()
        );
        assert_ne!(
            seal_public_seed(&key, &CHANNEL, EpochId::new(7), &seed()).unwrap(),
            wire,
            "a fresh nonce per seal"
        );
    }

    #[test]
    fn another_key_channel_or_epoch_does_not_open() {
        let key = channel_read_key(&[1; 32], &CHANNEL);
        let wire = seal_public_seed(&key, &CHANNEL, EpochId::new(7), &seed()).unwrap();
        let other_key = channel_read_key(&[2; 32], &CHANNEL);
        assert!(matches!(
            open_public_seed(&other_key, &CHANNEL, EpochId::new(7), &wire),
            Err(ChannelError::AeadOpen)
        ));
        assert!(matches!(
            open_public_seed(&key, &[0x0D; CHANNEL_ID_LEN], EpochId::new(7), &wire),
            Err(ChannelError::AeadOpen)
        ));
        assert!(matches!(
            open_public_seed(&key, &CHANNEL, EpochId::new(8), &wire),
            Err(ChannelError::Wire(_))
        ));
        // The clear epoch is bound: rewriting it (and asking for it) fails
        // the AEAD.
        let mut moved = wire;
        moved[3] = 8;
        assert!(matches!(
            open_public_seed(&key, &CHANNEL, EpochId::new(8), &moved),
            Err(ChannelError::AeadOpen)
        ));
        assert!(open_public_seed(&key, &CHANNEL, EpochId::new(7), &wire[..63]).is_err());
        assert_eq!(fuzz_open(&wire), Some(true));
    }

    #[test]
    fn the_read_key_is_per_channel_and_per_owner() {
        let a = channel_read_key(&[1; 32], &CHANNEL);
        assert_eq!(a, channel_read_key(&[1; 32], &CHANNEL));
        assert_ne!(a, channel_read_key(&[1; 32], &[0x0D; CHANNEL_ID_LEN]));
        assert_ne!(a, channel_read_key(&[2; 32], &CHANNEL));
        assert_ne!(a, crate::owner::channel_owner_seed(&[1; 32], &CHANNEL));
    }
}
