// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 section 4.3 channel polls on the wasm surface (UniFFI mirror:
//! `lakko-ffi-uniffi/src/channel_poll.rs`). Label lists cross
//! flat-concatenated, 16 bytes each.
//!
//! Errors throw `"channelPoll.<code>: <reason>"` with the codes
//! `optionOutOfRange`, `badLabelCount`, `labelsNotCanonical`,
//! `malformedTally`, `tallyCountAboveVoters`, `badLabelLength`, `badAction`.

use lakko_channels::poll as core;
use lakko_channels::reaction::{ReactionLabel, REACTION_LABEL_LEN};
use wasm_bindgen::prelude::*;

use crate::channel_feedback::ChannelReactionKey;

const fn code(e: core::ChannelPollError) -> &'static str {
    match e {
        core::ChannelPollError::OptionOutOfRange => "optionOutOfRange",
        core::ChannelPollError::BadLabelCount => "badLabelCount",
        core::ChannelPollError::LabelsNotCanonical => "labelsNotCanonical",
        core::ChannelPollError::MalformedTally => "malformedTally",
        core::ChannelPollError::TallyCountAboveVoters => "tallyCountAboveVoters",
    }
}

fn poll_err(e: core::ChannelPollError) -> String {
    format!("channelPoll.{}: {e}", code(e))
}

fn labels_of(flat: &[u8]) -> Result<Vec<ReactionLabel>, String> {
    if flat.len() % REACTION_LABEL_LEN != 0 {
        return Err(format!(
            "channelPoll.badLabelLength: labels must be a multiple of {REACTION_LABEL_LEN} bytes, got {}",
            flat.len()
        ));
    }
    Ok(flat
        .chunks_exact(REACTION_LABEL_LEN)
        .filter_map(|l| ReactionLabel::try_from(l).ok())
        .collect())
}

const _: () =
    assert!(core::POLL_TALLY_MAX_LEN == lakko_attachments::poll::MAX_POLL_CLOSE_TALLY_BYTES);

/// A channel poll's frozen tally.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPollTally {
    voters: u64,
    counts: Vec<u64>,
}

#[wasm_bindgen]
impl ChannelPollTally {
    /// Build one (`voters` a `bigint`, `counts` a `BigUint64Array`).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(voters: u64, counts: Vec<u64>) -> ChannelPollTally {
        Self { voters, counts }
    }

    /// Distinct voters the publisher read.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn voters(&self) -> u64 {
        self.voters
    }

    /// Votes per option, in option order.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn counts(&self) -> Vec<u64> {
        self.counts.clone()
    }
}

impl From<core::ChannelPollTally> for ChannelPollTally {
    fn from(t: core::ChannelPollTally) -> Self {
        Self {
            voters: t.voters,
            counts: t.counts,
        }
    }
}

/// The decoded tally of a close op's tally bytes, or `None`.
pub(crate) fn decode_close_tally(bytes: &[u8]) -> Option<ChannelPollTally> {
    core::decode_poll_tally(bytes).ok().map(Into::into)
}

/// The labels of options `0..optionCount` of the poll in the post at
/// `postCursor` (`bigint`), flat-concatenated in option order.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange`.
#[wasm_bindgen(js_name = channelPollOptionLabels)]
pub fn channel_poll_option_labels(
    key: &ChannelReactionKey,
    post_cursor: u64,
    option_count: u8,
) -> Result<Vec<u8>, String> {
    Ok(
        core::poll_option_labels(&key.inner, post_cursor, usize::from(option_count))
            .map_err(poll_err)?
            .concat(),
    )
}

/// The message a vote's presentation is bound to. `action` is `"put"` or
/// `"delete"`; `labels` the chosen options' labels, flat (sorted here).
///
/// # Errors
///
/// `channelPoll.badAction`, `channelPoll.badLabelCount`,
/// `channelPoll.labelsNotCanonical`, `channelPoll.badLabelLength`.
#[wasm_bindgen(js_name = channelPollVoteMessage)]
pub fn channel_poll_vote_message(
    action: &str,
    at_ms: u64,
    labels: &[u8],
) -> Result<Vec<u8>, String> {
    let action = match action {
        "put" => core::VoteAction::Put,
        "delete" => core::VoteAction::Delete,
        _ => return Err("channelPoll.badAction: action must be \"put\" or \"delete\"".into()),
    };
    let mut labels = labels_of(labels)?;
    labels.sort_unstable();
    core::vote_message(action, at_ms, &labels).map_err(poll_err)
}

/// A tally from the service's counts: `optionLabels` flat in option order,
/// `labels` flat with `counts` index for index, and `voters`.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange`, `channelPoll.badLabelLength`.
#[wasm_bindgen(js_name = channelPollTallyFromCounts)]
pub fn channel_poll_tally_from_counts(
    option_labels: &[u8],
    labels: &[u8],
    counts: Vec<u64>,
    voters: u64,
) -> Result<ChannelPollTally, String> {
    let options = labels_of(option_labels)?;
    let pairs: Vec<(ReactionLabel, u64)> = labels_of(labels)?.into_iter().zip(counts).collect();
    core::ChannelPollTally::from_counts(&options, &pairs, voters)
        .map(Into::into)
        .map_err(poll_err)
}

/// Encode a tally for `composePollCloseEnvelope`.
///
/// # Errors
///
/// `channelPoll.optionOutOfRange`, `channelPoll.tallyCountAboveVoters`.
#[wasm_bindgen(js_name = encodeChannelPollTally)]
pub fn encode_channel_poll_tally(tally: &ChannelPollTally) -> Result<Vec<u8>, String> {
    core::ChannelPollTally {
        voters: tally.voters,
        counts: tally.counts.clone(),
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
#[wasm_bindgen(js_name = decodeChannelPollTally)]
pub fn decode_channel_poll_tally(bytes: &[u8]) -> Result<ChannelPollTally, String> {
    core::decode_poll_tally(bytes)
        .map(Into::into)
        .map_err(poll_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tally_round_trips_and_the_close_carries_it() {
        let key = ChannelReactionKey::derive(&[2; 32], &[3; 16], 1).unwrap();
        let labels = channel_poll_option_labels(&key, 7, 2).unwrap();
        let tally = channel_poll_tally_from_counts(&labels, &labels[16..32], vec![3], 4).unwrap();
        assert_eq!(tally.counts(), vec![0, 3]);
        let bytes = encode_channel_poll_tally(&tally).unwrap();
        assert_eq!(decode_channel_poll_tally(&bytes).unwrap(), tally);
        let close = lakko_attachments::compose_poll_close(9, Some(&bytes)).unwrap();
        let parsed = crate::attachments::parse_body_envelope(&close);
        assert_eq!(parsed.kind(), "poll-close");
        assert_eq!(parsed.poll_tally(), Some(tally));
        assert!(channel_poll_vote_message("put", 1, &labels).is_ok());
        assert!(channel_poll_vote_message("delete", 1, &labels).is_err());
    }
}
