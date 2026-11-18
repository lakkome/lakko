// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Polls in channels: option labels, the message a vote's credential
//! presentation is bound to, and the frozen tally ([ADR-0092] section 4).
//!
//! A channel subscriber is not an MLS member, so a vote is a row at the
//! channel service, keyed by the tag of an anonymous credential
//! presentation (`lakko-anon-cred`). The service holds opaque labels only:
//!
//! ```text
//! label_i = BLAKE3-keyed(reaction_key, post_cursor(8, BE) ‖ "lakko/poll-option/v1\0" ‖ option_index(1))[..16]
//! ```
//!
//! `reaction_key` is the post's [`ReactionKey`] (ADR-0087 section 13a), so
//! labels differ under every post and the service cannot tell which label is
//! which option.
//!
//! ## The vote message
//!
//! A presentation is Fiat-Shamir-bound to the poll (its scope: channel and
//! post) and to this message, so a captured presentation cannot be replayed
//! as a retract, nor to restore a vote its voter has since changed:
//!
//! ```text
//! "lakko/channel-poll/vote/v1\0" ‖ action(1: 0x01 put, 0x02 delete) ‖ at_ms(8, BE)
//!   ‖ label_count(1) ‖ labels(16 each, strictly ascending)
//! ```
//!
//! A put carries 1..=10 labels, a delete none. `at_ms` is the client's
//! clock; the service accepts it within a window of its own clock and only
//! when it is strictly greater than the `at_ms` it stored for the tag (the
//! row is kept as a tombstone after a delete), so every accepted request is
//! newer than the last one.
//!
//! ## The frozen tally
//!
//! At close the publisher edits the post with the counts it read
//! ([`ChannelPollTally`]); readers show that and stop offering a vote. The
//! tally is the publisher's reading (ADR-0092 4.6); decoding only checks it
//! is well formed.
//!
//! ```text
//! "lakko/channel-poll/tally/v1\0" ‖ 0x01 ‖ voters(8, BE) ‖ option_count(1: 2..=10)
//!   ‖ counts(8, BE each)
//! ```
//!
//! Each count is at most `voters` (a voter marks an option once).
//!
//! [ADR-0092]: ../../../docs/adr/0092-attachment-kinds.md

use crate::reaction::{ReactionKey, ReactionLabel, REACTION_LABEL_LEN};

/// Domain folded into an option label, after the post cursor. Frozen.
pub const POLL_OPTION_LABEL_DOMAIN: &[u8] = b"lakko/poll-option/v1\0";

/// Domain opening a vote message. Frozen.
pub const VOTE_MESSAGE_DOMAIN: &[u8] = b"lakko/channel-poll/vote/v1\0";

/// Domain opening a frozen tally. Frozen.
pub const POLL_TALLY_DOMAIN: &[u8] = b"lakko/channel-poll/tally/v1\0";

/// Version byte of the frozen tally.
pub const POLL_TALLY_VERSION_V1: u8 = 0x01;

/// The fewest options a poll has (ADR-0092 section 3.1).
pub const MIN_CHANNEL_POLL_OPTIONS: usize = 2;

/// The most options a poll has (ADR-0092 section 3.1).
pub const MAX_CHANNEL_POLL_OPTIONS: usize = 10;

/// The most labels one vote carries (ADR-0092 section 4.3).
pub const MAX_VOTE_LABELS: usize = MAX_CHANNEL_POLL_OPTIONS;

/// The longest vote message.
pub const MAX_VOTE_MESSAGE_LEN: usize =
    VOTE_MESSAGE_DOMAIN.len() + 1 + 8 + 1 + MAX_VOTE_LABELS * REACTION_LABEL_LEN;

/// The shortest frozen tally.
pub const POLL_TALLY_MIN_LEN: usize =
    POLL_TALLY_DOMAIN.len() + 1 + 8 + 1 + MIN_CHANNEL_POLL_OPTIONS * 8;

/// The longest frozen tally.
pub const POLL_TALLY_MAX_LEN: usize =
    POLL_TALLY_DOMAIN.len() + 1 + 8 + 1 + MAX_CHANNEL_POLL_OPTIONS * 8;

/// Why a channel-poll value was refused. Carries no input bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChannelPollError {
    /// An option index at or above [`MAX_CHANNEL_POLL_OPTIONS`], or an
    /// option count outside `2..=10`.
    #[error("option index or count out of range")]
    OptionOutOfRange,
    /// A put with no label or more than [`MAX_VOTE_LABELS`], or a delete
    /// with any.
    #[error("vote carries the wrong number of labels")]
    BadLabelCount,
    /// Labels not strictly ascending (unsorted or repeated).
    #[error("vote labels are not strictly ascending")]
    LabelsNotCanonical,
    /// A frozen tally with the wrong domain, version or length.
    #[error("malformed poll tally")]
    MalformedTally,
    /// A frozen tally whose count exceeds its voters.
    #[error("poll tally count exceeds its voters")]
    TallyCountAboveVoters,
}

/// What a vote request does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteAction {
    /// Store or replace the vote (`PUT .../votes/{tag}`).
    Put,
    /// Retract it (`DELETE .../votes/{tag}`).
    Delete,
}

impl VoteAction {
    /// The frozen wire byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Put => 0x01,
            Self::Delete => 0x02,
        }
    }
}

/// The label of option `option_index` of the poll in the post at
/// `post_cursor`, under that post's reaction key.
///
/// # Errors
///
/// [`ChannelPollError::OptionOutOfRange`] for an index at or above
/// [`MAX_CHANNEL_POLL_OPTIONS`].
pub fn poll_option_label(
    key: &ReactionKey,
    post_cursor: u64,
    option_index: u8,
) -> Result<ReactionLabel, ChannelPollError> {
    if usize::from(option_index) >= MAX_CHANNEL_POLL_OPTIONS {
        return Err(ChannelPollError::OptionOutOfRange);
    }
    let mut input = Vec::with_capacity(8 + POLL_OPTION_LABEL_DOMAIN.len() + 1);
    input.extend_from_slice(&post_cursor.to_be_bytes());
    input.extend_from_slice(POLL_OPTION_LABEL_DOMAIN);
    input.push(option_index);
    Ok(key.keyed_label(&input))
}

/// The labels of options `0..option_count`, in option order.
///
/// # Errors
///
/// [`ChannelPollError::OptionOutOfRange`] for a count outside `2..=10`.
pub fn poll_option_labels(
    key: &ReactionKey,
    post_cursor: u64,
    option_count: usize,
) -> Result<Vec<ReactionLabel>, ChannelPollError> {
    if !(MIN_CHANNEL_POLL_OPTIONS..=MAX_CHANNEL_POLL_OPTIONS).contains(&option_count) {
        return Err(ChannelPollError::OptionOutOfRange);
    }
    (0..option_count)
        .map(|i| {
            let i = u8::try_from(i).map_err(|_| ChannelPollError::OptionOutOfRange)?;
            poll_option_label(key, post_cursor, i)
        })
        .collect()
}

/// Check the labels of a vote: a put carries 1..=[`MAX_VOTE_LABELS`]
/// strictly ascending labels, a delete none.
///
/// # Errors
///
/// [`ChannelPollError::BadLabelCount`], [`ChannelPollError::LabelsNotCanonical`].
pub fn check_vote_labels(
    action: VoteAction,
    labels: &[ReactionLabel],
) -> Result<(), ChannelPollError> {
    match action {
        VoteAction::Put if labels.is_empty() || labels.len() > MAX_VOTE_LABELS => {
            return Err(ChannelPollError::BadLabelCount)
        },
        VoteAction::Delete if !labels.is_empty() => return Err(ChannelPollError::BadLabelCount),
        _ => {},
    }
    if labels.windows(2).any(|w| w[0] >= w[1]) {
        return Err(ChannelPollError::LabelsNotCanonical);
    }
    Ok(())
}

/// The message a vote's presentation is bound to (layout in the module
/// docs). The labels must already be canonical; sort them first.
///
/// # Errors
///
/// What [`check_vote_labels`] refuses.
pub fn vote_message(
    action: VoteAction,
    at_ms: u64,
    labels: &[ReactionLabel],
) -> Result<Vec<u8>, ChannelPollError> {
    check_vote_labels(action, labels)?;
    let count = u8::try_from(labels.len()).map_err(|_| ChannelPollError::BadLabelCount)?;
    let mut out = Vec::with_capacity(MAX_VOTE_MESSAGE_LEN);
    out.extend_from_slice(VOTE_MESSAGE_DOMAIN);
    out.push(action.byte());
    out.extend_from_slice(&at_ms.to_be_bytes());
    out.push(count);
    for label in labels {
        out.extend_from_slice(label);
    }
    Ok(out)
}

/// The frozen tally of a channel poll at close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPollTally {
    /// Distinct voters the publisher read.
    pub voters: u64,
    /// Votes per option, in option order (2..=10 entries).
    pub counts: Vec<u64>,
}

impl ChannelPollTally {
    /// Build a tally from the service's counts: `option_labels` are the
    /// poll's labels in option order ([`poll_option_labels`]), `label_counts`
    /// the service's `(label, count)` pairs, `voters` its voter count. A
    /// label no option has is dropped; a count above `voters` is clamped to
    /// it.
    ///
    /// # Errors
    ///
    /// [`ChannelPollError::OptionOutOfRange`] for a label list outside
    /// `2..=10`.
    pub fn from_counts(
        option_labels: &[ReactionLabel],
        label_counts: &[(ReactionLabel, u64)],
        voters: u64,
    ) -> Result<Self, ChannelPollError> {
        if !(MIN_CHANNEL_POLL_OPTIONS..=MAX_CHANNEL_POLL_OPTIONS).contains(&option_labels.len()) {
            return Err(ChannelPollError::OptionOutOfRange);
        }
        let counts = option_labels
            .iter()
            .map(|label| {
                label_counts
                    .iter()
                    .filter(|(l, _)| l == label)
                    .fold(0u64, |acc, (_, n)| acc.saturating_add(*n))
                    .min(voters)
            })
            .collect();
        Ok(Self { voters, counts })
    }

    /// Encode (layout in the module docs).
    ///
    /// # Errors
    ///
    /// What [`decode_poll_tally`] would refuse.
    pub fn encode(&self) -> Result<Vec<u8>, ChannelPollError> {
        self.check()?;
        let n = u8::try_from(self.counts.len()).map_err(|_| ChannelPollError::OptionOutOfRange)?;
        let mut out = Vec::with_capacity(POLL_TALLY_MAX_LEN);
        out.extend_from_slice(POLL_TALLY_DOMAIN);
        out.push(POLL_TALLY_VERSION_V1);
        out.extend_from_slice(&self.voters.to_be_bytes());
        out.push(n);
        for c in &self.counts {
            out.extend_from_slice(&c.to_be_bytes());
        }
        Ok(out)
    }

    fn check(&self) -> Result<(), ChannelPollError> {
        if !(MIN_CHANNEL_POLL_OPTIONS..=MAX_CHANNEL_POLL_OPTIONS).contains(&self.counts.len()) {
            return Err(ChannelPollError::OptionOutOfRange);
        }
        if self.counts.iter().any(|c| *c > self.voters) {
            return Err(ChannelPollError::TallyCountAboveVoters);
        }
        Ok(())
    }
}

/// Decode a frozen tally; strict and canonical (exact length).
///
/// # Errors
///
/// [`ChannelPollError::MalformedTally`], [`ChannelPollError::OptionOutOfRange`],
/// [`ChannelPollError::TallyCountAboveVoters`].
pub fn decode_poll_tally(bytes: &[u8]) -> Result<ChannelPollTally, ChannelPollError> {
    let rest = bytes
        .strip_prefix(POLL_TALLY_DOMAIN)
        .ok_or(ChannelPollError::MalformedTally)?;
    let (&version, rest) = rest.split_first().ok_or(ChannelPollError::MalformedTally)?;
    if version != POLL_TALLY_VERSION_V1 {
        return Err(ChannelPollError::MalformedTally);
    }
    if rest.len() < 9 {
        return Err(ChannelPollError::MalformedTally);
    }
    let (voters_bytes, rest) = rest.split_at(8);
    let mut voters_arr = [0u8; 8];
    voters_arr.copy_from_slice(voters_bytes);
    let voters = u64::from_be_bytes(voters_arr);
    let (&count, rest) = rest.split_first().ok_or(ChannelPollError::MalformedTally)?;
    let count = usize::from(count);
    if !(MIN_CHANNEL_POLL_OPTIONS..=MAX_CHANNEL_POLL_OPTIONS).contains(&count) {
        return Err(ChannelPollError::OptionOutOfRange);
    }
    if rest.len() != count * 8 {
        return Err(ChannelPollError::MalformedTally);
    }
    let counts = rest
        .chunks_exact(8)
        .map(|c| {
            let mut a = [0u8; 8];
            a.copy_from_slice(c);
            u64::from_be_bytes(a)
        })
        .collect();
    let tally = ChannelPollTally { voters, counts };
    tally.check()?;
    Ok(tally)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epoch::EpochId;
    use crate::seed::ContentKeySeed;

    const CHANNEL: [u8; 16] = [0x0C; 16];

    fn key(seed: u8) -> ReactionKey {
        ReactionKey::derive(
            &ContentKeySeed::from_bytes_for_test([seed; 32]),
            &CHANNEL,
            EpochId::new(3),
        )
        .unwrap()
    }

    #[test]
    fn option_labels_are_distinct_and_depend_on_post_and_key() {
        let k = key(1);
        let labels = poll_option_labels(&k, 7, 10).unwrap();
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_ne!(labels, poll_option_labels(&k, 8, 10).unwrap());
        assert_ne!(labels, poll_option_labels(&key(2), 7, 10).unwrap());
        // Never the label of an emoji under the same post.
        assert!(!labels.contains(&k.label(7, "\u{1F44D}").unwrap()));
    }

    #[test]
    fn option_label_matches_the_adr_formula() {
        let k = key(1);
        let mut input = 7u64.to_be_bytes().to_vec();
        input.extend_from_slice(b"lakko/poll-option/v1\0");
        input.push(3);
        assert_eq!(poll_option_label(&k, 7, 3).unwrap(), k.keyed_label(&input));
    }

    #[test]
    fn option_ranges_are_enforced() {
        let k = key(1);
        assert_eq!(
            poll_option_label(&k, 0, 10),
            Err(ChannelPollError::OptionOutOfRange)
        );
        assert!(poll_option_labels(&k, 0, 1).is_err());
        assert!(poll_option_labels(&k, 0, 11).is_err());
    }

    #[test]
    fn vote_message_is_pinned_and_canonical() {
        let a = [1u8; 16];
        let b = [2u8; 16];
        let msg = vote_message(VoteAction::Put, 0x0102_0304_0506_0708, &[a, b]).unwrap();
        let mut expected = b"lakko/channel-poll/vote/v1\0".to_vec();
        expected.push(1);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        expected.push(2);
        expected.extend_from_slice(&a);
        expected.extend_from_slice(&b);
        assert_eq!(msg, expected);

        let del = vote_message(VoteAction::Delete, 5, &[]).unwrap();
        assert_eq!(del.len(), VOTE_MESSAGE_DOMAIN.len() + 10);
        assert_eq!(del[VOTE_MESSAGE_DOMAIN.len()], 2);

        assert_eq!(
            vote_message(VoteAction::Put, 5, &[b, a]),
            Err(ChannelPollError::LabelsNotCanonical)
        );
        assert_eq!(
            vote_message(VoteAction::Put, 5, &[a, a]),
            Err(ChannelPollError::LabelsNotCanonical)
        );
        assert_eq!(
            vote_message(VoteAction::Put, 5, &[]),
            Err(ChannelPollError::BadLabelCount)
        );
        assert_eq!(
            vote_message(VoteAction::Delete, 5, &[a]),
            Err(ChannelPollError::BadLabelCount)
        );
        let eleven: Vec<[u8; 16]> = (0u8..11).map(|i| [i; 16]).collect();
        assert_eq!(
            vote_message(VoteAction::Put, 5, &eleven),
            Err(ChannelPollError::BadLabelCount)
        );
        // Messages differ by action, time and labels.
        assert_ne!(
            vote_message(VoteAction::Put, 5, &[a]).unwrap(),
            vote_message(VoteAction::Put, 6, &[a]).unwrap()
        );
    }

    #[test]
    fn tally_from_counts_maps_labels_to_options() {
        let k = key(1);
        let labels = poll_option_labels(&k, 9, 3).unwrap();
        let counts = vec![(labels[2], 4), ([0xEE; 16], 9), (labels[0], 1)];
        let t = ChannelPollTally::from_counts(&labels, &counts, 5).unwrap();
        assert_eq!(t.counts, vec![1, 0, 4]);
        assert_eq!(t.voters, 5);
        // Clamped to voters.
        let t = ChannelPollTally::from_counts(&labels, &[(labels[1], 9)], 2).unwrap();
        assert_eq!(t.counts, vec![0, 2, 0]);
    }

    #[test]
    fn tally_round_trips_and_refuses_bad_input() {
        let t = ChannelPollTally {
            voters: 10,
            counts: vec![3, 7, 10],
        };
        let bytes = t.encode().unwrap();
        assert_eq!(bytes.len(), POLL_TALLY_DOMAIN.len() + 1 + 8 + 1 + 24);
        assert_eq!(decode_poll_tally(&bytes).unwrap(), t);
        for n in 0..bytes.len() {
            assert!(decode_poll_tally(&bytes[..n]).is_err());
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode_poll_tally(&longer).is_err());
        let mut wrong_version = bytes.clone();
        wrong_version[POLL_TALLY_DOMAIN.len()] = 2;
        assert!(decode_poll_tally(&wrong_version).is_err());
        let over = ChannelPollTally {
            voters: 1,
            counts: vec![2, 0],
        };
        assert_eq!(over.encode(), Err(ChannelPollError::TallyCountAboveVoters));
        let one = ChannelPollTally {
            voters: 1,
            counts: vec![1],
        };
        assert!(one.encode().is_err());
    }

    #[test]
    fn decode_never_panics_on_short_or_odd_input() {
        for len in 0..POLL_TALLY_MAX_LEN + 4 {
            let mut v = POLL_TALLY_DOMAIN.to_vec();
            v.push(1);
            v.resize(POLL_TALLY_DOMAIN.len() + 1 + len, 0xFF);
            let _ = decode_poll_tally(&v);
        }
    }
}
