// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A channel return: what a device of a channel's owner or a publisher sends
//! a subscriber or a publisher who reset their keys, so the channel comes
//! back to them by itself ([ADR-0101] section 7). An owner returns by itself
//! from its own owned-channels list since 2026-11-26 (`lakko_channels::owned`);
//! the owner role (byte 3) of this envelope is removed and refused.
//!
//! ## Where it travels
//!
//! Inside a sealed mailbox row ([`crate::mailbox`], kind
//! [`crate::mailbox::MailboxKind::ChannelReturn`]) written to the reset
//! identity's **continuity mailbox** ([`crate::continuity`]). The row is
//! sealed to the mailbox key and padded, so this envelope has no seal of its
//! own: the Delivery Service sees a padded row, not a channel id.
//!
//! ## What it proves, and what it does not
//!
//! Anyone who knows the reset account's id can write to the continuity
//! mailbox for the window, so the return must not be enough to subscribe
//! someone to a channel. It therefore carries the **old member card's account
//! binding**: the Ed25519 signature the person's previous account identity
//! key made over
//!
//! ```text
//! MEMBER_CARD_BINDING_DOMAIN_V1 ‖ channel_id(16) ‖ old_subscriber_pk(32) ‖ profile_hash(32)
//! ```
//!
//! when it uploaded its member card (ADR-0060 section 3; `profile_hash` is
//! `BLAKE3` of the card's profile bytes). The reset device checks it with
//! [`ChannelReturn::verify_old_card_binding`] against its own previous
//! account key, read from the KT log; it re-subscribes with no prompt only
//! when that verifies, and otherwise shows the return as an ordinary
//! invitation (ADR-0101 section 7). The domain is defined here once and
//! `lakko-channels` uses this constant, since `lakko-channels` depends on
//! this crate and not the other way round.
//!
//! The role is the sender's statement of what the recipient was in the
//! channel. The invitation is opaque here (the clients' `/g/` link material,
//! and for a private channel what its admission needs); the epoch seeds are
//! the newest seeds the sender holds (ADR-0101 section 9), strictly
//! ascending; the profile, when present, is the newest signed profile
//! envelope ([`crate::profile`] v6) the sender holds for the person, which
//! the reset device keeps only if [`crate::profile::verify_profile`] accepts
//! it under its previous account key.
//!
//! ## Wire format (version 1)
//!
//! ```text
//! CHANNEL_RETURN_DOMAIN_V1 ("lakko/channel-return/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ role(1: 1 subscriber, 2 publisher; 3 retired)
//!   ‖ old_binding_sig(64) ‖ old_subscriber_pk(32) ‖ profile_hash(32)
//!   ‖ invitation_len(2 BE) ‖ invitation
//!   ‖ seed_count(1) ‖ seed_count × (epoch(4 BE) ‖ seed(32))
//!   ‖ profile_len(2 BE) ‖ profile
//! ```
//!
//! Bounds: invitation at most [`CHANNEL_RETURN_MAX_INVITATION_LEN`] (chosen,
//! not measured), at most [`CHANNEL_EPOCH_SEEDS_MAX`] seeds strictly
//! ascending by epoch, a profile of 0 (absent) or 1 to
//! [`PROFILE_ENVELOPE_MAX_LEN`] bytes that decodes as a profile. The version
//! byte is checked before any length; every length is checked before the
//! bytes it names are copied; nothing may follow the profile. `compose`
//! refuses exactly what `decode` refuses.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::signing::{Signature, VerifyingKey};
use thiserror::Error;

use crate::channel_epoch_seeds::{
    ChannelEpochSeed, CHANNEL_EPOCH_ID_LEN, CHANNEL_EPOCH_SEEDS_MAX, CHANNEL_EPOCH_SEED_ENTRY_LEN,
    CHANNEL_EPOCH_SEED_LEN,
};
use crate::profile::{decode_profile, ProfileError, PROFILE_ENVELOPE_MAX_LEN};

/// Domain separator. Frozen: the dispatcher matches on it.
pub const CHANNEL_RETURN_DOMAIN_V1: &[u8] = b"lakko/channel-return/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_RETURN_VERSION_V1: u8 = 0x01;

/// Domain separator of a channel member card's account binding (ADR-0060
/// section 3). The one definition: `lakko_channels::member_card` uses it.
/// Frozen.
pub const MEMBER_CARD_BINDING_DOMAIN_V1: &[u8] = b"lakko/channel/member-card-binding/v1";

/// Length of a channel id.
pub const CHANNEL_RETURN_CHANNEL_ID_LEN: usize = 16;

/// Length of the old card binding signature.
pub const CHANNEL_RETURN_SIG_LEN: usize = 64;

/// Length of the old subscriber key and of the profile hash.
pub const CHANNEL_RETURN_KEY_LEN: usize = 32;

/// Longest invitation a return carries. Chosen, not measured: above the
/// sealed `/g/` capability ([`crate::group_invite::MAX_SEALED_LEN`]) with
/// room for the link secret and a private channel's admission material.
pub const CHANNEL_RETURN_MAX_INVITATION_LEN: usize = 4096;

/// Bytes every return carries: the separator, version, channel id, role,
/// signature, subscriber key, profile hash, the three length fields.
pub const CHANNEL_RETURN_MIN_LEN: usize = CHANNEL_RETURN_DOMAIN_V1.len()
    + 1
    + CHANNEL_RETURN_CHANNEL_ID_LEN
    + 1
    + CHANNEL_RETURN_SIG_LEN
    + 2 * CHANNEL_RETURN_KEY_LEN
    + 2
    + 1
    + 2;

/// The longest well-formed return.
pub const CHANNEL_RETURN_MAX_LEN: usize = CHANNEL_RETURN_MIN_LEN
    + CHANNEL_RETURN_MAX_INVITATION_LEN
    + CHANNEL_EPOCH_SEEDS_MAX * CHANNEL_EPOCH_SEED_ENTRY_LEN
    + PROFILE_ENVELOPE_MAX_LEN;

/// What the recipient was in the channel, as the sender states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelReturnRole {
    /// A subscriber: re-subscribe.
    Subscriber,
    /// A publisher: the owner's device admits the new identity's devices on
    /// the PublisherGroup; the Welcome follows separately.
    Publisher,
}

impl ChannelReturnRole {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Subscriber => 1,
            Self::Publisher => 2,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`ChannelReturnError::UnknownRole`] for any other byte, the retired
    /// owner byte 3 included (ADR-0101 section 7, amended 2026-11-26).
    pub const fn from_byte(byte: u8) -> CoreResult<Self, ChannelReturnError> {
        match byte {
            1 => Ok(Self::Subscriber),
            2 => Ok(Self::Publisher),
            other => Err(ChannelReturnError::UnknownRole(other)),
        }
    }
}

/// A channel return. The invitation and the seeds are secret; `Debug`
/// prints neither.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelReturn {
    /// The channel.
    pub channel_id: [u8; CHANNEL_RETURN_CHANNEL_ID_LEN],
    /// What the recipient was in it.
    pub role: ChannelReturnRole,
    /// The old member card's account binding signature.
    pub old_binding_sig: [u8; CHANNEL_RETURN_SIG_LEN],
    /// The subscriber key that card was uploaded under.
    pub old_subscriber_pk: [u8; CHANNEL_RETURN_KEY_LEN],
    /// `BLAKE3` of that card's profile bytes.
    pub profile_hash: [u8; CHANNEL_RETURN_KEY_LEN],
    /// The channel's invitation, opaque here; empty for none.
    pub invitation: Vec<u8>,
    /// Epoch seeds, strictly ascending by epoch, at most
    /// [`CHANNEL_EPOCH_SEEDS_MAX`].
    pub epoch_seeds: Vec<ChannelEpochSeed>,
    /// The newest signed profile envelope the sender holds for the person,
    /// or `None`.
    pub profile: Option<Vec<u8>>,
}

impl fmt::Debug for ChannelReturn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelReturn")
            .field("channel_id", &self.channel_id)
            .field("role", &self.role)
            .field("invitation_len", &self.invitation.len())
            .field("epoch_seeds", &self.epoch_seeds)
            .field("profile_len", &self.profile.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

/// Why a channel return was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelReturnError {
    /// No domain separator. Only from [`decode_channel_return`].
    #[error("plaintext does not carry the channel-return domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("channel return is truncated")]
    Truncated,
    /// Bytes remain after the profile.
    #[error("channel return has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the last field.
        count: usize,
    },
    /// The version byte is not [`CHANNEL_RETURN_VERSION_V1`].
    #[error("channel return carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The role byte is not 1 or 2 (3, the owner, was retired 2026-11-26).
    #[error("channel return names unknown role {0:#04x}")]
    UnknownRole(u8),
    /// The invitation is longer than [`CHANNEL_RETURN_MAX_INVITATION_LEN`].
    #[error("channel return invitation is too long")]
    InvitationTooLong,
    /// More than [`CHANNEL_EPOCH_SEEDS_MAX`] seeds.
    #[error("channel return carries too many epoch seeds")]
    TooManySeeds,
    /// The seeds are not strictly ascending by epoch.
    #[error("channel return epoch seeds are not strictly ascending")]
    SeedsNotAscending,
    /// The profile is present but empty or longer than
    /// [`PROFILE_ENVELOPE_MAX_LEN`].
    #[error("channel return profile has a bad length")]
    ProfileLength,
    /// The profile does not decode as a profile envelope.
    #[error("channel return profile: {0}")]
    Profile(ProfileError),
    /// The previous identity key or the old subscriber key is not a usable
    /// public key.
    #[error("channel return names an unusable key")]
    BadKey,
    /// The old card binding does not verify under the previous identity key.
    #[error("channel return card binding does not verify")]
    BadSignature,
}

impl ChannelReturnError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::UnknownRole(_) => "unknownRole".to_string(),
            Self::InvitationTooLong => "invitationTooLong".to_string(),
            Self::TooManySeeds => "tooManySeeds".to_string(),
            Self::SeedsNotAscending => "seedsNotAscending".to_string(),
            Self::ProfileLength => "profileLength".to_string(),
            Self::Profile(e) => format!("profile.{}", e.code()),
            Self::BadKey => "badKey".to_string(),
            Self::BadSignature => "badSignature".to_string(),
        }
    }
}

/// The bytes an account identity key signs to bind a member card to its
/// account (ADR-0060 section 3), from the hash of the card's profile:
/// `MEMBER_CARD_BINDING_DOMAIN_V1 ‖ channel_id ‖ subscriber_pk ‖ profile_hash`.
#[must_use]
pub fn member_card_binding_bytes_from_hash(
    channel_id: &[u8; CHANNEL_RETURN_CHANNEL_ID_LEN],
    subscriber_pk: &[u8; CHANNEL_RETURN_KEY_LEN],
    profile_hash: &[u8; CHANNEL_RETURN_KEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        MEMBER_CARD_BINDING_DOMAIN_V1.len()
            + CHANNEL_RETURN_CHANNEL_ID_LEN
            + 2 * CHANNEL_RETURN_KEY_LEN,
    );
    out.extend_from_slice(MEMBER_CARD_BINDING_DOMAIN_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out.extend_from_slice(profile_hash);
    out
}

impl ChannelReturn {
    /// Check every bound. The rule both [`compose_channel_return`] and the
    /// decoder run.
    ///
    /// # Errors
    ///
    /// [`ChannelReturnError::InvitationTooLong`],
    /// [`ChannelReturnError::TooManySeeds`],
    /// [`ChannelReturnError::SeedsNotAscending`],
    /// [`ChannelReturnError::ProfileLength`], [`ChannelReturnError::Profile`].
    pub fn validate(&self) -> CoreResult<(), ChannelReturnError> {
        if self.invitation.len() > CHANNEL_RETURN_MAX_INVITATION_LEN {
            return Err(ChannelReturnError::InvitationTooLong);
        }
        if self.epoch_seeds.len() > CHANNEL_EPOCH_SEEDS_MAX {
            return Err(ChannelReturnError::TooManySeeds);
        }
        if self
            .epoch_seeds
            .windows(2)
            .any(|w| w[0].epoch >= w[1].epoch)
        {
            return Err(ChannelReturnError::SeedsNotAscending);
        }
        if let Some(profile) = &self.profile {
            if profile.is_empty() || profile.len() > PROFILE_ENVELOPE_MAX_LEN {
                return Err(ChannelReturnError::ProfileLength);
            }
            decode_profile(profile).map_err(ChannelReturnError::Profile)?;
        }
        Ok(())
    }

    /// Verify the old member card's account binding against the reset
    /// device's own **previous** account identity key (read from the KT
    /// log). Only a return that passes this is acted on without a prompt.
    ///
    /// # Errors
    ///
    /// [`ChannelReturnError::BadKey`], [`ChannelReturnError::BadSignature`].
    pub fn verify_old_card_binding(
        &self,
        previous_identity_pk: &[u8; CHANNEL_RETURN_KEY_LEN],
    ) -> CoreResult<(), ChannelReturnError> {
        let key = VerifyingKey::from_bytes(previous_identity_pk)
            .map_err(|_| ChannelReturnError::BadKey)?;
        if key.is_weak() {
            return Err(ChannelReturnError::BadKey);
        }
        let signature = Signature::from_bytes(&self.old_binding_sig)
            .map_err(|_| ChannelReturnError::BadSignature)?;
        key.verify(
            &member_card_binding_bytes_from_hash(
                &self.channel_id,
                &self.old_subscriber_pk,
                &self.profile_hash,
            ),
            &signature,
        )
        .map_err(|_| ChannelReturnError::BadSignature)
    }
}

/// Compose a channel return.
///
/// # Errors
///
/// Exactly the errors [`ChannelReturn::validate`] returns.
pub fn compose_channel_return(ret: &ChannelReturn) -> CoreResult<Vec<u8>, ChannelReturnError> {
    ret.validate()?;
    let invitation_len =
        u16::try_from(ret.invitation.len()).map_err(|_| ChannelReturnError::InvitationTooLong)?;
    let seed_count =
        u8::try_from(ret.epoch_seeds.len()).map_err(|_| ChannelReturnError::TooManySeeds)?;
    let profile = ret.profile.as_deref().unwrap_or(&[]);
    let profile_len =
        u16::try_from(profile.len()).map_err(|_| ChannelReturnError::ProfileLength)?;
    let mut out = Vec::with_capacity(
        CHANNEL_RETURN_MIN_LEN
            + ret.invitation.len()
            + ret.epoch_seeds.len() * CHANNEL_EPOCH_SEED_ENTRY_LEN
            + profile.len(),
    );
    out.extend_from_slice(CHANNEL_RETURN_DOMAIN_V1);
    out.push(CHANNEL_RETURN_VERSION_V1);
    out.extend_from_slice(&ret.channel_id);
    out.push(ret.role.to_byte());
    out.extend_from_slice(&ret.old_binding_sig);
    out.extend_from_slice(&ret.old_subscriber_pk);
    out.extend_from_slice(&ret.profile_hash);
    out.extend_from_slice(&invitation_len.to_be_bytes());
    out.extend_from_slice(&ret.invitation);
    out.push(seed_count);
    for s in &ret.epoch_seeds {
        out.extend_from_slice(&s.epoch.to_be_bytes());
        out.extend_from_slice(&s.seed);
    }
    out.extend_from_slice(&profile_len.to_be_bytes());
    out.extend_from_slice(profile);
    Ok(out)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], ChannelReturnError> {
    if cur.len() < n {
        return Err(ChannelReturnError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> CoreResult<[u8; N], ChannelReturnError> {
    take(cur, N)?
        .try_into()
        .map_err(|_| ChannelReturnError::Truncated)
}

fn take_u16(cur: &mut &[u8]) -> CoreResult<usize, ChannelReturnError> {
    Ok(usize::from(u16::from_be_bytes(take_array::<2>(cur)?)))
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`ChannelReturnError::UnknownVersion`] (checked first), then every
/// layout and bound error of [`ChannelReturn::validate`],
/// [`ChannelReturnError::UnknownRole`], [`ChannelReturnError::Truncated`],
/// [`ChannelReturnError::TrailingBytes`].
pub fn try_unwrap_channel_return(
    plaintext: &[u8],
) -> CoreResult<Option<ChannelReturn>, ChannelReturnError> {
    let Some(payload) = plaintext.strip_prefix(CHANNEL_RETURN_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;
    let [version] = take_array::<1>(&mut cur)?;
    if version != CHANNEL_RETURN_VERSION_V1 {
        return Err(ChannelReturnError::UnknownVersion(version));
    }
    let channel_id = take_array::<CHANNEL_RETURN_CHANNEL_ID_LEN>(&mut cur)?;
    let [role] = take_array::<1>(&mut cur)?;
    let role = ChannelReturnRole::from_byte(role)?;
    let old_binding_sig = take_array::<CHANNEL_RETURN_SIG_LEN>(&mut cur)?;
    let old_subscriber_pk = take_array::<CHANNEL_RETURN_KEY_LEN>(&mut cur)?;
    let profile_hash = take_array::<CHANNEL_RETURN_KEY_LEN>(&mut cur)?;
    let invitation_len = take_u16(&mut cur)?;
    if invitation_len > CHANNEL_RETURN_MAX_INVITATION_LEN {
        return Err(ChannelReturnError::InvitationTooLong);
    }
    let invitation = take(&mut cur, invitation_len)?.to_vec();
    let [seed_count] = take_array::<1>(&mut cur)?;
    let seed_count = usize::from(seed_count);
    if seed_count > CHANNEL_EPOCH_SEEDS_MAX {
        return Err(ChannelReturnError::TooManySeeds);
    }
    let mut epoch_seeds = Vec::with_capacity(seed_count);
    for _ in 0..seed_count {
        let epoch = u32::from_be_bytes(take_array::<CHANNEL_EPOCH_ID_LEN>(&mut cur)?);
        let seed = take_array::<CHANNEL_EPOCH_SEED_LEN>(&mut cur)?;
        epoch_seeds.push(ChannelEpochSeed { epoch, seed });
    }
    let profile_len = take_u16(&mut cur)?;
    if profile_len > PROFILE_ENVELOPE_MAX_LEN {
        return Err(ChannelReturnError::ProfileLength);
    }
    let profile = take(&mut cur, profile_len)?;
    if !cur.is_empty() {
        return Err(ChannelReturnError::TrailingBytes { count: cur.len() });
    }
    let ret = ChannelReturn {
        channel_id,
        role,
        old_binding_sig,
        old_subscriber_pk,
        profile_hash,
        invitation,
        epoch_seeds,
        profile: if profile.is_empty() {
            None
        } else {
            Some(profile.to_vec())
        },
    };
    ret.validate()?;
    Ok(Some(ret))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ChannelReturnError::WrongDomain`] and every error of
/// [`try_unwrap_channel_return`].
pub fn decode_channel_return(bytes: &[u8]) -> CoreResult<ChannelReturn, ChannelReturnError> {
    try_unwrap_channel_return(bytes)?.ok_or(ChannelReturnError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::channel_return_decode`: `None` when the
/// decoder refuses; `Some(true)` when what decoded re-composes to the same
/// bytes within the advertised bounds, and its card binding never verifies
/// under a fixed key the fuzzer does not hold.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let ret = decode_channel_return(bytes).ok()?;
    let recomposed = compose_channel_return(&ret).ok()?;
    let fixed = lakko_crypto::signing::SigningKey::from_seed(&[0x5E; 32])
        .verifying_key()
        .to_bytes();
    Some(
        recomposed == bytes
            && (CHANNEL_RETURN_MIN_LEN..=CHANNEL_RETURN_MAX_LEN).contains(&bytes.len())
            && ret.verify_old_card_binding(&fixed).is_err(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;
    use proptest::prelude::*;

    const CHANNEL: [u8; 16] = [0xC1; 16];
    const SUB: [u8; 32] = [0x5B; 32];

    fn old_identity() -> SigningKey {
        SigningKey::from_seed(&[0x0D; 32])
    }

    fn sample(profile: Option<Vec<u8>>) -> ChannelReturn {
        let hash = [0xAA; 32];
        let sig = old_identity()
            .sign(&member_card_binding_bytes_from_hash(&CHANNEL, &SUB, &hash))
            .to_bytes();
        ChannelReturn {
            channel_id: CHANNEL,
            role: ChannelReturnRole::Subscriber,
            old_binding_sig: sig,
            old_subscriber_pk: SUB,
            profile_hash: hash,
            invitation: b"/g/AbCdEfGh#secret".to_vec(),
            epoch_seeds: vec![
                ChannelEpochSeed {
                    epoch: 3,
                    seed: [3; 32],
                },
                ChannelEpochSeed {
                    epoch: 9,
                    seed: [9; 32],
                },
            ],
            profile,
        }
    }

    fn signed_profile() -> Vec<u8> {
        crate::profile::compose_profile(
            &crate::profile::Profile {
                display_name: Some("Alice".to_string()),
                username: None,
                updated_at_ms: 1,
                account_id: None,
                username_certificate: None,
                grants: Vec::new(),
                avatar: None,
                card_shareable: true,
            },
            &old_identity(),
        )
        .unwrap()
    }

    #[test]
    fn round_trips_with_and_without_a_profile() {
        for profile in [None, Some(signed_profile())] {
            let ret = sample(profile);
            let wire = compose_channel_return(&ret).unwrap();
            assert_eq!(decode_channel_return(&wire).unwrap(), ret);
            assert_eq!(fuzz_decode(&wire), Some(true));
        }
        let mut empty = sample(None);
        empty.invitation.clear();
        empty.epoch_seeds.clear();
        let wire = compose_channel_return(&empty).unwrap();
        assert_eq!(wire.len(), CHANNEL_RETURN_MIN_LEN);
        assert_eq!(decode_channel_return(&wire).unwrap(), empty);
    }

    #[test]
    fn the_old_card_binding_verifies_only_under_the_previous_key() {
        let ret = sample(None);
        let old = old_identity().verifying_key().to_bytes();
        assert_eq!(ret.verify_old_card_binding(&old), Ok(()));
        let other = SigningKey::from_seed(&[0x0E; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            ret.verify_old_card_binding(&other),
            Err(ChannelReturnError::BadSignature)
        );
        for field in 0..3 {
            let mut bad = ret.clone();
            match field {
                0 => bad.channel_id[0] ^= 1,
                1 => bad.old_subscriber_pk[0] ^= 1,
                _ => bad.profile_hash[0] ^= 1,
            }
            assert_eq!(
                bad.verify_old_card_binding(&old),
                Err(ChannelReturnError::BadSignature)
            );
        }
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            ret.verify_old_card_binding(&weak),
            Err(ChannelReturnError::BadKey)
        );
    }

    #[test]
    fn bounds_are_refused_by_compose_and_decode_alike() {
        let mut long = sample(None);
        long.invitation = vec![0; CHANNEL_RETURN_MAX_INVITATION_LEN + 1];
        assert_eq!(
            compose_channel_return(&long),
            Err(ChannelReturnError::InvitationTooLong)
        );
        let mut desc = sample(None);
        desc.epoch_seeds.reverse();
        assert_eq!(
            compose_channel_return(&desc),
            Err(ChannelReturnError::SeedsNotAscending)
        );
        let mut many = sample(None);
        many.epoch_seeds = (0..=u32::try_from(CHANNEL_EPOCH_SEEDS_MAX).unwrap())
            .map(|epoch| ChannelEpochSeed {
                epoch,
                seed: [1; 32],
            })
            .collect();
        assert_eq!(
            compose_channel_return(&many),
            Err(ChannelReturnError::TooManySeeds)
        );
        let mut bad_profile = sample(Some(b"not a profile".to_vec()));
        assert!(matches!(
            compose_channel_return(&bad_profile),
            Err(ChannelReturnError::Profile(_))
        ));
        bad_profile.profile = Some(Vec::new());
        assert_eq!(
            compose_channel_return(&bad_profile),
            Err(ChannelReturnError::ProfileLength)
        );

        // The same on the wire: descending seeds.
        let mut wire = compose_channel_return(&sample(None)).unwrap();
        let seeds_at = CHANNEL_RETURN_MIN_LEN - 3 + sample(None).invitation.len() + 1;
        wire[seeds_at..seeds_at + 4].copy_from_slice(&10u32.to_be_bytes());
        assert_eq!(
            decode_channel_return(&wire),
            Err(ChannelReturnError::SeedsNotAscending)
        );
    }

    #[test]
    fn version_role_truncation_and_trailing_bytes() {
        let wire = compose_channel_return(&sample(Some(signed_profile()))).unwrap();
        let d = CHANNEL_RETURN_DOMAIN_V1.len();
        let mut v2 = wire.clone();
        v2[d] = 2;
        assert_eq!(
            decode_channel_return(&v2),
            Err(ChannelReturnError::UnknownVersion(2))
        );
        // The version is checked before any length.
        assert_eq!(
            decode_channel_return(&[CHANNEL_RETURN_DOMAIN_V1, &[7]].concat()),
            Err(ChannelReturnError::UnknownVersion(7))
        );
        for retired_or_unknown in [3u8, 4, 0] {
            let mut role = wire.clone();
            role[d + 1 + 16] = retired_or_unknown;
            assert_eq!(
                decode_channel_return(&role),
                Err(ChannelReturnError::UnknownRole(retired_or_unknown))
            );
        }
        for cut in d..wire.len() {
            assert_eq!(
                decode_channel_return(&wire[..cut]),
                Err(ChannelReturnError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire;
        long.push(0);
        assert_eq!(
            decode_channel_return(&long),
            Err(ChannelReturnError::TrailingBytes { count: 1 })
        );
        assert_eq!(try_unwrap_channel_return(b"hello"), Ok(None));
        assert_eq!(
            decode_channel_return(b"hello"),
            Err(ChannelReturnError::WrongDomain)
        );
    }

    #[test]
    fn debug_never_prints_the_invitation_or_a_seed() {
        let shown = format!("{:?}", sample(None));
        assert!(!shown.contains("secret"), "{shown}");
        assert!(!shown.contains("[3, 3"), "{shown}");
    }

    #[test]
    fn it_dispatches_and_does_not_collide_with_its_siblings() {
        let wire = compose_channel_return(&sample(None)).unwrap();
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelReturn(r)) if r == sample(None)
        ));
        for other in [
            crate::channel_epoch_seeds::CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1,
            crate::channel_read_key::CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
            crate::channel_rotate_request::CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::history_request::HISTORY_REQUEST_DOMAIN_V1,
            crate::conversation_reentry::CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(!other.starts_with(CHANNEL_RETURN_DOMAIN_V1));
            assert!(!CHANNEL_RETURN_DOMAIN_V1.starts_with(other));
        }
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            let mut framed = CHANNEL_RETURN_DOMAIN_V1.to_vec();
            framed.extend_from_slice(&bytes);
            if let Some(ok) = fuzz_decode(&framed) {
                prop_assert!(ok);
            }
            let _ = fuzz_decode(&bytes);
        }

        #[test]
        fn any_valid_return_round_trips(
            role in 1u8..=2,
            inv in proptest::collection::vec(any::<u8>(), 0..64),
            epochs in proptest::collection::btree_set(any::<u32>(), 0..8),
        ) {
            let mut ret = sample(None);
            ret.role = ChannelReturnRole::from_byte(role).unwrap();
            ret.invitation = inv;
            ret.epoch_seeds = epochs.into_iter().map(|epoch| ChannelEpochSeed { epoch, seed: [7; 32] }).collect();
            let wire = compose_channel_return(&ret).unwrap();
            prop_assert_eq!(decode_channel_return(&wire).unwrap(), ret);
        }
    }
}
