// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A private channel's admission seeds, carried between the owner's own
//! devices ([ADR-0090] open question 4, closed by this register).
//!
//! A private channel's publisher reseals a seed only to a subscriber row whose
//! admission ticket it can open (`lakko_channels::invite`): the ticket is
//! sealed to the channel's admission key of a given `u16` version, a random
//! X25519 seed the owner minted. Before this register the seeds lived only on
//! the device that minted them, so an owner device admitted later, or a
//! restored phone that founds the PublisherGroup again (ADR-0090 decision A),
//! could open no earlier ticket and resealed nothing to the earlier
//! subscribers. The register carries every version the owner minted to every
//! device of the owner's account and, through
//! [`super::SyncedState::to_bytes`], into the backup.
//!
//! What else such a device needs to keep checking tickets is already carried:
//! the live invite keys a ticket must name come from the `/g/` link records
//! (ADR-0079 link register, invite seeds included), and the owner's read key
//! derives from the account seed. Co-publishers still receive each version by
//! the PublisherGroup hand-over (`lakko_contacts::admission_seed`); this
//! register is the owner account's own copy.
//!
//! ## The register
//!
//! `(channel id, admission version, seed id) -> seed(32)`, where the seed id
//! is [`admission_seed_id`] (`BLAKE3-derive-key(ADMISSION_SEED_ID_CONTEXT,
//! seed)`). **Two seeds of one version are two registers and both survive**:
//! two owner devices that read the same highest version and mint it at once
//! each hand out invite links naming their own seed, so the subscribers of
//! both must keep passing their ticket check on every device. A device
//! therefore checks a ticket of version `v` against every seed held for `v`
//! ([`super::SyncedState::channel_admission_seeds_for`]), and its reseal and
//! admission code accept a row whose ticket opens under any of them.
//! **Write-once** per register: writing a seed already held writes nothing.
//! Folded by the ordinary rule ([`super::state`]); never dropped.
//!
//! ## Wire
//!
//! ```text
//! domain(37) "lakko/sync/channel-admission-seed/v1\0"
//! version(1) = 0x01
//! count(1)                 1..=16
//! count x {
//!     hlc(8, BE)           the register's ORIGINAL stamp
//!     channel_id(16)
//!     admission_version(2, BE)
//!     seed(32)
//! }
//! ```
//!
//! Entries are sorted by `(channel id, admission version, seed id)`, strictly
//! increasing (the seed id is computed, not carried); nothing may trail; every violation refuses the whole
//! envelope. Not in the growth snapshot: a device re-sends
//! [`super::SyncedState::channel_admission_seed_envelopes`] on DeviceGroup
//! growth, as it does the channel subscriptions.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account or from a backup.
//! The seed is SECRET: whoever holds it opens the channel's tickets of that
//! version (it learns which invite link a subscriber came by). It never
//! reaches `Debug`.
//!
//! [ADR-0090]: ../../../../docs/adr/0090-publisher-group-device-leaves.md

use thiserror::Error;

use super::entry::{take_n, take_u64, take_u8, CodecError, Stamped};

/// BLAKE3 derive-key context of a seed id. Frozen: it names a register.
pub const ADMISSION_SEED_ID_CONTEXT: &str = "lakko/sync/channel-admission-seed/id/v1";

/// The register key part that tells two seeds of one version apart: a
/// one-way 32-byte id of the seed, so a register key never shows the seed.
#[must_use]
pub fn admission_seed_id(seed: &[u8; CHANNEL_ADMISSION_SEED_LEN]) -> [u8; 32] {
    lakko_crypto::hash::blake3_derive_key_from_material(ADMISSION_SEED_ID_CONTEXT, seed)
}

/// Length of a channel id.
pub const CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN: usize = 16;

/// Length of an admission seed (an X25519 seed).
pub const CHANNEL_ADMISSION_SEED_LEN: usize = 32;

/// Domain separator of the admission-seed envelope, trailing NUL included.
/// Frozen.
pub const CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1: &[u8] =
    b"lakko/sync/channel-admission-seed/v1\0";

/// The version byte this build writes and accepts.
pub const CHANNEL_ADMISSION_SEED_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Most entries one envelope carries.
pub const CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_ENTRIES: usize = 16;

/// One entry on the wire: hlc, channel id, version, seed.
const ENTRY_LEN: usize = 8 + CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN + 2 + CHANNEL_ADMISSION_SEED_LEN;

/// Shortest valid envelope: the header and one entry.
pub const CHANNEL_ADMISSION_SEED_ENVELOPE_MIN_LEN: usize =
    CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len() + 2 + ENTRY_LEN;

/// Longest valid envelope.
pub const CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_LEN: usize = CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1
    .len()
    + 2
    + CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_ENTRIES * ENTRY_LEN;

/// One admission-seed register value.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelAdmissionSeedEntry {
    /// The channel.
    pub channel_id: [u8; CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN],
    /// The admission key's version.
    pub admission_version: u16,
    /// The admission seed. SECRET.
    pub seed: [u8; CHANNEL_ADMISSION_SEED_LEN],
}

impl core::fmt::Debug for ChannelAdmissionSeedEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The seed opens the channel's tickets: left out.
        f.debug_struct("ChannelAdmissionSeedEntry")
            .field("admission_version", &self.admission_version)
            .finish_non_exhaustive()
    }
}

impl ChannelAdmissionSeedEntry {
    /// Append the body to `out`: the key (channel id, version), then the seed.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_id);
        out.extend_from_slice(&self.admission_version.to_be_bytes());
        out.extend_from_slice(&self.seed);
    }

    /// Read a body written by [`Self::encode_body`].
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let mut channel_id = [0u8; CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(take_n(cur, CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN)?);
        let v = take_n(cur, 2)?;
        let admission_version = u16::from_be_bytes([v[0], v[1]]);
        let mut seed = [0u8; CHANNEL_ADMISSION_SEED_LEN];
        seed.copy_from_slice(take_n(cur, CHANNEL_ADMISSION_SEED_LEN)?);
        Ok(Self {
            channel_id,
            admission_version,
            seed,
        })
    }

    /// The register key: channel id, version, seed id.
    pub(crate) fn sort_key(&self) -> ([u8; CHANNEL_ADMISSION_SEED_CHANNEL_ID_LEN], u16, [u8; 32]) {
        (
            self.channel_id,
            self.admission_version,
            admission_seed_id(&self.seed),
        )
    }
}

/// Why an admission-seed envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelAdmissionSeedEnvelopeError {
    /// The bytes do not open with [`CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1`].
    #[error("not a channel admission-seed envelope")]
    WrongDomain,
    /// The version byte is not [`CHANNEL_ADMISSION_SEED_ENVELOPE_VERSION_V1`].
    #[error("unknown channel admission-seed envelope version")]
    UnknownVersion,
    /// The count is zero or above
    /// [`CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_ENTRIES`].
    #[error("channel admission-seed envelope count is out of bounds")]
    Count,
    /// The bytes ended early.
    #[error("channel admission-seed envelope is truncated")]
    Truncated,
    /// Entries were not strictly increasing by (channel id, version, seed id).
    #[error("channel admission-seed envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in channel admission-seed envelope")]
    Trailing,
}

impl ChannelAdmissionSeedEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Count => "count",
            Self::Truncated => "truncated",
            Self::NotCanonical => "notCanonical",
            Self::Trailing => "trailing",
        }
    }
}

/// `true` when `bytes` open with the admission-seed envelope domain: the
/// cheap routing question. Says nothing about validity.
#[must_use]
pub fn is_channel_admission_seed_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=16 stamped registers.
///
/// # Errors
///
/// [`ChannelAdmissionSeedEnvelopeError::Count`] for none or too many entries,
/// [`ChannelAdmissionSeedEnvelopeError::NotCanonical`] for two entries of one
/// (channel, version, seed).
pub fn compose_channel_admission_seed_envelope(
    entries: &[Stamped<ChannelAdmissionSeedEntry>],
) -> Result<Vec<u8>, ChannelAdmissionSeedEnvelopeError> {
    if entries.is_empty() || entries.len() > CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelAdmissionSeedEnvelopeError::Count);
    }
    let count =
        u8::try_from(entries.len()).map_err(|_| ChannelAdmissionSeedEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<ChannelAdmissionSeedEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.sort_key());
    if sorted
        .windows(2)
        .any(|w| w[0].value.sort_key() == w[1].value.sort_key())
    {
        return Err(ChannelAdmissionSeedEnvelopeError::NotCanonical);
    }
    let mut out = Vec::with_capacity(
        CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len() + 2 + sorted.len() * ENTRY_LEN,
    );
    out.extend_from_slice(CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_ADMISSION_SEED_ENVELOPE_VERSION_V1);
    out.push(count);
    for e in sorted {
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`ChannelAdmissionSeedEnvelopeError`]; the domain is checked first,
/// then the version, then the count, then each entry, then trailing bytes.
pub fn decode_channel_admission_seed_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<ChannelAdmissionSeedEntry>>, ChannelAdmissionSeedEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1)
        .ok_or(ChannelAdmissionSeedEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| ChannelAdmissionSeedEnvelopeError::Truncated)?;
    if version != CHANNEL_ADMISSION_SEED_ENVELOPE_VERSION_V1 {
        return Err(ChannelAdmissionSeedEnvelopeError::UnknownVersion);
    }
    let count =
        usize::from(take_u8(&mut cur).map_err(|_| ChannelAdmissionSeedEnvelopeError::Truncated)?);
    if count == 0 || count > CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelAdmissionSeedEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<ChannelAdmissionSeedEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| ChannelAdmissionSeedEnvelopeError::Truncated)?;
        let value = ChannelAdmissionSeedEntry::take_body(&mut cur)
            .map_err(|_| ChannelAdmissionSeedEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.sort_key() >= value.sort_key() {
                return Err(ChannelAdmissionSeedEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(ChannelAdmissionSeedEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(channel: u8, version: u16, hlc: u64) -> Stamped<ChannelAdmissionSeedEntry> {
        Stamped {
            hlc,
            value: ChannelAdmissionSeedEntry {
                channel_id: [channel; 16],
                admission_version: version,
                seed: [0x5E; 32],
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1.len(), 37);
        let wire =
            compose_channel_admission_seed_envelope(&[entry(1, 0x0203, 9)]).unwrap_or_default();
        assert_eq!(wire.len(), CHANNEL_ADMISSION_SEED_ENVELOPE_MIN_LEN);
        assert_eq!(wire.len(), 37 + 2 + 58);
        assert_eq!(wire[37], 0x01);
        assert_eq!(wire[38], 1);
        assert_eq!(&wire[39..47], &9u64.to_be_bytes());
        assert_eq!(&wire[47..63], &[1u8; 16]);
        assert_eq!(&wire[63..65], &[0x02, 0x03]);
        assert_eq!(&wire[65..97], &[0x5E; 32]);
        assert!(is_channel_admission_seed_envelope(&wire));
        assert_eq!(CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_LEN, 37 + 2 + 16 * 58);
    }

    #[test]
    fn the_seed_stays_out_of_debug() {
        let text = format!("{:?}", entry(3, 7, 1));
        assert!(!text.contains("94") && !text.contains("seed:"), "{text}");
        assert!(text.contains("admission_version: 7"));
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_and_bad_counts() {
        let wire = compose_channel_admission_seed_envelope(&[
            entry(2, 0, 1),
            entry(1, 5, 2),
            entry(1, 1, 3),
        ])
        .unwrap_or_default();
        assert_eq!(
            decode_channel_admission_seed_envelope(&wire),
            Ok(vec![entry(1, 1, 3), entry(1, 5, 2), entry(2, 0, 1)])
        );
        assert_eq!(
            compose_channel_admission_seed_envelope(&[entry(1, 1, 1), entry(1, 1, 2)]),
            Err(ChannelAdmissionSeedEnvelopeError::NotCanonical)
        );
        // Two seeds of one version are two entries.
        let mut other = entry(1, 1, 2);
        other.value.seed = [0x6F; 32];
        let two = compose_channel_admission_seed_envelope(&[entry(1, 1, 1), other.clone()])
            .unwrap_or_default();
        let decoded = decode_channel_admission_seed_envelope(&two).unwrap_or_default();
        assert_eq!(decoded.len(), 2);
        assert!(decoded.contains(&other));
        assert_eq!(
            compose_channel_admission_seed_envelope(&[]),
            Err(ChannelAdmissionSeedEnvelopeError::Count)
        );
        let many: Vec<_> = (0..17).map(|v| entry(1, v, 1)).collect();
        assert_eq!(
            compose_channel_admission_seed_envelope(&many),
            Err(ChannelAdmissionSeedEnvelopeError::Count)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire = compose_channel_admission_seed_envelope(&[entry(1, 1, 1), entry(1, 2, 2)])
            .unwrap_or_default();
        assert_eq!(
            admission_seed_id(&[0x5E; 32]),
            lakko_crypto::hash::blake3_derive_key_from_material(
                "lakko/sync/channel-admission-seed/id/v1",
                &[0x5E; 32]
            )
        );
        for cut in 0..wire.len() {
            assert!(
                decode_channel_admission_seed_envelope(&wire[..cut]).is_err(),
                "{cut}"
            );
        }
        let mut version = wire.clone();
        version[37] = 2;
        assert_eq!(
            decode_channel_admission_seed_envelope(&version),
            Err(ChannelAdmissionSeedEnvelopeError::UnknownVersion)
        );
        let mut count = wire.clone();
        count[38] = 0;
        assert_eq!(
            decode_channel_admission_seed_envelope(&count),
            Err(ChannelAdmissionSeedEnvelopeError::Count)
        );
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_channel_admission_seed_envelope(&trailing),
            Err(ChannelAdmissionSeedEnvelopeError::Trailing)
        );
        let mut swapped = wire[..39].to_vec();
        swapped.extend_from_slice(&wire[39 + ENTRY_LEN..]);
        swapped.extend_from_slice(&wire[39..39 + ENTRY_LEN]);
        assert_eq!(
            decode_channel_admission_seed_envelope(&swapped),
            Err(ChannelAdmissionSeedEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_channel_admission_seed_envelope(b"lakko/sync/link-record/v1\0\x01"),
            Err(ChannelAdmissionSeedEnvelopeError::WrongDomain)
        );
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let mut input = CHANNEL_ADMISSION_SEED_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_channel_admission_seed_envelope(&input) {
                prop_assert!(input.len() <= CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_LEN);
                prop_assert_eq!(compose_channel_admission_seed_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(entries in proptest::collection::btree_map((any::<[u8; 16]>(), any::<u16>(), any::<[u8; 32]>()), any::<u64>(), 1..=16)) {
            let mut stamped: Vec<Stamped<ChannelAdmissionSeedEntry>> = entries
                .into_iter()
                .map(|((channel_id, admission_version, seed), hlc)| Stamped {
                    hlc,
                    value: ChannelAdmissionSeedEntry { channel_id, admission_version, seed },
                })
                .collect();
            let wire = compose_channel_admission_seed_envelope(&stamped).unwrap_or_default();
            stamped.sort_by_key(|e| e.value.sort_key());
            prop_assert_eq!(decode_channel_admission_seed_envelope(&wire), Ok(stamped));
        }
    }
}
