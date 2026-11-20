// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Polls (ADR-0092 section 3): the canonical binary poll body, the author
//! signature over it, and the fold of non-anonymous votes and closes.
//!
//! ```text
//! poll_body = "lakko/poll/v1\0" || 0x01 || id(16) || flags(u8: bit0 multiple,
//!             bit1 quiz, bit2 anonymous, bit3 close_at present)
//!             || question_len(u8) || question
//!             || option_count(u8: 2..=10) || (len(u8) || option)*
//!             || [quiz: correct(u8) || explanation_len(u16 BE) || explanation]
//!             || [close_at: close_at_ms(i64 BE)]
//!             || [anonymous: secret(32) || ring0_hash(32)]
//! author_sig = Ed25519 by the author's account key over
//!              "lakko/poll/author-sig/v1\0" || group_id || poll_body
//! ```
//!
//! Anonymous ballots are `lakko-ballot`'s; this module only carries the
//! anonymous fields so that crate can build on them, and [`fold`] counts
//! nothing for an anonymous poll.

use std::collections::BTreeMap;
use std::fmt;

use crate::content::{
    check_display_text, push_len_u16, push_len_u8, sign_author, verify_author, ContentError,
    Reader, ACCOUNT_KEY_LEN, AUTHOR_SIG_LEN, GROUP_ID_LEN,
};

/// Domain separator that opens a poll body. Frozen.
pub const POLL_DOMAIN: &[u8] = b"lakko/poll/v1\0";
/// The only poll body version this build reads.
pub const POLL_VERSION: u8 = 0x01;
/// Domain of the author signature. Frozen.
pub const POLL_AUTHOR_SIG_DOMAIN: &[u8] = b"lakko/poll/author-sig/v1\0";
/// Length of the random poll id.
pub const POLL_ID_LEN: usize = 16;
/// Longest question, in bytes (at least 1).
pub const MAX_POLL_QUESTION_BYTES: usize = 255;
/// Fewest options.
pub const MIN_POLL_OPTIONS: usize = 2;
/// Most options.
pub const MAX_POLL_OPTIONS: usize = 10;
/// Longest option text, in bytes (at least 1).
pub const MAX_POLL_OPTION_BYTES: usize = 100;
/// Longest quiz explanation, in bytes (may be empty).
pub const MAX_POLL_EXPLANATION_BYTES: usize = 600;
/// Length of an anonymous poll's secret.
pub const POLL_SECRET_LEN: usize = 32;
/// Length of an anonymous poll's ring-version-0 hash.
pub const POLL_RING_HASH_LEN: usize = 32;

const FLAG_MULTIPLE: u8 = 0x01;
const FLAG_QUIZ: u8 = 0x02;
const FLAG_ANONYMOUS: u8 = 0x04;
const FLAG_CLOSE_AT: u8 = 0x08;
const KNOWN_FLAGS: u8 = FLAG_MULTIPLE | FLAG_QUIZ | FLAG_ANONYMOUS | FLAG_CLOSE_AT;

/// The largest encoded poll body.
pub const MAX_POLL_BODY_LEN: usize = POLL_DOMAIN.len()
    + 1
    + POLL_ID_LEN
    + 1
    + 1
    + MAX_POLL_QUESTION_BYTES
    + 1
    + MAX_POLL_OPTIONS * (1 + MAX_POLL_OPTION_BYTES)
    + 1
    + 2
    + MAX_POLL_EXPLANATION_BYTES
    + 8
    + POLL_SECRET_LEN
    + POLL_RING_HASH_LEN;

/// The quiz part of a poll: which option is correct and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollQuiz {
    /// Index of the correct option.
    pub correct: u8,
    /// Explanation shown after voting; may be empty.
    pub explanation: String,
}

/// The anonymous part of a poll (ADR-0092 section 3.6): the poll secret the
/// ballot seal key derives from, and the hash of ring version 0. Its
/// `Debug` hides the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct PollAnonymous {
    /// The poll secret.
    pub secret: [u8; POLL_SECRET_LEN],
    /// `BLAKE3-derive-key("lakko/poll/ring/v1", ring version 0)`, computed by
    /// `lakko-ballot`; opaque here.
    pub ring0_hash: [u8; POLL_RING_HASH_LEN],
}

impl fmt::Debug for PollAnonymous {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollAnonymous")
            .field("secret", &"<redacted>")
            .field("ring0_hash", &self.ring0_hash)
            .finish()
    }
}

/// A poll body (ADR-0092 section 3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollBody {
    /// Random id.
    pub id: [u8; POLL_ID_LEN],
    /// The question, 1..=[`MAX_POLL_QUESTION_BYTES`] bytes of display text.
    pub question: String,
    /// The options, [`MIN_POLL_OPTIONS`]..=[`MAX_POLL_OPTIONS`], each
    /// 1..=[`MAX_POLL_OPTION_BYTES`] bytes of display text.
    pub options: Vec<String>,
    /// Several answers allowed. Never together with a quiz.
    pub multiple: bool,
    /// A quiz, when set.
    pub quiz: Option<PollQuiz>,
    /// Automatic close time (milliseconds since the epoch, positive).
    pub close_at_ms: Option<i64>,
    /// The anonymous fields, when the poll is anonymous.
    pub anonymous: Option<PollAnonymous>,
}

impl PollBody {
    /// Check every bound the encoding needs.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        check_display_text(&self.question, 1, MAX_POLL_QUESTION_BYTES, "question")?;
        if !(MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS).contains(&self.options.len()) {
            return Err(ContentError::OutOfRange {
                field: "optionCount",
            });
        }
        for option in &self.options {
            check_display_text(option, 1, MAX_POLL_OPTION_BYTES, "option")?;
        }
        if let Some(quiz) = &self.quiz {
            if self.multiple {
                return Err(ContentError::UnknownFlags);
            }
            if usize::from(quiz.correct) >= self.options.len() {
                return Err(ContentError::OutOfRange { field: "correct" });
            }
            check_display_text(
                &quiz.explanation,
                0,
                MAX_POLL_EXPLANATION_BYTES,
                "explanation",
            )?;
        }
        if let Some(at) = self.close_at_ms {
            if at <= 0 {
                return Err(ContentError::OutOfRange { field: "closeAt" });
            }
        }
        Ok(())
    }

    /// The canonical encoding.
    ///
    /// # Errors
    ///
    /// What [`Self::validate`] refuses.
    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        self.validate()?;
        let mut out = Vec::with_capacity(MAX_POLL_BODY_LEN);
        out.extend_from_slice(POLL_DOMAIN);
        out.push(POLL_VERSION);
        out.extend_from_slice(&self.id);
        let mut flags = 0u8;
        if self.multiple {
            flags |= FLAG_MULTIPLE;
        }
        if self.quiz.is_some() {
            flags |= FLAG_QUIZ;
        }
        if self.anonymous.is_some() {
            flags |= FLAG_ANONYMOUS;
        }
        if self.close_at_ms.is_some() {
            flags |= FLAG_CLOSE_AT;
        }
        out.push(flags);
        push_len_u8(&mut out, self.question.len(), "question")?;
        out.extend_from_slice(self.question.as_bytes());
        push_len_u8(&mut out, self.options.len(), "optionCount")?;
        for option in &self.options {
            push_len_u8(&mut out, option.len(), "option")?;
            out.extend_from_slice(option.as_bytes());
        }
        if let Some(quiz) = &self.quiz {
            out.push(quiz.correct);
            push_len_u16(&mut out, quiz.explanation.len(), "explanation")?;
            out.extend_from_slice(quiz.explanation.as_bytes());
        }
        if let Some(at) = self.close_at_ms {
            out.extend_from_slice(&at.to_be_bytes());
        }
        if let Some(anon) = &self.anonymous {
            out.extend_from_slice(&anon.secret);
            out.extend_from_slice(&anon.ring0_hash);
        }
        Ok(out)
    }

    /// Decode a canonical poll body. Strict: unknown flags, a bound broken,
    /// a quiz with multiple answers, or a trailing byte refuse the whole poll.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.len() > MAX_POLL_BODY_LEN {
            return Err(ContentError::TooLarge);
        }
        let mut r = Reader::new(bytes);
        if r.take(POLL_DOMAIN.len())? != POLL_DOMAIN {
            return Err(ContentError::WrongDomain);
        }
        let version = r.u8()?;
        if version != POLL_VERSION {
            return Err(ContentError::UnknownVersion(version));
        }
        let id = r.array::<POLL_ID_LEN>()?;
        let flags = r.u8()?;
        if flags & !KNOWN_FLAGS != 0 {
            return Err(ContentError::UnknownFlags);
        }
        let question_len = usize::from(r.u8()?);
        let question = r.str(question_len, "question")?.to_owned();
        let count = usize::from(r.u8()?);
        if !(MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS).contains(&count) {
            return Err(ContentError::OutOfRange {
                field: "optionCount",
            });
        }
        let mut options = Vec::with_capacity(count);
        for _ in 0..count {
            let len = usize::from(r.u8()?);
            options.push(r.str(len, "option")?.to_owned());
        }
        let quiz = if flags & FLAG_QUIZ != 0 {
            let correct = r.u8()?;
            let len = usize::from(r.u16()?);
            if len > MAX_POLL_EXPLANATION_BYTES {
                return Err(ContentError::Text {
                    field: "explanation",
                    reason: crate::content::TextError::TooLong,
                });
            }
            let explanation = r.str(len, "explanation")?.to_owned();
            Some(PollQuiz {
                correct,
                explanation,
            })
        } else {
            None
        };
        let close_at_ms = if flags & FLAG_CLOSE_AT != 0 {
            Some(r.i64()?)
        } else {
            None
        };
        let anonymous = if flags & FLAG_ANONYMOUS != 0 {
            Some(PollAnonymous {
                secret: r.array()?,
                ring0_hash: r.array()?,
            })
        } else {
            None
        };
        r.finish()?;
        let body = Self {
            id,
            question,
            options,
            multiple: flags & FLAG_MULTIPLE != 0,
            quiz,
            close_at_ms,
            anonymous,
        };
        body.validate()?;
        Ok(body)
    }

    /// Whether `choices` is a valid vote in this poll: empty (a retract), or
    /// strictly ascending option indices, exactly one unless `multiple`.
    #[must_use]
    pub fn choices_valid(&self, choices: &[u8]) -> bool {
        if choices.is_empty() {
            return true;
        }
        if !self.multiple && choices.len() != 1 {
            return false;
        }
        choices_shape_valid(choices) && choices.iter().all(|&c| usize::from(c) < self.options.len())
    }
}

/// The poll as it rides in the envelope: the body and the author signature.
/// The signature is absent only in a channel post, where the post signature
/// authenticates the publisher (ADR-0092 section 4.1); a group receiver
/// refuses a poll without one ([`SignedPoll::verify`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPoll {
    /// The poll.
    pub body: PollBody,
    /// Ed25519 by the author's account key; `None` in a channel post.
    pub author_sig: Option<[u8; AUTHOR_SIG_LEN]>,
}

impl SignedPoll {
    /// Sign `body` for `group_id` with the author's account seed.
    ///
    /// # Errors
    ///
    /// What [`PollBody::encode`] refuses.
    pub fn sign(
        body: PollBody,
        group_id: &[u8; GROUP_ID_LEN],
        author_seed: &[u8; 32],
    ) -> Result<Self, ContentError> {
        let bytes = body.encode()?;
        let sig = sign_author(POLL_AUTHOR_SIG_DOMAIN, group_id, &bytes, author_seed);
        Ok(Self {
            body,
            author_sig: Some(sig),
        })
    }

    /// A channel poll: no author signature.
    #[must_use]
    pub const fn unsigned(body: PollBody) -> Self {
        Self {
            body,
            author_sig: None,
        }
    }

    /// Check the author signature against the account key the MLS sender
    /// (or, in a hand-over, the stated author) resolves to.
    ///
    /// # Errors
    ///
    /// [`ContentError::SignatureMissing`], [`ContentError::SignatureInvalid`],
    /// or what [`PollBody::encode`] refuses.
    pub fn verify(
        &self,
        group_id: &[u8; GROUP_ID_LEN],
        author_key: &[u8; ACCOUNT_KEY_LEN],
    ) -> Result<(), ContentError> {
        let sig = self.author_sig.ok_or(ContentError::SignatureMissing)?;
        let bytes = self.body.encode()?;
        verify_author(POLL_AUTHOR_SIG_DOMAIN, group_id, &bytes, author_key, &sig)
    }
}

/// The longest frozen channel-poll tally a close carries (ADR-0092 section
/// 4.3): `lakko_channels::POLL_TALLY_MAX_LEN`, copied because
/// `lakko-channels` depends on this crate; the FFI crates pin the two equal.
pub const MAX_POLL_CLOSE_TALLY_BYTES: usize = 118;

/// The structural rule for a `choices` list on the wire, before the poll is
/// known: at most [`MAX_POLL_OPTIONS`] indices, each below it, strictly
/// ascending. Empty is a retract.
#[must_use]
pub fn choices_shape_valid(choices: &[u8]) -> bool {
    choices.len() <= MAX_POLL_OPTIONS
        && choices.iter().all(|&c| usize::from(c) < MAX_POLL_OPTIONS)
        && choices.windows(2).all(|w| w[0] < w[1])
}

/// One non-anonymous vote op as the client stored it at receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollVoteRecord {
    /// DS `messageId` of the vote row.
    pub message_id: i64,
    /// DS `created_at` of the vote row, milliseconds.
    pub created_at_ms: i64,
    /// The voter's account signature key, resolved from the MLS sender at
    /// receipt (C-68).
    pub voter: [u8; ACCOUNT_KEY_LEN],
    /// The choices (empty = retract).
    pub choices: Vec<u8>,
}

/// One close op as the client stored it at receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollCloseRecord {
    /// DS `messageId` of the close row.
    pub message_id: i64,
    /// The closer's account signature key, resolved at receipt (C-68).
    pub closer: [u8; ACCOUNT_KEY_LEN],
    /// Whether the closer was the owner or an admin of the live role table
    /// at receipt (the caller's reading; a one-to-one has none).
    pub closer_is_admin: bool,
}

/// The folded state of a poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollTally {
    /// Votes per option, in option order. All zero for an anonymous poll.
    pub counts: Vec<u32>,
    /// How many voters have a counted, non-empty vote.
    pub voters: u32,
    /// Each counted voter's choices, sorted by voter key. Empty for an
    /// anonymous poll.
    pub choices_by_voter: Vec<([u8; ACCOUNT_KEY_LEN], Vec<u8>)>,
    /// DS `messageId` of the close that took effect, if any.
    pub closed_by: Option<i64>,
    /// The automatic close time from the body, if any.
    pub close_at_ms: Option<i64>,
}

impl PollTally {
    /// Whether the poll is closed at `now_ms`: by a close op, or by its
    /// automatic close time.
    #[must_use]
    pub fn is_closed(&self, now_ms: i64) -> bool {
        self.closed_by.is_some() || self.close_at_ms.is_some_and(|at| now_ms >= at)
    }

    /// `voter`'s counted choices, if any.
    #[must_use]
    pub fn choices_of(&self, voter: &[u8; ACCOUNT_KEY_LEN]) -> Option<&[u8]> {
        self.choices_by_voter
            .binary_search_by(|(k, _)| k.cmp(voter))
            .ok()
            .map(|i| self.choices_by_voter[i].1.as_slice())
    }
}

/// The DS `messageId` of the close that takes effect: the earliest close row
/// after the poll's own row by its author or an admin. Closing is final.
#[must_use]
pub fn effective_close(
    author: &[u8; ACCOUNT_KEY_LEN],
    poll_message_id: i64,
    closes: &[PollCloseRecord],
) -> Option<i64> {
    closes
        .iter()
        .filter(|c| c.message_id > poll_message_id)
        .filter(|c| c.closer == *author || c.closer_is_admin)
        .map(|c| c.message_id)
        .min()
}

/// Fold the non-anonymous votes of a poll (ADR-0092 section 3.2): last
/// writer wins per voter in DS `messageId` order; a vote counts only when it
/// follows the poll row, precedes the effective close and the automatic
/// close time, and its choices are valid for this poll; an empty vote
/// retracts. The result does not depend on the order of `votes` or
/// `closes`. For an anonymous poll no vote counts (ballots are
/// `lakko-ballot`'s); the close state is still folded.
#[must_use]
pub fn fold(
    poll: &PollBody,
    author: &[u8; ACCOUNT_KEY_LEN],
    poll_message_id: i64,
    votes: &[PollVoteRecord],
    closes: &[PollCloseRecord],
) -> PollTally {
    let closed_by = effective_close(author, poll_message_id, closes);
    let mut counts = vec![0u32; poll.options.len()];
    let mut latest: BTreeMap<[u8; ACCOUNT_KEY_LEN], &PollVoteRecord> = BTreeMap::new();
    if poll.anonymous.is_none() {
        for vote in votes {
            if vote.message_id <= poll_message_id
                || closed_by.is_some_and(|c| vote.message_id >= c)
                || poll.close_at_ms.is_some_and(|at| vote.created_at_ms >= at)
                || !poll.choices_valid(&vote.choices)
            {
                continue;
            }
            match latest.get(&vote.voter) {
                Some(prev)
                    if (prev.message_id, prev.choices.as_slice())
                        >= (vote.message_id, vote.choices.as_slice()) => {},
                _ => {
                    latest.insert(vote.voter, vote);
                },
            }
        }
    }
    let mut choices_by_voter = Vec::new();
    let mut voter_count = 0u32;
    for (voter, vote) in latest {
        if vote.choices.is_empty() {
            continue;
        }
        voter_count = voter_count.saturating_add(1);
        for &c in &vote.choices {
            if let Some(slot) = counts.get_mut(usize::from(c)) {
                *slot = slot.saturating_add(1);
            }
        }
        choices_by_voter.push((voter, vote.choices.clone()));
    }
    PollTally {
        counts,
        voters: voter_count,
        choices_by_voter,
        closed_by,
        close_at_ms: poll.close_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> PollBody {
        PollBody {
            id: [3; POLL_ID_LEN],
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Soup".into(), "Salad".into()],
            multiple: false,
            quiz: None,
            close_at_ms: None,
            anonymous: None,
        }
    }

    fn text(max: usize) -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9 ?.,é日]{1,40}")
            .unwrap_or_else(|_| unreachable!())
            .prop_filter("bound", move |s| s.len() <= max)
    }

    prop_compose! {
        fn any_poll()(
            id in any::<[u8; POLL_ID_LEN]>(),
            question in text(MAX_POLL_QUESTION_BYTES),
            options in proptest::collection::vec(text(MAX_POLL_OPTION_BYTES), MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS),
            kind in 0u8..3,
            correct in 0u8..10,
            explanation in proptest::option::of(text(MAX_POLL_EXPLANATION_BYTES)),
            close in proptest::option::of(1i64..i64::MAX),
            anon in proptest::option::of((any::<[u8; 32]>(), any::<[u8; 32]>())),
        ) -> PollBody {
            let n = u8::try_from(options.len()).unwrap_or(2);
            let quiz = (kind == 2).then(|| PollQuiz { correct: correct % n, explanation: explanation.unwrap_or_default() });
            PollBody {
                id, question, options,
                multiple: kind == 1,
                quiz,
                close_at_ms: close,
                anonymous: anon.map(|(secret, ring0_hash)| PollAnonymous { secret, ring0_hash }),
            }
        }
    }

    proptest! {
        #[test]
        fn round_trip_and_canonical(p in any_poll()) {
            let bytes = p.encode().unwrap();
            prop_assert!(bytes.len() <= MAX_POLL_BODY_LEN);
            let back = PollBody::decode(&bytes).unwrap();
            prop_assert_eq!(&back, &p);
            prop_assert_eq!(back.encode().unwrap(), bytes);
        }

        #[test]
        fn hostile_bytes_never_panic_and_accepted_ones_are_canonical(
            bytes in proptest::collection::vec(any::<u8>(), 0..2200),
            prefix in any::<bool>(),
        ) {
            let mut input = if prefix { POLL_DOMAIN.to_vec() } else { Vec::new() };
            if prefix { input.push(POLL_VERSION); }
            input.extend_from_slice(&bytes);
            if let Ok(p) = PollBody::decode(&input) {
                prop_assert_eq!(p.encode().unwrap(), input);
            }
        }

        #[test]
        fn a_truncated_or_extended_body_is_refused(p in any_poll(), cut in 1usize..64, extra in any::<u8>()) {
            let bytes = p.encode().unwrap();
            let cut = cut.min(bytes.len());
            prop_assert!(PollBody::decode(&bytes[..bytes.len() - cut]).is_err());
            let mut longer = bytes.clone();
            longer.push(extra);
            prop_assert!(PollBody::decode(&longer).is_err());
        }

        #[test]
        fn fold_is_independent_of_input_order(
            ops in proptest::collection::vec((1i64..40, 0u8..4, proptest::collection::vec(0u8..4, 0..3)), 0..30),
            seed in any::<u64>(),
        ) {
            let poll = PollBody { multiple: true, ..sample() };
            let author = [9u8; 32];
            let mut votes: Vec<PollVoteRecord> = ops.iter().map(|(m, v, c)| {
                let mut c = c.clone(); c.sort_unstable(); c.dedup();
                PollVoteRecord { message_id: *m, created_at_ms: *m, voter: [*v; 32], choices: c }
            }).collect();
            let a = fold(&poll, &author, 0, &votes, &[]);
            // A deterministic shuffle.
            let mut s = seed;
            for i in (1..votes.len()).rev() {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let j = usize::try_from(s >> 33).unwrap_or(0) % (i + 1);
                votes.swap(i, j);
            }
            let b = fold(&poll, &author, 0, &votes, &[]);
            prop_assert_eq!(a, b);
        }
    }

    #[test]
    fn encode_refuses_what_decode_refuses() {
        let mut p = sample();
        p.options = vec!["only one".into()];
        assert!(p.encode().is_err());
        let mut p = sample();
        p.multiple = true;
        p.quiz = Some(PollQuiz {
            correct: 0,
            explanation: String::new(),
        });
        assert!(p.encode().is_err());
        let mut p = sample();
        p.quiz = Some(PollQuiz {
            correct: 3,
            explanation: String::new(),
        });
        assert!(p.encode().is_err());
        let mut p = sample();
        p.question = "a\nb".into();
        assert!(p.encode().is_err());
        let mut p = sample();
        p.question = String::new();
        assert!(p.encode().is_err());
        let mut p = sample();
        p.close_at_ms = Some(0);
        assert!(p.encode().is_err());
    }

    #[test]
    fn decode_refuses_unknown_flags_and_versions() {
        let mut bytes = sample().encode().unwrap();
        bytes[POLL_DOMAIN.len() + 1 + POLL_ID_LEN] |= 0x10;
        assert_eq!(PollBody::decode(&bytes), Err(ContentError::UnknownFlags));
        let mut bytes = sample().encode().unwrap();
        bytes[POLL_DOMAIN.len()] = 2;
        assert_eq!(
            PollBody::decode(&bytes),
            Err(ContentError::UnknownVersion(2))
        );
        let mut bytes = sample().encode().unwrap();
        bytes[0] = b'x';
        assert_eq!(PollBody::decode(&bytes), Err(ContentError::WrongDomain));
    }

    #[test]
    fn the_author_signature_binds_the_group_and_the_author() {
        let seed = [5u8; 32];
        let key = lakko_crypto::signing::SigningKey::from_seed(&seed)
            .verifying_key()
            .to_bytes();
        let g = [1u8; GROUP_ID_LEN];
        let signed = SignedPoll::sign(sample(), &g, &seed).unwrap();
        assert!(signed.verify(&g, &key).is_ok());
        assert!(signed.verify(&[2u8; GROUP_ID_LEN], &key).is_err());
        assert!(signed.verify(&g, &[4u8; 32]).is_err());
        let mut altered = signed.clone();
        altered.body.question = "Dinner?".into();
        assert!(altered.verify(&g, &key).is_err());
        assert_eq!(
            SignedPoll::unsigned(sample()).verify(&g, &key),
            Err(ContentError::SignatureMissing)
        );
    }

    fn vote(m: i64, v: u8, c: &[u8]) -> PollVoteRecord {
        PollVoteRecord {
            message_id: m,
            created_at_ms: m * 1000,
            voter: [v; 32],
            choices: c.to_vec(),
        }
    }

    #[test]
    fn last_writer_wins_and_an_empty_vote_retracts() {
        let poll = sample();
        let author = [9; 32];
        let t = fold(
            &poll,
            &author,
            10,
            &[
                vote(11, 1, &[0]),
                vote(12, 2, &[1]),
                vote(13, 1, &[2]),
                vote(14, 2, &[]),
            ],
            &[],
        );
        assert_eq!(t.counts, vec![0, 0, 1]);
        assert_eq!(t.voters, 1);
        assert_eq!(t.choices_of(&[1; 32]), Some(&[2u8][..]));
        assert_eq!(t.choices_of(&[2; 32]), None);
    }

    #[test]
    fn invalid_votes_are_dropped_not_counted_as_retracts() {
        let poll = sample();
        let t = fold(
            &poll,
            &[9; 32],
            10,
            &[
                vote(11, 1, &[0]),
                vote(12, 1, &[0, 1]), // two choices in a single-answer poll
                vote(13, 1, &[7]),    // out of range
                vote(9, 2, &[1]),     // before the poll row
            ],
            &[],
        );
        assert_eq!(t.counts, vec![1, 0, 0]);
        assert_eq!(t.voters, 1);
    }

    #[test]
    fn a_close_by_the_author_or_an_admin_ends_voting_and_others_are_ignored() {
        let poll = sample();
        let author = [9; 32];
        let closes = [
            PollCloseRecord {
                message_id: 12,
                closer: [3; 32],
                closer_is_admin: false,
            },
            PollCloseRecord {
                message_id: 14,
                closer: [4; 32],
                closer_is_admin: true,
            },
            PollCloseRecord {
                message_id: 16,
                closer: author,
                closer_is_admin: false,
            },
        ];
        let t = fold(
            &poll,
            &author,
            10,
            &[vote(13, 1, &[0]), vote(15, 2, &[1])],
            &closes,
        );
        assert_eq!(t.closed_by, Some(14));
        assert_eq!(t.counts, vec![1, 0, 0]);
        assert!(t.is_closed(0));
    }

    #[test]
    fn the_automatic_close_time_uses_the_ds_timestamp() {
        let poll = PollBody {
            close_at_ms: Some(12_000),
            ..sample()
        };
        let t = fold(
            &poll,
            &[9; 32],
            10,
            &[vote(11, 1, &[0]), vote(12, 2, &[1])],
            &[],
        );
        assert_eq!(t.counts, vec![1, 0, 0]);
        assert!(!t.is_closed(11_999));
        assert!(t.is_closed(12_000));
    }

    #[test]
    fn an_anonymous_poll_counts_no_named_vote() {
        let poll = PollBody {
            anonymous: Some(PollAnonymous {
                secret: [1; 32],
                ring0_hash: [2; 32],
            }),
            ..sample()
        };
        let t = fold(&poll, &[9; 32], 10, &[vote(11, 1, &[0])], &[]);
        assert_eq!(t.counts, vec![0, 0, 0]);
        assert_eq!(t.voters, 0);
        assert!(!format!("{poll:?}").contains("[1, 1"));
    }

    #[test]
    fn choice_shape() {
        assert!(choices_shape_valid(&[]));
        assert!(choices_shape_valid(&[0, 3, 9]));
        assert!(!choices_shape_valid(&[1, 1]));
        assert!(!choices_shape_valid(&[2, 1]));
        assert!(!choices_shape_valid(&[10]));
    }
}
