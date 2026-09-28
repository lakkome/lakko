// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel member card ([ADR-0058] section 3, "Channels get a member list").
//!
//! A subscriber (publishers too) uploads a **member card** to
//! `services/channels`: its profile envelope ([ADR-0048] v3, opaque here),
//! sealed under the channel's current epoch seed so every holder of that seed —
//! every current subscriber and publisher — can open it and the server cannot.
//! The server stores one card per subscriber key it already holds.
//!
//! ## Keyed per subscriber
//!
//! The card key is derived per `(channel, epoch, subscriber)`, and the AEAD AAD
//! binds the same three plus the version, so the channels server cannot swap two
//! subscribers' cards (or move a card between channels or epochs): a card served
//! under the wrong subscriber key fails the authenticator and is refused, never
//! shown under someone else's name.
//!
//! ```text
//! card_key = HKDF-SHA-256(ikm  = epoch seed,
//!                         salt = channel_id(16) ‖ epoch_id(4 BE) ‖ subscriber_pk(32),
//!                         info = "lakko/channel/member-card/v1")
//! wire     = version(1)=0x01 ‖ epoch_id(4 BE) ‖ nonce(12) ‖ ciphertext(profile + 16-byte tag)
//! aad      = version ‖ epoch_id(4 BE) ‖ channel_id(16) ‖ subscriber_pk(32)
//! ```
//!
//! **HKDF, not BLAKE3.** The key follows this crate's own convention (the
//! [`post`](crate::post) per-message key is HKDF-SHA-256 with the seed as IKM
//! and the binding fields as salt), so a reviewer reads one derivation style
//! across the crate. The info label is distinct from the post labels
//! (`lakko/channel/v1/msg-key`, `lakko/channel/v1/msg-nonce`), so a card key
//! never equals a post key.
//!
//! **Random nonce.** A card is re-uploaded whenever the profile changes, so the
//! same `(channel, epoch, subscriber)` key can seal several plaintexts; the
//! nonce is 96 fresh random bits per seal and rides in the clear. The number of
//! seals under one key is a handful of profile edits within one epoch, far below
//! the random-nonce birthday bound.
//!
//! **Epoch in the clear.** A reader holds several epoch seeds (ADR-0031
//! retention), so the epoch rides in the clear for [`member_card_epoch`] to pick
//! the seed. It is also in the salt and the AAD, so a relabelled epoch fails.
//!
//! **Size.** The profile is at most [`MAX_MEMBER_CARD_PROFILE_LEN`] = 1024
//! bytes. The value is chosen, not measured: it is above the ADR-0048 profile
//! envelope's current maximum with room for the v3 grant (a 153-byte username
//! certificate plus reach seeds), and small enough to bound a paged member-list
//! response.
//!
//! [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
//! [ADR-0048]: ../../../docs/adr/0048-profile-names.md

use lakko_crypto::aead::{self, Algorithm, KEY_LEN, NONCE_LEN};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::PUBLIC_KEY_LEN;

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::ContentKeySeed;
use crate::CHANNEL_ID_LEN;

/// Wire-format version of a member card. The opener refuses any other value.
pub const MEMBER_CARD_VERSION_V1: u8 = 1;

/// HKDF info label for the per-subscriber card key, frozen at v1.
pub const MEMBER_CARD_KEY_INFO_V1: &[u8] = b"lakko/channel/member-card/v1";

/// Maximum length of the sealed profile, in bytes. Chosen, not measured (see
/// the module docs).
pub const MAX_MEMBER_CARD_PROFILE_LEN: usize = 1024;

/// Length of the subscriber key a card is bound to (the 32-byte key the
/// subscriber registered with `services/channels`).
pub const SUBSCRIBER_PK_LEN: usize = PUBLIC_KEY_LEN;

/// Header length: `version + epoch_id + nonce`.
const HEADER_LEN: usize = 1 + EPOCH_ID_LEN + NONCE_LEN;

/// Smallest valid card: header plus the tag of an empty profile.
pub const MEMBER_CARD_MIN_LEN: usize = HEADER_LEN + aead::TAG_LEN;

/// Largest valid card: header plus a maximal profile plus its tag.
pub const MEMBER_CARD_MAX_LEN: usize = MEMBER_CARD_MIN_LEN + MAX_MEMBER_CARD_PROFILE_LEN;

/// Seal `profile` into a member card for `subscriber_pk` in `channel_id` under
/// the seed of `epoch`.
///
/// # Errors
///
/// - [`ChannelError::MemberCardTooLong`] if `profile` exceeds
///   [`MAX_MEMBER_CARD_PROFILE_LEN`].
/// - [`ChannelError::KeyDerivation`] on HKDF failure (impossible with these
///   lengths, surfaced for completeness).
/// - [`ChannelError::AeadSeal`] on AEAD failure (likewise).
pub fn seal_member_card(
    epoch_seed: &ContentKeySeed,
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    profile: &[u8],
) -> Result<Vec<u8>> {
    if profile.len() > MAX_MEMBER_CARD_PROFILE_LEN {
        return Err(ChannelError::MemberCardTooLong {
            max: MAX_MEMBER_CARD_PROFILE_LEN,
            got: profile.len(),
        });
    }
    let key = card_key(epoch_seed, epoch, channel_id, subscriber_pk)?;
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aad = card_aad(epoch, channel_id, subscriber_pk);
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &aad, profile)
        .map_err(|_| ChannelError::AeadSeal)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.push(MEMBER_CARD_VERSION_V1);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Read the clear epoch of a member card, so a reader can pick the matching
/// seed before [`open_member_card`]. The epoch is not authenticated until the
/// card opens.
///
/// # Errors
///
/// [`ChannelError::Wire`] if the card is shorter than [`MEMBER_CARD_MIN_LEN`],
/// longer than [`MEMBER_CARD_MAX_LEN`], or carries an unknown version.
pub fn member_card_epoch(bytes: &[u8]) -> Result<EpochId> {
    check_frame(bytes)?;
    EpochId::from_be_bytes(&bytes[1..=EPOCH_ID_LEN])
}

/// Open a member card sealed for `subscriber_pk` in `channel_id`, under the
/// seed of the epoch the card names. Fail-closed: a card sealed for another
/// subscriber, channel or epoch, or tampered in any byte, does not open.
///
/// # Errors
///
/// - [`ChannelError::Wire`] on a malformed frame (see [`member_card_epoch`]).
/// - [`ChannelError::KeyDerivation`] on HKDF failure (impossible).
/// - [`ChannelError::AeadOpen`] if the authenticator refuses the card (wrong
///   seed, wrong subscriber / channel / epoch binding, or tampered bytes).
pub fn open_member_card(
    epoch_seed: &ContentKeySeed,
    bytes: &[u8],
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<Vec<u8>> {
    let epoch = member_card_epoch(bytes)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[1 + EPOCH_ID_LEN..HEADER_LEN]);
    let key = card_key(epoch_seed, epoch, channel_id, subscriber_pk)?;
    let aad = card_aad(epoch, channel_id, subscriber_pk);
    aead::open(
        Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &aad,
        &bytes[HEADER_LEN..],
    )
    .map_err(|_| ChannelError::AeadOpen)
}

// ----------------------------------------------------------------
// Internal helpers — one source of truth for the frame, key and AAD.
// ----------------------------------------------------------------

fn check_frame(bytes: &[u8]) -> Result<()> {
    if bytes.len() < MEMBER_CARD_MIN_LEN {
        return Err(ChannelError::Wire("member card shorter than minimum"));
    }
    if bytes.len() > MEMBER_CARD_MAX_LEN {
        return Err(ChannelError::Wire("member card longer than maximum"));
    }
    if bytes[0] != MEMBER_CARD_VERSION_V1 {
        return Err(ChannelError::Wire("unknown member card version"));
    }
    Ok(())
}

fn card_key(
    epoch_seed: &ContentKeySeed,
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<SecretBytes<KEY_LEN>> {
    let mut salt = Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + SUBSCRIBER_PK_LEN);
    salt.extend_from_slice(channel_id);
    salt.extend_from_slice(&epoch.to_be_bytes());
    salt.extend_from_slice(subscriber_pk);
    hkdf_sha256::<KEY_LEN>(epoch_seed.as_inner_bytes(), &salt, MEMBER_CARD_KEY_INFO_V1)
        .map_err(|_| ChannelError::KeyDerivation)
}

fn card_aad(
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + EPOCH_ID_LEN + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN);
    aad.push(MEMBER_CARD_VERSION_V1);
    aad.extend_from_slice(&epoch.to_be_bytes());
    aad.extend_from_slice(channel_id);
    aad.extend_from_slice(subscriber_pk);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const ALICE: [u8; SUBSCRIBER_PK_LEN] = [0xA1; SUBSCRIBER_PK_LEN];
    const BOB: [u8; SUBSCRIBER_PK_LEN] = [0xB0; SUBSCRIBER_PK_LEN];

    fn seed() -> ContentKeySeed {
        ContentKeySeed::from_bytes_for_test([0x5E; 32])
    }

    fn card(profile: &[u8]) -> Vec<u8> {
        seal_member_card(&seed(), EpochId::new(7), &CHANNEL, &ALICE, profile).unwrap()
    }

    #[test]
    fn round_trip() {
        let profile = b"profile envelope bytes";
        let wire = card(profile);
        assert_eq!(wire.len(), HEADER_LEN + profile.len() + aead::TAG_LEN);
        assert_eq!(wire[0], MEMBER_CARD_VERSION_V1);
        assert_eq!(member_card_epoch(&wire).unwrap(), EpochId::new(7));
        assert_eq!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE).unwrap(),
            profile
        );
    }

    #[test]
    fn empty_and_maximal_profiles_round_trip() {
        for len in [0, MAX_MEMBER_CARD_PROFILE_LEN] {
            let profile = vec![0x42; len];
            let wire = card(&profile);
            assert!(wire.len() >= MEMBER_CARD_MIN_LEN && wire.len() <= MEMBER_CARD_MAX_LEN);
            assert_eq!(
                open_member_card(&seed(), &wire, &CHANNEL, &ALICE).unwrap(),
                profile
            );
        }
    }

    #[test]
    fn two_seals_of_one_profile_differ() {
        // Fresh nonce per seal: re-uploading the same profile is not linkable
        // byte-for-byte, and a key is never reused with one nonce.
        assert_ne!(card(b"same"), card(b"same"));
    }

    #[test]
    fn a_card_does_not_open_for_another_subscriber() {
        let wire = card(b"alice");
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &BOB),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_card_does_not_open_in_another_channel() {
        let wire = card(b"alice");
        let other = [0x0D; CHANNEL_ID_LEN];
        assert!(matches!(
            open_member_card(&seed(), &wire, &other, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_card_does_not_open_under_another_seed() {
        let wire = card(b"alice");
        let other = ContentKeySeed::from_bytes_for_test([0x5F; 32]);
        assert!(matches!(
            open_member_card(&other, &wire, &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_relabelled_epoch_does_not_open() {
        let mut wire = card(b"alice");
        wire[1..=EPOCH_ID_LEN].copy_from_slice(&EpochId::new(8).to_be_bytes());
        assert_eq!(member_card_epoch(&wire).unwrap(), EpochId::new(8));
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn tampering_any_byte_is_refused() {
        let wire = card(b"alice's profile");
        for idx in 0..wire.len() {
            let mut bad = wire.clone();
            bad[idx] ^= 0x01;
            assert!(
                open_member_card(&seed(), &bad, &CHANNEL, &ALICE).is_err(),
                "tampering byte {idx} must be refused"
            );
        }
    }

    #[test]
    fn truncated_and_oversize_cards_are_refused() {
        let wire = card(b"alice");
        for len in 0..MEMBER_CARD_MIN_LEN {
            assert!(matches!(
                member_card_epoch(&wire[..len.min(wire.len())]),
                Err(ChannelError::Wire(_))
            ));
        }
        // A truncated tail that still meets the minimum fails the authenticator.
        assert!(matches!(
            open_member_card(&seed(), &wire[..wire.len() - 1], &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
        let mut long = card(&[0u8; MAX_MEMBER_CARD_PROFILE_LEN]);
        long.push(0);
        assert!(matches!(
            open_member_card(&seed(), &long, &CHANNEL, &ALICE),
            Err(ChannelError::Wire(_))
        ));
    }

    #[test]
    fn oversize_profile_is_refused_at_seal() {
        let profile = vec![0u8; MAX_MEMBER_CARD_PROFILE_LEN + 1];
        assert!(matches!(
            seal_member_card(&seed(), EpochId::new(7), &CHANNEL, &ALICE, &profile),
            Err(ChannelError::MemberCardTooLong { max, got })
                if max == MAX_MEMBER_CARD_PROFILE_LEN && got == MAX_MEMBER_CARD_PROFILE_LEN + 1
        ));
    }

    #[test]
    fn unknown_version_is_refused() {
        let mut wire = card(b"alice");
        wire[0] = 2;
        assert!(matches!(
            member_card_epoch(&wire),
            Err(ChannelError::Wire(_))
        ));
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE),
            Err(ChannelError::Wire(_))
        ));
    }
}
