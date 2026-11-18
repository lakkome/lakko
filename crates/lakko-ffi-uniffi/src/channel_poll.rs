// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 section 4.3 channel polls on the UniFFI surface: the option
//! labels a vote names, the vote message a credential presentation is bound
//! to, and the frozen tally a close carries. Every rule is
//! `lakko_channels::poll`'s.
//!
//! Errors: [`FfiError::Channels`] `"channelPoll.<code>: <reason>"` with the
//! codes `optionOutOfRange`, `badLabelCount`, `labelsNotCanonical`,
//! `malformedTally`, `tallyCountAboveVoters`, `badLabelLength`.

use std::sync::Arc;

use lakko_channels::poll as core;
use lakko_channels::reaction::{ReactionLabel, REACTION_LABEL_LEN};

use crate::channel_feedback::{ChannelReactionKey, FfiReactionLabelCount};
use crate::FfiError;

const fn code(e: core::ChannelPollError) -> &'static str {
    match e {
        core::ChannelPollError::OptionOutOfRange => "optionOutOfRange",
        core::ChannelPollError::BadLabelCount => "badLabelCount",
        core::ChannelPollError::LabelsNotCanonical => "labelsNotCanonical",
        core::ChannelPollError::MalformedTally => "malformedTally",
        core::ChannelPollError::TallyCountAboveVoters => "tallyCountAboveVoters",
    }
}

fn poll_err(e: core::ChannelPollError) -> FfiError {
    FfiError::Channels(format!("channelPoll.{}: {e}", code(e)))
}

fn label_of(v: &[u8]) -> Result<ReactionLabel, FfiError> {
    ReactionLabel::try_from(v).map_err(|_| {
        FfiError::Channels(format!(
            "channelPoll.badLabelLength: a label must be {REACTION_LABEL_LEN} bytes, got {}",
            v.len()
        ))
    })
}

// The close op's tally bound in lakko-attachments is a copy of this crate's.
const _: () =
    assert!(core::POLL_TALLY_MAX_LEN == lakko_attachments::poll::MAX_POLL_CLOSE_TALLY_BYTES);

/// A channel poll's frozen tally (what the close op carries).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChannelPollTally {
    /// Distinct voters the publisher read.
    pub voters: u64,
    /// Votes per option, in option order (2..=10).
    pub counts: Vec<u64>,
}

impl From<core::ChannelPollTally> for ChannelPollTally {
    fn from(t: core::ChannelPollTally) -> Self {
        Self {
            voters: t.voters,
            counts: t.counts,
        }
    }
}

/// What a vote request does.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelPollVoteAction {
    /// Store or replace the vote.
    Put,
    /// Retract it.
    Delete,
}

/// The 16-byte labels of options `0..optionCount` of the poll in the post at
/// `post_cursor`, under that post's epoch reaction key, in option order.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange` for a count outside 2..=10.
#[uniffi::export]
pub fn channel_poll_option_labels(
    key: Arc<ChannelReactionKey>,
    post_cursor: u64,
    option_count: u8,
) -> Result<Vec<Vec<u8>>, FfiError> {
    Ok(
        core::poll_option_labels(&key.inner, post_cursor, usize::from(option_count))
            .map_err(poll_err)?
            .iter()
            .map(|l| l.to_vec())
            .collect(),
    )
}

/// The message a vote's presentation is bound to. `labels` are the chosen
/// options' labels (any order; sorted here); a delete carries none.
///
/// # Errors
///
/// `channelPoll.badLabelCount`, `channelPoll.labelsNotCanonical` (a label
/// twice), `channelPoll.badLabelLength`.
#[uniffi::export]
pub fn channel_poll_vote_message(
    action: ChannelPollVoteAction,
    at_ms: u64,
    labels: Vec<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let mut labels = labels
        .iter()
        .map(|l| label_of(l))
        .collect::<Result<Vec<_>, _>>()?;
    labels.sort_unstable();
    let action = match action {
        ChannelPollVoteAction::Put => core::VoteAction::Put,
        ChannelPollVoteAction::Delete => core::VoteAction::Delete,
    };
    core::vote_message(action, at_ms, &labels).map_err(poll_err)
}

/// A tally from the service's counts: `option_labels` in option order
/// ([`channel_poll_option_labels`]), the service's label counts and voters.
/// A label no option has is dropped; a count above `voters` is clamped.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange`, `channelPoll.badLabelLength`.
#[uniffi::export]
pub fn channel_poll_tally_from_counts(
    option_labels: Vec<Vec<u8>>,
    label_counts: Vec<FfiReactionLabelCount>,
    voters: u64,
) -> Result<ChannelPollTally, FfiError> {
    let options = option_labels
        .iter()
        .map(|l| label_of(l))
        .collect::<Result<Vec<_>, _>>()?;
    let counts = label_counts
        .iter()
        .map(|c| Ok((label_of(&c.label)?, c.count)))
        .collect::<Result<Vec<_>, FfiError>>()?;
    core::ChannelPollTally::from_counts(&options, &counts, voters)
        .map(Into::into)
        .map_err(poll_err)
}

/// Encode a tally for `composePollCloseEnvelope`.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange`, `channelPoll.tallyCountAboveVoters`.
#[uniffi::export]
pub fn encode_channel_poll_tally(tally: ChannelPollTally) -> Result<Vec<u8>, FfiError> {
    core::ChannelPollTally {
        voters: tally.voters,
        counts: tally.counts,
    }
    .encode()
    .map_err(poll_err)
}

/// Decode a frozen tally (strict).
///
/// # Errors
///
/// `channelPoll.malformedTally`, `channelPoll.optionOutOfRange`,
/// `channelPoll.tallyCountAboveVoters`.
#[uniffi::export]
pub fn decode_channel_poll_tally(bytes: Vec<u8>) -> Result<ChannelPollTally, FfiError> {
    core::decode_poll_tally(&bytes)
        .map(Into::into)
        .map_err(poll_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::{parse_body_envelope, MessageBodyKind};

    #[test]
    fn a_close_carries_a_decoded_tally_and_a_bad_one_is_dropped() {
        let key = ChannelReactionKey::derive(vec![2; 32], vec![3; 16], 1).unwrap();
        let labels = channel_poll_option_labels(key, 7, 3).unwrap();
        assert_eq!(labels.len(), 3);
        let tally = channel_poll_tally_from_counts(
            labels.clone(),
            vec![FfiReactionLabelCount {
                label: labels[1].clone(),
                count: 4,
            }],
            5,
        )
        .unwrap();
        assert_eq!(tally.counts, vec![0, 4, 0]);
        let bytes = encode_channel_poll_tally(tally.clone()).unwrap();
        assert_eq!(decode_channel_poll_tally(bytes.clone()).unwrap(), tally);
        let close = crate::content::compose_poll_close_envelope(9, Some(bytes)).unwrap();
        let parsed = parse_body_envelope(close);
        assert_eq!(parsed.kind, MessageBodyKind::PollClose);
        assert_eq!(parsed.poll_tally, Some(tally));
        // Bytes within the bound that are not a tally: the close stands.
        let close = crate::content::compose_poll_close_envelope(9, Some(vec![1; 20])).unwrap();
        let parsed = parse_body_envelope(close);
        assert_eq!(parsed.target, Some(9));
        assert_eq!(parsed.poll_tally, None);
        let m = channel_poll_vote_message(
            ChannelPollVoteAction::Put,
            1,
            vec![labels[2].clone(), labels[0].clone()],
        )
        .unwrap();
        assert!(m.starts_with(core::VOTE_MESSAGE_DOMAIN));
        assert!(channel_poll_vote_message(
            ChannelPollVoteAction::Delete,
            1,
            vec![labels[0].clone()]
        )
        .is_err());
    }
}
