// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's own poll choices, and their envelope ([ADR-0090] section
//! 3.6, "Own choice").
//!
//! An anonymous ballot names nobody, so the voter's other devices cannot learn
//! the voter's choice from it; a channel vote is anonymous at the service for
//! the same reason. The account's devices keep their own choices among
//! themselves instead, in this register, carried over the DeviceGroup and in
//! the backup ([`super::SyncedState::to_bytes`]).
//!
//! ## The register
//!
//! `(conversation, poll message id) -> choices`. The conversation is the same
//! [`ConversationRef`] the other registers use: a group by its MLS group id,
//! a channel by its channel id. The poll message id is the DS `messageId` of
//! the poll row in a group, the post cursor in a channel, as an `i64`. The
//! choices are option indices, strictly ascending, at most
//! [`POLL_CHOICE_MAX_OPTIONS`], each below it; an empty list is a retracted
//! vote, kept like any value so a device offline across the retraction cannot
//! bring the vote back with an older stamp. The value also carries
//! `set_version`, the ring version the account's anonymity set in this poll
//! was drawn from at its first vote (`lakko_ballot::draw_set`; 0 for a channel
//! poll, which has no ring): every later vote from any device of the account
//! draws at that version again, so it proves over the same set. One register per poll the account
//! voted in, folded by the ordinary rule ([`super::state`]): the greater
//! stamp wins, on an exact tie the larger canonical bytes.
//!
//! **Kept until the conversation ends.** [`super::SyncedState::prune_poll_choices`]
//! drops every register of a conversation whose conversation-end register
//! reads ended (ADR-0087 section 9); nothing else drops one. That is safe for
//! the same reason the channel-activity prune is: once the conversation is
//! gone from the account's devices nothing shows the choice, so a register a
//! late envelope brings back changes nothing and goes at the next prune. A
//! conversation joined again keeps its registers. [`POLL_CHOICES_MAX_CLIENT`]
//! is a client-side limit on the writer, not a decode bound.
//!
//! ## Wire
//!
//! ```text
//! domain(26) "lakko/sync/poll-choice/v1\0"
//! version(1) = 0x02
//! count(1)                    1..=32
//! count x {
//!     hlc(8, BE)              the register's ORIGINAL stamp
//!     ref_kind(1)             1 one-to-one, 2 group, 3 channel
//!     ref_id_len(1)           1..=64
//!     ref_id
//!     poll_message_id(8, BE)  i64
//!     choice_count(1)         0..=10
//!     choice(1)*              strictly ascending, each < 10
//!     set_version(4, BE)      the ring version of the anonymity set
//! }
//! ```
//!
//! Version `0x01` (no set version) is refused: a flag day. Entries are
//! sorted by `(ref kind, ref id, poll message id)`, strictly
//! increasing; nothing may trail; the encoding is canonical and every
//! violation refuses the whole envelope. It is not in the growth snapshot: a
//! device re-sends [`super::SyncedState::poll_choice_envelopes`] on growth, as
//! it does the own channel reactions.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account (inside a
//! DeviceGroup application message) or from a backup. A hostile device of the
//! account can at most make the others show a choice the account did not make
//! (it could cast that vote anyway). The register never reaches a tally. The
//! decoder is bounded and total.
//!
//! [ADR-0090]: ../../../docs/adr/0090-attachment-kinds.md

use thiserror::Error;

use super::entry::{
    put_ref, take_ref, take_u32, take_u64, take_u8, BoundsError, CodecError, ConversationRef,
    Stamped,
};

/// The most options a poll has, and so the most choices and the bound on an
/// index (ADR-0090 section 3.1).
pub const POLL_CHOICE_MAX_OPTIONS: usize = 10;

/// The most registers a client writes (an update of a held one is always
/// written). Chosen, not measured. The fold never refuses an entry for it.
pub const POLL_CHOICES_MAX_CLIENT: usize = 10_000;

/// Domain separator of the poll-choice envelope, trailing NUL included.
/// Frozen.
pub const POLL_CHOICE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/poll-choice/v1\0";

/// The version byte this build writes and accepts: `0x02`, the layout with
/// the set version (2026-11-15). `0x01` is refused.
pub const POLL_CHOICE_ENVELOPE_VERSION: u8 = 0x02;

/// Most entries one envelope carries.
pub const POLL_CHOICE_ENVELOPE_MAX_ENTRIES: usize = 32;

/// Longest conversation-reference id (the same bound as every register's).
const REF_ID_MAX: usize = super::entry::CONVERSATION_REF_ID_MAX_LEN;

/// Fixed part of one entry: hlc, ref kind, ref id length, message id, count,
/// set version.
const ENTRY_FIXED_LEN: usize = 8 + 1 + 1 + 8 + 1 + 4;

/// Shortest valid envelope: the header and one entry with a one-byte ref id
/// and no choices.
pub const POLL_CHOICE_ENVELOPE_MIN_LEN: usize =
    POLL_CHOICE_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + ENTRY_FIXED_LEN + 1;

/// Longest valid envelope.
pub const POLL_CHOICE_ENVELOPE_MAX_LEN: usize = POLL_CHOICE_ENVELOPE_DOMAIN_V1.len()
    + 1
    + 1
    + POLL_CHOICE_ENVELOPE_MAX_ENTRIES * (ENTRY_FIXED_LEN + REF_ID_MAX + POLL_CHOICE_MAX_OPTIONS);

/// One own-choice register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollChoiceEntry {
    /// The conversation the poll is in.
    pub conversation: ConversationRef,
    /// The DS `messageId` of the poll row (a group) or the post cursor (a
    /// channel).
    pub poll_message_id: i64,
    /// The chosen option indices, strictly ascending; empty = retracted.
    pub choices: Vec<u8>,
    /// The ring version the account's anonymity set in this poll was drawn
    /// from at its first vote; 0 for a channel poll.
    pub set_version: u32,
}

impl PollChoiceEntry {
    /// Check the reference and the choices.
    ///
    /// # Errors
    ///
    /// [`BoundsError::RefIdLength`] or [`BoundsError::PollChoices`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        self.conversation.validate()?;
        if !choices_are_valid(&self.choices) {
            return Err(BoundsError::PollChoices);
        }
        Ok(())
    }

    /// The register key's parts, in the envelope's sort order.
    pub(crate) fn sort_key(&self) -> (u8, &[u8], i64) {
        (
            self.conversation.kind.to_wire(),
            self.conversation.id.as_slice(),
            self.poll_message_id,
        )
    }

    /// Append the body to `out`: the key, then the choices, then the set
    /// version. Call only on a validated entry.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        put_ref(out, &self.conversation);
        out.extend_from_slice(&self.poll_message_id.to_be_bytes());
        // A validated choice list holds at most 10 entries.
        out.push(u8::try_from(self.choices.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(&self.choices);
        out.extend_from_slice(&self.set_version.to_be_bytes());
    }

    /// Read a body written by [`Self::encode_body`]. The count is checked
    /// against its bound before the bytes are taken.
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let conversation = take_ref(cur)?;
        let poll_message_id = i64::from_be_bytes(take_u64(cur)?.to_be_bytes());
        let count = usize::from(take_u8(cur)?);
        if count > POLL_CHOICE_MAX_OPTIONS {
            return Err(BoundsError::PollChoices.into());
        }
        let mut choices = Vec::with_capacity(count);
        for _ in 0..count {
            choices.push(take_u8(cur)?);
        }
        if !choices_are_valid(&choices) {
            return Err(BoundsError::PollChoices.into());
        }
        let set_version = take_u32(cur)?;
        Ok(Self {
            conversation,
            poll_message_id,
            choices,
            set_version,
        })
    }
}

fn choices_are_valid(choices: &[u8]) -> bool {
    choices.len() <= POLL_CHOICE_MAX_OPTIONS
        && choices
            .iter()
            .all(|&c| usize::from(c) < POLL_CHOICE_MAX_OPTIONS)
        && choices.windows(2).all(|w| w[0] < w[1])
}

/// Why a poll-choice envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PollChoiceEnvelopeError {
    /// The bytes do not open with [`POLL_CHOICE_ENVELOPE_DOMAIN_V1`].
    #[error("not a poll-choice envelope")]
    WrongDomain,
    /// The version byte is not [`POLL_CHOICE_ENVELOPE_VERSION`].
    #[error("unknown poll-choice envelope version")]
    UnknownVersion,
    /// The count is zero or above [`POLL_CHOICE_ENVELOPE_MAX_ENTRIES`].
    #[error("poll-choice envelope count is out of bounds")]
    Count,
    /// The bytes ended early, or an entry is malformed (a reference out of
    /// bounds, a choice list out of bounds or not strictly ascending).
    #[error("poll-choice envelope is truncated or malformed")]
    Truncated,
    /// Entries were not strictly increasing by key.
    #[error("poll-choice envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in poll-choice envelope")]
    Trailing,
}

impl PollChoiceEnvelopeError {
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

/// `true` when `bytes` open with the poll-choice envelope domain: the cheap
/// routing question. Says nothing about validity.
#[must_use]
pub fn is_poll_choice_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(POLL_CHOICE_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=32 stamped registers, sorted by key.
///
/// # Errors
///
/// [`PollChoiceEnvelopeError::Count`] for none or too many entries,
/// [`PollChoiceEnvelopeError::NotCanonical`] for two entries of one register,
/// [`PollChoiceEnvelopeError::Truncated`] for an entry that breaks a bound.
pub fn compose_poll_choice_envelope(
    entries: &[Stamped<PollChoiceEntry>],
) -> Result<Vec<u8>, PollChoiceEnvelopeError> {
    if entries.is_empty() || entries.len() > POLL_CHOICE_ENVELOPE_MAX_ENTRIES {
        return Err(PollChoiceEnvelopeError::Count);
    }
    if entries.iter().any(|e| e.value.validate().is_err()) {
        return Err(PollChoiceEnvelopeError::Truncated);
    }
    let count = u8::try_from(entries.len()).map_err(|_| PollChoiceEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<PollChoiceEntry>> = entries.iter().collect();
    sorted.sort_by(|a, b| a.value.sort_key().cmp(&b.value.sort_key()));
    if sorted
        .windows(2)
        .any(|w| w[0].value.sort_key() == w[1].value.sort_key())
    {
        return Err(PollChoiceEnvelopeError::NotCanonical);
    }
    let mut out = Vec::with_capacity(POLL_CHOICE_ENVELOPE_MAX_LEN);
    out.extend_from_slice(POLL_CHOICE_ENVELOPE_DOMAIN_V1);
    out.push(POLL_CHOICE_ENVELOPE_VERSION);
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
/// Every [`PollChoiceEnvelopeError`]; the domain is checked first, then the
/// version, then the count, then each entry, then trailing bytes.
pub fn decode_poll_choice_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<PollChoiceEntry>>, PollChoiceEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(POLL_CHOICE_ENVELOPE_DOMAIN_V1)
        .ok_or(PollChoiceEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| PollChoiceEnvelopeError::Truncated)?;
    if version != POLL_CHOICE_ENVELOPE_VERSION {
        return Err(PollChoiceEnvelopeError::UnknownVersion);
    }
    let count = usize::from(take_u8(&mut cur).map_err(|_| PollChoiceEnvelopeError::Truncated)?);
    if count == 0 || count > POLL_CHOICE_ENVELOPE_MAX_ENTRIES {
        return Err(PollChoiceEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<PollChoiceEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| PollChoiceEnvelopeError::Truncated)?;
        let value =
            PollChoiceEntry::take_body(&mut cur).map_err(|_| PollChoiceEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.sort_key() >= value.sort_key() {
                return Err(PollChoiceEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(PollChoiceEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::entry::ConversationRefKind;
    use super::*;
    use proptest::prelude::*;

    fn entry(
        kind: ConversationRefKind,
        id: u8,
        message: i64,
        choices: &[u8],
        hlc: u64,
    ) -> Stamped<PollChoiceEntry> {
        Stamped {
            hlc,
            value: PollChoiceEntry {
                conversation: ConversationRef {
                    kind,
                    id: vec![id; 32],
                },
                poll_message_id: message,
                choices: choices.to_vec(),
                set_version: 3,
            },
        }
    }

    #[test]
    fn round_trip_and_canonical_order() {
        let a = entry(ConversationRefKind::Group, 1, 5, &[0, 3], 10);
        let b = entry(ConversationRefKind::Channel, 1, -2, &[], 11);
        let c = entry(ConversationRefKind::Group, 1, 4, &[9], 12);
        let wire = compose_poll_choice_envelope(&[a.clone(), b.clone(), c.clone()]).unwrap();
        assert!(is_poll_choice_envelope(&wire));
        assert_eq!(decode_poll_choice_envelope(&wire).unwrap(), vec![c, a, b]);
        assert!(wire.len() <= POLL_CHOICE_ENVELOPE_MAX_LEN);
    }

    #[test]
    fn bad_choices_and_duplicates_are_refused() {
        for bad in [&[1u8, 1][..], &[2, 1], &[10]] {
            assert_eq!(
                compose_poll_choice_envelope(&[entry(ConversationRefKind::Group, 1, 1, bad, 1)]),
                Err(PollChoiceEnvelopeError::Truncated)
            );
        }
        let a = entry(ConversationRefKind::Group, 1, 1, &[0], 1);
        let b = entry(ConversationRefKind::Group, 1, 1, &[1], 2);
        assert_eq!(
            compose_poll_choice_envelope(&[a, b]),
            Err(PollChoiceEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_poll_choice_envelope(&[]),
            Err(PollChoiceEnvelopeError::Count)
        );
    }

    #[test]
    fn wrong_domain_version_and_trailing_bytes_are_refused() {
        let wire =
            compose_poll_choice_envelope(&[entry(ConversationRefKind::Group, 1, 1, &[0], 1)])
                .unwrap();
        let mut other = wire.clone();
        other[POLL_CHOICE_ENVELOPE_DOMAIN_V1.len()] = 0x01;
        assert_eq!(
            decode_poll_choice_envelope(&other),
            Err(PollChoiceEnvelopeError::UnknownVersion)
        );
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_poll_choice_envelope(&trailing),
            Err(PollChoiceEnvelopeError::Trailing)
        );
        assert_eq!(
            decode_poll_choice_envelope(b"lakko/x"),
            Err(PollChoiceEnvelopeError::WrongDomain)
        );
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let mut prefixed = POLL_CHOICE_ENVELOPE_DOMAIN_V1.to_vec();
            prefixed.extend_from_slice(&bytes);
            for input in [&bytes, &prefixed] {
                if let Ok(entries) = decode_poll_choice_envelope(input) {
                    prop_assert_eq!(compose_poll_choice_envelope(&entries).unwrap(), input.clone());
                }
            }
        }
    }
}
