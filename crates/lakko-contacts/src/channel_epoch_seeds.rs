// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Earlier channel seeds to a new publisher ([ADR-0087] section 7, amended
//! 2026-11-08; UX `docs/ux/group-and-channel-privacy.md` section 15.2.1).
//!
//! ## Why a carrier is needed
//!
//! A person added as a publisher joins the channel's PublisherGroup at a
//! later MLS epoch, and MLS gives a newcomer nothing from before. A channel
//! post is sealed under the content-key seed of the channel epoch it was
//! written in, so the new publisher could open no earlier post, not even the
//! owner's epoch-0 channel record. The owner's device holds the seeds of the
//! earlier epochs in its local epoch map; it seals the newest of them to the
//! new publisher's account key, beside the read key
//! ([`crate::channel_read_key`]), on the same PublisherGroup journal.
//!
//! ## Wire format (v1)
//!
//! ```text
//! CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1 ("lakko/channel-epoch-seeds/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ member_account_key(32) ‖ count(1)
//!   ‖ sealed(48 + 36 × count)
//! sealed = eph_pub(32) ‖ ChaCha20-Poly1305(count × (epoch_id(4 BE) ‖ seed(32)))
//!          ‖ tag(16),   AAD = every byte before it
//! ```
//!
//! `count` is 1 to [`CHANNEL_EPOCH_SEEDS_MAX`] and sits in the clear, so the
//! length is checked exactly before anything is opened. The entries are
//! strictly ascending by epoch; after opening, the count and the order are
//! checked again and anything else is refused whole. The sealed box is
//! [`crate::envelope`]'s, sealed once for the whole list.
//!
//! ## Why 64
//!
//! Chosen, not measured. The channels service keeps a subscriber's
//! envelopes for the newest 4 epochs (ADR-0031), which bounds what a
//! subscriber fetches, not what the owner holds: the owner's device keeps
//! every seed it filed. Posts are kept 30 days; at the default rotation of
//! one a day plus an epoch per publisher change, a channel's live posts span
//! about 31 epochs. 64 covers that with room and keeps one envelope at most
//! [`CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN`] (2,431) bytes. An owner holding
//! more sends the newest 64.
//!
//! ## The receiver's rule
//!
//! A device keeps the seeds only when all of these hold, judged at receipt
//! (C-68):
//!
//! 1. it arrived on the PublisherGroup journal of the channel `channel_id`
//!    names, and this device publishes that channel;
//! 2. `member_account_key` is this device's own account key;
//! 3. the MLS sender resolves to the channel's owner;
//! 4. the box opens and the plaintext is well formed.
//!
//! It adds a seed only for an epoch it holds none of and never replaces one
//! it holds. Anything else is dropped whole.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};

/// The sealed-box labels of this envelope: its own, so the box opens as no
/// other sealed-to-account envelope and no contact request.
pub(crate) const SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/sealed/channel-epoch-seeds/v1/key",
    nonce: b"lakko/sealed/channel-epoch-seeds/v1/nonce",
};

/// 29-byte domain separator prepended to the envelope. Frozen: the
/// dispatcher matches on it.
pub const CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/channel-epoch-seeds/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_EPOCH_SEEDS_VERSION_V1: u8 = 0x01;

/// Length of a channel id.
pub const CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN: usize = 16;

/// Length of one channel content-key seed
/// (`lakko_channels::seed::CONTENT_KEY_SEED_LEN`).
pub const CHANNEL_EPOCH_SEED_LEN: usize = 32;

/// Length of one channel epoch id (`lakko_channels::epoch::EPOCH_ID_LEN`).
pub const CHANNEL_EPOCH_ID_LEN: usize = 4;

/// Length of one sealed entry: epoch id and seed.
pub const CHANNEL_EPOCH_SEED_ENTRY_LEN: usize = CHANNEL_EPOCH_ID_LEN + CHANNEL_EPOCH_SEED_LEN;

/// Most seeds one envelope carries.
pub const CHANNEL_EPOCH_SEEDS_MAX: usize = 64;

/// Sealed-box overhead: the ephemeral key and the tag.
const SEAL_OVERHEAD: usize = EPH_PUB_LEN + TAG_LEN;

const CHANNEL_AT: usize = 1;
const MEMBER_AT: usize = CHANNEL_AT + CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN;
const COUNT_AT: usize = MEMBER_AT + 32;
const SEALED_AT: usize = COUNT_AT + 1;

/// Bytes after the separator and before the sealed box.
const PAYLOAD_HEADER_LEN: usize = SEALED_AT;

/// Length of the whole envelope for `count` seeds.
#[must_use]
pub const fn channel_epoch_seeds_envelope_len(count: usize) -> usize {
    CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.len()
        + PAYLOAD_HEADER_LEN
        + SEAL_OVERHEAD
        + count * CHANNEL_EPOCH_SEED_ENTRY_LEN
}

/// Length of an envelope carrying one seed.
pub const CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN: usize = channel_epoch_seeds_envelope_len(1);

/// Length of an envelope carrying [`CHANNEL_EPOCH_SEEDS_MAX`] seeds.
pub const CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN: usize =
    channel_epoch_seeds_envelope_len(CHANNEL_EPOCH_SEEDS_MAX);

/// One channel epoch's content-key seed. Secret: `Debug` prints the epoch
/// only, and the seed is overwritten on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelEpochSeed {
    /// The channel epoch id the seed serves.
    pub epoch: u32,
    /// The 32-byte content-key seed of that epoch.
    pub seed: [u8; CHANNEL_EPOCH_SEED_LEN],
}

impl fmt::Debug for ChannelEpochSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelEpochSeed")
            .field("epoch", &self.epoch)
            .field("seed", &"<redacted>")
            .finish()
    }
}

impl Drop for ChannelEpochSeed {
    fn drop(&mut self) {
        self.seed.fill(0);
    }
}

/// A decoded epoch-seeds envelope. The seeds are still sealed:
/// [`Self::open`] with the addressee's account X25519 secret recovers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEpochSeedsEnvelope {
    /// The channel the seeds belong to.
    pub channel_id: [u8; CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN],
    /// The addressee's account Ed25519 public key.
    pub member_account_key: [u8; 32],
    /// How many seeds the sealed box carries, 1 to
    /// [`CHANNEL_EPOCH_SEEDS_MAX`].
    pub count: u8,
    /// The sealed list, exactly `48 + 36 × count` bytes.
    pub sealed: Vec<u8>,
}

/// Why an epoch-seeds envelope was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelEpochSeedsError {
    /// No domain separator. Only from [`decode_channel_epoch_seeds`].
    #[error("plaintext does not carry the channel epoch-seeds domain separator")]
    WrongDomain,
    /// The payload is not exactly the length its count names.
    #[error("channel epoch-seeds envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`CHANNEL_EPOCH_SEEDS_VERSION_V1`].
    #[error("channel epoch-seeds envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The count is 0 or above [`CHANNEL_EPOCH_SEEDS_MAX`].
    #[error("channel epoch-seeds envelope carries a count out of range")]
    BadCount,
    /// Compose was given the same epoch twice.
    #[error("channel epoch-seeds list names an epoch twice")]
    DuplicateEpoch,
    /// Sealing the list failed.
    #[error("channel epoch seeds could not be sealed")]
    Seal,
    /// The sealed list did not open under the given key.
    #[error("channel epoch seeds did not open")]
    Open,
    /// The opened list is not the count's length or is not strictly
    /// ascending by epoch.
    #[error("opened channel epoch-seeds list is malformed")]
    Malformed,
}

impl ChannelEpochSeedsError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadCount => "badCount",
            Self::DuplicateEpoch => "duplicateEpoch",
            Self::Seal => "seal",
            Self::Open => "open",
            Self::Malformed => "malformed",
        }
    }
}

fn header(
    channel_id: &[u8; CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
    count: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(channel_epoch_seeds_envelope_len(usize::from(count)));
    out.extend_from_slice(CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_EPOCH_SEEDS_VERSION_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(member_account_key);
    out.push(count);
    out
}

fn count_in_range(count: usize) -> CoreResult<u8, ChannelEpochSeedsError> {
    if count == 0 || count > CHANNEL_EPOCH_SEEDS_MAX {
        return Err(ChannelEpochSeedsError::BadCount);
    }
    u8::try_from(count).map_err(|_| ChannelEpochSeedsError::BadCount)
}

/// Compose the envelope handed to the MLS application-message send path on
/// the channel's PublisherGroup journal. `seeds` may come in any order; they
/// are sealed sorted by epoch. `member_x25519_pub` is the publisher
/// account's X25519 public key (its identity bundle's `x25519_pubkey`).
///
/// # Errors
///
/// [`ChannelEpochSeedsError::BadCount`] for no seed or more than
/// [`CHANNEL_EPOCH_SEEDS_MAX`], [`ChannelEpochSeedsError::DuplicateEpoch`]
/// for an epoch named twice, [`ChannelEpochSeedsError::Seal`] if the sealed
/// box fails (not expected).
pub fn compose_channel_epoch_seeds(
    channel_id: &[u8; CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
    member_x25519_pub: &X25519PublicKey,
    seeds: &[ChannelEpochSeed],
) -> CoreResult<Vec<u8>, ChannelEpochSeedsError> {
    let count = count_in_range(seeds.len())?;
    let mut order: Vec<&ChannelEpochSeed> = seeds.iter().collect();
    order.sort_by_key(|s| s.epoch);
    if order.windows(2).any(|w| w[0].epoch == w[1].epoch) {
        return Err(ChannelEpochSeedsError::DuplicateEpoch);
    }
    let mut plain = Vec::with_capacity(seeds.len() * CHANNEL_EPOCH_SEED_ENTRY_LEN);
    for s in order {
        plain.extend_from_slice(&s.epoch.to_be_bytes());
        plain.extend_from_slice(&s.seed);
    }
    let mut out = header(channel_id, member_account_key, count);
    let sealed = envelope::seal_labeled(&SEAL_LABELS, member_x25519_pub, &out, &plain);
    plain.fill(0);
    let sealed = sealed.map_err(|_| ChannelEpochSeedsError::Seal)?;
    if sealed.len() != SEAL_OVERHEAD + seeds.len() * CHANNEL_EPOCH_SEED_ENTRY_LEN {
        return Err(ChannelEpochSeedsError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn array_at<const N: usize>(
    payload: &[u8],
    at: usize,
) -> CoreResult<[u8; N], ChannelEpochSeedsError> {
    payload
        .get(at..at + N)
        .and_then(|s| s.try_into().ok())
        .ok_or(ChannelEpochSeedsError::BadLength)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`ChannelEpochSeedsError::UnknownVersion`] (checked before any length),
/// [`ChannelEpochSeedsError::BadLength`] and
/// [`ChannelEpochSeedsError::BadCount`].
pub fn try_unwrap_channel_epoch_seeds(
    plaintext: &[u8],
) -> CoreResult<Option<ChannelEpochSeedsEnvelope>, ChannelEpochSeedsError> {
    let Some(payload) = plaintext.strip_prefix(CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload
        .split_first()
        .ok_or(ChannelEpochSeedsError::BadLength)?;
    if version != CHANNEL_EPOCH_SEEDS_VERSION_V1 {
        return Err(ChannelEpochSeedsError::UnknownVersion(version));
    }
    let &count = payload
        .get(COUNT_AT)
        .ok_or(ChannelEpochSeedsError::BadLength)?;
    count_in_range(usize::from(count))?;
    let sealed_len = SEAL_OVERHEAD + usize::from(count) * CHANNEL_EPOCH_SEED_ENTRY_LEN;
    if payload.len() != PAYLOAD_HEADER_LEN + sealed_len {
        return Err(ChannelEpochSeedsError::BadLength);
    }
    Ok(Some(ChannelEpochSeedsEnvelope {
        channel_id: array_at(payload, CHANNEL_AT)?,
        member_account_key: array_at(payload, MEMBER_AT)?,
        count,
        sealed: payload[SEALED_AT..].to_vec(),
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ChannelEpochSeedsError::WrongDomain`] and every error of
/// [`try_unwrap_channel_epoch_seeds`].
pub fn decode_channel_epoch_seeds(
    bytes: &[u8],
) -> CoreResult<ChannelEpochSeedsEnvelope, ChannelEpochSeedsError> {
    try_unwrap_channel_epoch_seeds(bytes)?.ok_or(ChannelEpochSeedsError::WrongDomain)
}

impl ChannelEpochSeedsEnvelope {
    /// Whether this envelope is addressed to `account_key`.
    #[must_use]
    pub fn is_addressed_to(&self, account_key: &[u8; 32]) -> bool {
        self.member_account_key == *account_key
    }

    /// The exact wire bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = header(&self.channel_id, &self.member_account_key, self.count);
        out.extend_from_slice(&self.sealed);
        out
    }

    /// Open the seeds with the addressee account's X25519 secret, in
    /// ascending epoch order. The header is the AAD, so another channel,
    /// addressee or count fails to open.
    ///
    /// # Errors
    ///
    /// [`ChannelEpochSeedsError::Open`] when the box does not open,
    /// [`ChannelEpochSeedsError::Malformed`] when the opened list is not
    /// `count` entries strictly ascending by epoch.
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<Vec<ChannelEpochSeed>, ChannelEpochSeedsError> {
        let aad = header(&self.channel_id, &self.member_account_key, self.count);
        let mut plain =
            envelope::open_labeled(&SEAL_LABELS, account_x25519_secret, &aad, &self.sealed)
                .map_err(|_| ChannelEpochSeedsError::Open)?;
        let parsed = parse_plain(&plain, usize::from(self.count));
        plain.fill(0);
        parsed
    }
}

fn parse_plain(
    plain: &[u8],
    count: usize,
) -> CoreResult<Vec<ChannelEpochSeed>, ChannelEpochSeedsError> {
    if count == 0
        || count > CHANNEL_EPOCH_SEEDS_MAX
        || plain.len() != count * CHANNEL_EPOCH_SEED_ENTRY_LEN
    {
        return Err(ChannelEpochSeedsError::Malformed);
    }
    let mut out: Vec<ChannelEpochSeed> = Vec::with_capacity(count);
    for entry in plain.chunks_exact(CHANNEL_EPOCH_SEED_ENTRY_LEN) {
        let (epoch_bytes, seed_bytes) = entry.split_at(CHANNEL_EPOCH_ID_LEN);
        let epoch_arr: [u8; CHANNEL_EPOCH_ID_LEN] = epoch_bytes
            .try_into()
            .map_err(|_| ChannelEpochSeedsError::Malformed)?;
        let seed: [u8; CHANNEL_EPOCH_SEED_LEN] = seed_bytes
            .try_into()
            .map_err(|_| ChannelEpochSeedsError::Malformed)?;
        let epoch = u32::from_be_bytes(epoch_arr);
        if out.last().is_some_and(|prev| prev.epoch >= epoch) {
            return Err(ChannelEpochSeedsError::Malformed);
        }
        out.push(ChannelEpochSeed { epoch, seed });
    }
    Ok(out)
}

/// The fuzz oracle behind `fuzz_api::channel_epoch_seeds_decode`: `None`
/// when the decoder refuses; `Some(true)` when what decoded re-encodes to
/// the same bytes, has the length its count names, and opening it under a
/// fixed key never panicked. The opened-list parser is fed the same bytes
/// directly, since a fuzzer cannot forge the tag.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let plain_ok = bytes.split_first().map_or(true, |(&count, rest)| {
        parse_plain(rest, usize::from(count)).map_or(true, |seeds| {
            seeds.windows(2).all(|w| w[0].epoch < w[1].epoch)
        })
    });
    if !plain_ok {
        return Some(false);
    }
    let env = decode_channel_epoch_seeds(bytes).ok()?;
    let _ = env.open(&X25519SecretKey::from_bytes([0x33; 32]));
    let len_ok = bytes.len() == channel_epoch_seeds_envelope_len(usize::from(env.count))
        && (CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN..=CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN)
            .contains(&bytes.len());
    Some(env.to_bytes() == bytes && len_ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CHANNEL: [u8; CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN] =
        [0x0C; CHANNEL_EPOCH_SEEDS_CHANNEL_ID_LEN];
    const MEMBER: [u8; 32] = [0x4B; 32];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn seed(epoch: u32, byte: u8) -> ChannelEpochSeed {
        ChannelEpochSeed {
            epoch,
            seed: [byte; CHANNEL_EPOCH_SEED_LEN],
        }
    }

    fn wire(seeds: &[ChannelEpochSeed]) -> Vec<u8> {
        let (_, public) = member_keys();
        compose_channel_epoch_seeds(&CHANNEL, &MEMBER, &public, seeds).unwrap()
    }

    #[test]
    fn lengths_are_as_documented() {
        assert_eq!(CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.len(), 29);
        assert_eq!(CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN, 29 + 50 + 48 + 36);
        assert_eq!(CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN, 2431);
    }

    #[test]
    fn compose_decode_open_round_trips_sorted() {
        let (secret, _) = member_keys();
        let bytes = wire(&[seed(7, 0x07), seed(0, 0x00), seed(3, 0x03)]);
        assert_eq!(bytes.len(), channel_epoch_seeds_envelope_len(3));
        let env = decode_channel_epoch_seeds(&bytes).unwrap();
        assert_eq!(env.channel_id, CHANNEL);
        assert_eq!(env.count, 3);
        assert!(env.is_addressed_to(&MEMBER));
        assert!(!env.is_addressed_to(&[0; 32]));
        let opened = env.open(&secret).unwrap();
        assert_eq!(
            opened.iter().map(|s| s.epoch).collect::<Vec<_>>(),
            vec![0, 3, 7]
        );
        assert_eq!(opened[1].seed, [0x03; 32]);
        assert_eq!(env.to_bytes(), bytes);
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn compose_refuses_an_empty_list_too_many_and_duplicates() {
        let (_, public) = member_keys();
        assert_eq!(
            compose_channel_epoch_seeds(&CHANNEL, &MEMBER, &public, &[]),
            Err(ChannelEpochSeedsError::BadCount)
        );
        let many: Vec<_> = (0..=64u32).map(|e| seed(e, 1)).collect();
        assert_eq!(
            compose_channel_epoch_seeds(&CHANNEL, &MEMBER, &public, &many),
            Err(ChannelEpochSeedsError::BadCount)
        );
        assert!(compose_channel_epoch_seeds(&CHANNEL, &MEMBER, &public, &many[..64]).is_ok());
        assert_eq!(
            compose_channel_epoch_seeds(&CHANNEL, &MEMBER, &public, &[seed(2, 1), seed(2, 2)]),
            Err(ChannelEpochSeedsError::DuplicateEpoch)
        );
    }

    #[test]
    fn the_seeds_are_not_in_the_clear_and_only_the_addressee_opens_them() {
        let bytes = wire(&[seed(1, 0xA7)]);
        assert!(!bytes.windows(32).any(|w| w == [0xA7; 32]));
        let env = decode_channel_epoch_seeds(&bytes).unwrap();
        assert_eq!(
            env.open(&X25519SecretKey::from_bytes([0x44; 32])),
            Err(ChannelEpochSeedsError::Open)
        );
    }

    #[test]
    fn the_clear_header_is_bound_to_the_seal() {
        let (secret, _) = member_keys();
        let good = wire(&[seed(1, 1), seed(2, 2)]);
        let domain = CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.len();
        for at in [domain + CHANNEL_AT, domain + MEMBER_AT] {
            let mut tampered = good.clone();
            tampered[at] ^= 0x01;
            let env = decode_channel_epoch_seeds(&tampered).unwrap();
            assert_eq!(
                env.open(&secret),
                Err(ChannelEpochSeedsError::Open),
                "byte {at}"
            );
        }
        // A changed count changes the length the decoder demands.
        let mut recount = good;
        recount[domain + COUNT_AT] = 1;
        assert_eq!(
            try_unwrap_channel_epoch_seeds(&recount),
            Err(ChannelEpochSeedsError::BadLength)
        );
    }

    #[test]
    fn version_count_and_length_are_checked() {
        let good = wire(&[seed(5, 5)]);
        let domain = CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            try_unwrap_channel_epoch_seeds(&good[..good.len() - 1]),
            Err(ChannelEpochSeedsError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_channel_epoch_seeds(&long),
            Err(ChannelEpochSeedsError::BadLength)
        );
        let mut v2 = good.clone();
        v2[domain] = 0x02;
        assert_eq!(
            try_unwrap_channel_epoch_seeds(&v2),
            Err(ChannelEpochSeedsError::UnknownVersion(0x02))
        );
        for bad in [0u8, 65, 255] {
            let mut c = good.clone();
            c[domain + COUNT_AT] = bad;
            assert_eq!(
                try_unwrap_channel_epoch_seeds(&c),
                Err(ChannelEpochSeedsError::BadCount),
                "count {bad}"
            );
        }
        assert_eq!(
            try_unwrap_channel_epoch_seeds(CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1),
            Err(ChannelEpochSeedsError::BadLength)
        );
        assert_eq!(
            try_unwrap_channel_epoch_seeds(&good[..domain + COUNT_AT]),
            Err(ChannelEpochSeedsError::BadLength)
        );
        assert_eq!(try_unwrap_channel_epoch_seeds(b"hello"), Ok(None));
        assert_eq!(
            decode_channel_epoch_seeds(b"hello"),
            Err(ChannelEpochSeedsError::WrongDomain)
        );
    }

    #[test]
    fn an_opened_list_out_of_order_or_short_is_refused() {
        let mut plain = Vec::new();
        for e in [4u32, 2] {
            plain.extend_from_slice(&e.to_be_bytes());
            plain.extend_from_slice(&[9; 32]);
        }
        assert_eq!(
            parse_plain(&plain, 2),
            Err(ChannelEpochSeedsError::Malformed)
        );
        assert_eq!(
            parse_plain(&plain[..36], 2),
            Err(ChannelEpochSeedsError::Malformed)
        );
        assert_eq!(parse_plain(&[], 0), Err(ChannelEpochSeedsError::Malformed));
        assert!(parse_plain(&plain[..36], 1).is_ok());
    }

    #[test]
    fn debug_never_prints_a_seed() {
        let printed = format!("{:?}", seed(9, 0xAB));
        assert!(printed.contains("redacted"));
        assert!(!printed.contains("171"));
    }

    #[test]
    fn separator_distinct_from_sibling_domains_and_dispatched() {
        for d in [
            crate::channel_read_key::CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(
                !d.starts_with(CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1)
                    && !CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire(&[seed(1, 1)])),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelEpochSeeds(_))
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn any_distinct_set_round_trips(
            epochs in proptest::collection::btree_set(any::<u32>(), 1..=64usize),
            fill in any::<u8>(),
        ) {
            let (secret, _) = member_keys();
            let seeds: Vec<_> = epochs
                .iter()
                .rev()
                .map(|&e| seed(e, fill ^ e.to_be_bytes()[3]))
                .collect();
            let bytes = wire(&seeds);
            prop_assert_eq!(bytes.len(), channel_epoch_seeds_envelope_len(seeds.len()));
            let opened = decode_channel_epoch_seeds(&bytes).unwrap().open(&secret).unwrap();
            let want: Vec<u32> = epochs.iter().copied().collect();
            prop_assert_eq!(opened.iter().map(|s| s.epoch).collect::<Vec<_>>(), want);
            for s in &opened {
                prop_assert_eq!(s.seed, [fill ^ s.epoch.to_be_bytes()[3]; 32]);
            }
            prop_assert_eq!(fuzz_decode(&bytes), Some(true));
        }

        #[test]
        fn arbitrary_bytes_behind_the_domain_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..3000)) {
            let mut bytes = CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }
    }
}
