// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 content messages on the UniFFI surface: polls, checklists,
//! locations, contact cards and articles; the ops on them (vote, close,
//! tick, add, edit); the folds; the state snapshot a history bundle or the
//! vault carries; and the editor limits.
//!
//! Every rule lives in `lakko-attachments`. This module only converts: byte
//! fields are raw bytes here and fixed-length arrays in the core, and a
//! wrong length is a caller bug refused with [`FfiError::Content`].
//!
//! Errors: [`FfiError::Content`] carries `"<code>: <reason>"`, the code
//! being `lakko_attachments::ContentError::code` (`"question.tooLong"`,
//! `"signatureInvalid"`, ...) or a byte-length code of this module
//! (`"<field>.badLength"`).

use lakko_attachments as inner;
use lakko_attachments::{article, checklist, content_state as cs, poll};

use crate::attachments::{refs_from_ffi, AttachmentRef};
use crate::FfiError;

pub(crate) fn content_err(e: inner::ContentError) -> FfiError {
    FfiError::Content(format!("{}: {e}", e.code()))
}

fn bytes<const N: usize>(v: &[u8], field: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        FfiError::Content(format!(
            "{field}.badLength: {field} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

fn opt_sig(v: Option<Vec<u8>>) -> Result<Option<[u8; 64]>, FfiError> {
    v.map(|s| bytes::<64>(&s, "authorSig")).transpose()
}

// ============================================================================
// Poll
// ============================================================================

/// A quiz poll's answer (ADR-0092 section 3.3).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollQuiz {
    /// Index of the correct option.
    pub correct: u8,
    /// The explanation shown after voting; may be empty.
    pub explanation: String,
}

/// The anonymous-poll material (ADR-0092 section 3.6). Secret: the poll's
/// ballot seal key is derived from `secret`.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct PollAnonymous {
    /// 32 bytes; feeds `PollBallotKeys`.
    pub secret: Vec<u8>,
    /// 32 bytes: the hash of ring version 0 (`pollRingHash`).
    pub ring0_hash: Vec<u8>,
}

impl std::fmt::Debug for PollAnonymous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollAnonymous")
            .field("secret", &"<redacted>")
            .field("ring0_hash", &self.ring0_hash)
            .finish()
    }
}

/// A poll's body (ADR-0092 section 3).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollBody {
    /// 16 random bytes naming the poll.
    pub id: Vec<u8>,
    /// The question.
    pub question: String,
    /// The options, 2..=10.
    pub options: Vec<String>,
    /// Several options may be chosen.
    pub multiple: bool,
    /// A quiz answer, or `None`.
    pub quiz: Option<PollQuiz>,
    /// The automatic close time, unix milliseconds, or `None`.
    pub close_at_ms: Option<i64>,
    /// Anonymous-poll material, or `None` for a poll that shows who voted.
    pub anonymous: Option<PollAnonymous>,
}

/// A poll with its author signature (64 bytes; `None` only in a channel
/// post).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SignedPoll {
    /// The poll.
    pub body: PollBody,
    /// Ed25519 by the author's account key over the body and the group id.
    pub author_sig: Option<Vec<u8>>,
}

impl TryFrom<PollBody> for poll::PollBody {
    type Error = FfiError;
    fn try_from(b: PollBody) -> Result<Self, FfiError> {
        Ok(Self {
            id: bytes::<{ poll::POLL_ID_LEN }>(&b.id, "pollId")?,
            question: b.question,
            options: b.options,
            multiple: b.multiple,
            quiz: b.quiz.map(|q| poll::PollQuiz {
                correct: q.correct,
                explanation: q.explanation,
            }),
            close_at_ms: b.close_at_ms,
            anonymous: b
                .anonymous
                .map(|a| -> Result<poll::PollAnonymous, FfiError> {
                    Ok(poll::PollAnonymous {
                        secret: bytes::<{ poll::POLL_SECRET_LEN }>(&a.secret, "pollSecret")?,
                        ring0_hash: bytes::<{ poll::POLL_RING_HASH_LEN }>(
                            &a.ring0_hash,
                            "ring0Hash",
                        )?,
                    })
                })
                .transpose()?,
        })
    }
}

impl From<poll::PollBody> for PollBody {
    fn from(b: poll::PollBody) -> Self {
        Self {
            id: b.id.to_vec(),
            question: b.question,
            options: b.options,
            multiple: b.multiple,
            quiz: b.quiz.map(|q| PollQuiz {
                correct: q.correct,
                explanation: q.explanation,
            }),
            close_at_ms: b.close_at_ms,
            anonymous: b.anonymous.map(|a| PollAnonymous {
                secret: a.secret.to_vec(),
                ring0_hash: a.ring0_hash.to_vec(),
            }),
        }
    }
}

impl TryFrom<SignedPoll> for inner::SignedPoll {
    type Error = FfiError;
    fn try_from(p: SignedPoll) -> Result<Self, FfiError> {
        Ok(Self {
            body: p.body.try_into()?,
            author_sig: opt_sig(p.author_sig)?,
        })
    }
}

impl From<inner::SignedPoll> for SignedPoll {
    fn from(p: inner::SignedPoll) -> Self {
        Self {
            body: p.body.into(),
            author_sig: p.author_sig.map(|s| s.to_vec()),
        }
    }
}

/// Sign a poll for the group `group_id` (32 bytes) with the author's
/// 32-byte account seed.
///
/// # Errors
///
/// [`FfiError::Content`] for a body that breaks a bound or a wrong length.
#[uniffi::export]
pub fn sign_poll(
    body: PollBody,
    group_id: Vec<u8>,
    author_seed: Vec<u8>,
) -> Result<SignedPoll, FfiError> {
    let group = bytes::<32>(&group_id, "groupId")?;
    let seed = bytes::<32>(&author_seed, "authorSeed")?;
    inner::SignedPoll::sign(body.try_into()?, &group, &seed)
        .map(Into::into)
        .map_err(content_err)
}

/// Check a poll's author signature against the account key the MLS sender
/// resolves to (C-68). A group receiver drops a poll that fails.
///
/// # Errors
///
/// [`FfiError::Content`] `signatureMissing` / `signatureInvalid`, or a bound.
#[uniffi::export]
pub fn verify_poll(
    poll: SignedPoll,
    group_id: Vec<u8>,
    author_key: Vec<u8>,
) -> Result<(), FfiError> {
    let group = bytes::<32>(&group_id, "groupId")?;
    let key = bytes::<32>(&author_key, "authorKey")?;
    inner::SignedPoll::try_from(poll)?
        .verify(&group, &key)
        .map_err(content_err)
}

/// Whether `choices` is a valid vote for this poll (strictly ascending, in
/// range, one choice unless `multiple`). Empty is a retract and valid.
///
/// # Errors
///
/// [`FfiError::Content`] for a wrong-length id in `body`.
#[uniffi::export]
pub fn poll_choices_valid(body: PollBody, choices: Vec<u8>) -> Result<bool, FfiError> {
    Ok(poll::PollBody::try_from(body)?.choices_valid(&choices))
}

/// One non-anonymous vote as stored at receipt (ADR-0092 section 3.2).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollVoteRecord {
    /// DS `messageId` of the vote row.
    pub message_id: i64,
    /// DS `created_at` of the vote row, milliseconds.
    pub created_at_ms: i64,
    /// The voter's 32-byte account key, resolved at receipt (C-68).
    pub voter: Vec<u8>,
    /// The choices (empty = retract).
    pub choices: Vec<u8>,
}

/// One close op on a poll or checklist as stored at receipt.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollCloseRecord {
    /// DS `messageId` of the close row.
    pub message_id: i64,
    /// The closer's 32-byte account key, resolved at receipt (C-68).
    pub closer: Vec<u8>,
    /// Whether the closer was the owner or an admin at receipt.
    pub closer_is_admin: bool,
}

/// One voter's counted choices.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollVoterChoices {
    /// The voter's account key.
    pub voter: Vec<u8>,
    /// The choices.
    pub choices: Vec<u8>,
}

/// A non-anonymous poll's folded state.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollTally {
    /// Votes per option. All zero for an anonymous poll.
    pub counts: Vec<u32>,
    /// Voters with a counted, non-empty vote.
    pub voters: u32,
    /// Each counted voter's choices, sorted by voter key.
    pub choices_by_voter: Vec<PollVoterChoices>,
    /// DS `messageId` of the close that took effect, if any.
    pub closed_by: Option<i64>,
    /// The automatic close time from the body, if any.
    pub close_at_ms: Option<i64>,
}

impl From<poll::PollTally> for PollTally {
    fn from(t: poll::PollTally) -> Self {
        Self {
            counts: t.counts,
            voters: t.voters,
            choices_by_voter: t
                .choices_by_voter
                .into_iter()
                .map(|(voter, choices)| PollVoterChoices {
                    voter: voter.to_vec(),
                    choices,
                })
                .collect(),
            closed_by: t.closed_by,
            close_at_ms: t.close_at_ms,
        }
    }
}

impl TryFrom<PollTally> for poll::PollTally {
    type Error = FfiError;
    fn try_from(t: PollTally) -> Result<Self, FfiError> {
        Ok(Self {
            counts: t.counts,
            voters: t.voters,
            choices_by_voter: t
                .choices_by_voter
                .into_iter()
                .map(|v| Ok((bytes::<32>(&v.voter, "voter")?, v.choices)))
                .collect::<Result<_, FfiError>>()?,
            closed_by: t.closed_by,
            close_at_ms: t.close_at_ms,
        })
    }
}

fn close_records(closes: Vec<PollCloseRecord>) -> Result<Vec<poll::PollCloseRecord>, FfiError> {
    closes
        .into_iter()
        .map(|c| {
            Ok(poll::PollCloseRecord {
                message_id: c.message_id,
                closer: bytes::<32>(&c.closer, "closer")?,
                closer_is_admin: c.closer_is_admin,
            })
        })
        .collect()
}

/// Fold a non-anonymous poll's votes and closes (ADR-0092 section 3.2):
/// last writer per voter, only votes after the poll row and before the
/// effective close. Independent of input order.
///
/// # Errors
///
/// [`FfiError::Content`] for a wrong-length key or id.
#[uniffi::export]
pub fn fold_poll(
    body: PollBody,
    author: Vec<u8>,
    poll_message_id: i64,
    votes: Vec<PollVoteRecord>,
    closes: Vec<PollCloseRecord>,
) -> Result<PollTally, FfiError> {
    let body = poll::PollBody::try_from(body)?;
    let author = bytes::<32>(&author, "author")?;
    let votes = votes
        .into_iter()
        .map(|v| {
            Ok(poll::PollVoteRecord {
                message_id: v.message_id,
                created_at_ms: v.created_at_ms,
                voter: bytes::<32>(&v.voter, "voter")?,
                choices: v.choices,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(poll::fold(
        &body,
        &author,
        poll_message_id,
        &votes,
        &close_records(closes)?,
    )
    .into())
}

/// Whether a folded poll is closed at `now_ms`.
#[uniffi::export]
#[must_use]
pub fn poll_tally_is_closed(tally: PollTally, now_ms: i64) -> bool {
    tally.closed_by.is_some() || tally.close_at_ms.is_some_and(|at| now_ms >= at)
}

// ============================================================================
// Checklist
// ============================================================================

/// One item of a checklist author's body.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistItem {
    /// The item id, stable across edits.
    pub id: u32,
    /// The text.
    pub text: String,
}

/// A checklist's body (ADR-0092 section 5).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistBody {
    /// The title.
    pub title: String,
    /// Others may add items.
    pub others_add: bool,
    /// Others may mark items done.
    pub others_mark: bool,
    /// The author's items.
    pub items: Vec<ChecklistItem>,
}

/// A checklist with its author signature (`None` only in a channel post).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SignedChecklist {
    /// The checklist.
    pub body: ChecklistBody,
    /// 64-byte Ed25519 author signature.
    pub author_sig: Option<Vec<u8>>,
}

impl From<ChecklistBody> for checklist::ChecklistBody {
    fn from(b: ChecklistBody) -> Self {
        Self {
            title: b.title,
            others_add: b.others_add,
            others_mark: b.others_mark,
            items: b
                .items
                .into_iter()
                .map(|i| checklist::ChecklistItem {
                    id: i.id,
                    text: i.text,
                })
                .collect(),
        }
    }
}

impl From<checklist::ChecklistBody> for ChecklistBody {
    fn from(b: checklist::ChecklistBody) -> Self {
        Self {
            title: b.title,
            others_add: b.others_add,
            others_mark: b.others_mark,
            items: b
                .items
                .into_iter()
                .map(|i| ChecklistItem {
                    id: i.id,
                    text: i.text,
                })
                .collect(),
        }
    }
}

impl TryFrom<SignedChecklist> for inner::SignedChecklist {
    type Error = FfiError;
    fn try_from(c: SignedChecklist) -> Result<Self, FfiError> {
        Ok(Self {
            body: c.body.into(),
            author_sig: opt_sig(c.author_sig)?,
        })
    }
}

impl From<inner::SignedChecklist> for SignedChecklist {
    fn from(c: inner::SignedChecklist) -> Self {
        Self {
            body: c.body.into(),
            author_sig: c.author_sig.map(|s| s.to_vec()),
        }
    }
}

/// Which checklist item a tick names.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecklistItemRef {
    /// An item of the author's body, by id.
    Author {
        /// The item id.
        id: u32,
    },
    /// An item someone added, by the DS `messageId` of its add row.
    Added {
        /// The add row's DS `messageId`.
        message_id: i64,
    },
}

impl From<ChecklistItemRef> for inner::ChecklistItemRef {
    fn from(r: ChecklistItemRef) -> Self {
        match r {
            ChecklistItemRef::Author { id } => Self::Author(id),
            ChecklistItemRef::Added { message_id } => Self::Added(message_id),
        }
    }
}

impl From<inner::ChecklistItemRef> for ChecklistItemRef {
    fn from(r: inner::ChecklistItemRef) -> Self {
        match r {
            inner::ChecklistItemRef::Author(id) => Self::Author { id },
            inner::ChecklistItemRef::Added(message_id) => Self::Added { message_id },
        }
    }
}

/// Sign a checklist for `group_id` with the author's account seed.
///
/// # Errors
///
/// [`FfiError::Content`] for a body that breaks a bound or a wrong length.
#[uniffi::export]
pub fn sign_checklist(
    body: ChecklistBody,
    group_id: Vec<u8>,
    author_seed: Vec<u8>,
) -> Result<SignedChecklist, FfiError> {
    let group = bytes::<32>(&group_id, "groupId")?;
    let seed = bytes::<32>(&author_seed, "authorSeed")?;
    inner::SignedChecklist::sign(body.into(), &group, &seed)
        .map(Into::into)
        .map_err(content_err)
}

/// Check a checklist's author signature (C-68 key).
///
/// # Errors
///
/// [`FfiError::Content`] `signatureMissing` / `signatureInvalid`, or a bound.
#[uniffi::export]
pub fn verify_checklist(
    checklist: SignedChecklist,
    group_id: Vec<u8>,
    author_key: Vec<u8>,
) -> Result<(), FfiError> {
    let group = bytes::<32>(&group_id, "groupId")?;
    let key = bytes::<32>(&author_key, "authorKey")?;
    inner::SignedChecklist::try_from(checklist)?
        .verify(&group, &key)
        .map_err(content_err)
}

/// An edit of the checklist by its author, as stored at receipt.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistEditRecord {
    /// DS `messageId` of the edit row.
    pub message_id: i64,
    /// The editor's account key (C-68).
    pub editor: Vec<u8>,
    /// The replacement body.
    pub body: ChecklistBody,
}

/// A tick as stored at receipt.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistTickRecord {
    /// DS `messageId` of the tick row.
    pub message_id: i64,
    /// The ticker's account key (C-68).
    pub ticker: Vec<u8>,
    /// A channel publisher (may tick whatever the flags say).
    pub privileged: bool,
    /// Which item.
    pub item: ChecklistItemRef,
    /// Done or not.
    pub done: bool,
}

/// An added item as stored at receipt.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistAddRecord {
    /// DS `messageId` of the add row (the item's id).
    pub message_id: i64,
    /// The adder's account key (C-68).
    pub adder: Vec<u8>,
    /// A channel publisher.
    pub privileged: bool,
    /// The item text.
    pub text: String,
}

/// A close as stored at receipt.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistCloseRecord {
    /// DS `messageId` of the close row.
    pub message_id: i64,
    /// The closer's account key (C-68).
    pub closer: Vec<u8>,
}

/// One item of a folded checklist.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistStateItem {
    /// Which item.
    pub item: ChecklistItemRef,
    /// The text.
    pub text: String,
    /// Who added it, for an added item.
    pub added_by: Option<Vec<u8>>,
    /// Who marked it done, when done.
    pub done_by: Option<Vec<u8>>,
}

/// A checklist's folded state.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ChecklistState {
    /// The title (the latest edit's).
    pub title: String,
    /// Others may add items.
    pub others_add: bool,
    /// Others may mark items.
    pub others_mark: bool,
    /// The items in display order.
    pub items: Vec<ChecklistStateItem>,
    /// DS `messageId` of the author's close, if closed.
    pub closed_by: Option<i64>,
}

impl From<checklist::ChecklistState> for ChecklistState {
    fn from(s: checklist::ChecklistState) -> Self {
        Self {
            title: s.title,
            others_add: s.others_add,
            others_mark: s.others_mark,
            items: s
                .items
                .into_iter()
                .map(|i| ChecklistStateItem {
                    item: i.item.into(),
                    text: i.text,
                    added_by: i.added_by.map(|k| k.to_vec()),
                    done_by: i.done_by.map(|k| k.to_vec()),
                })
                .collect(),
            closed_by: s.closed_by,
        }
    }
}

impl TryFrom<ChecklistState> for checklist::ChecklistState {
    type Error = FfiError;
    fn try_from(s: ChecklistState) -> Result<Self, FfiError> {
        Ok(Self {
            title: s.title,
            others_add: s.others_add,
            others_mark: s.others_mark,
            items: s
                .items
                .into_iter()
                .map(|i| {
                    Ok(checklist::ChecklistStateItem {
                        item: i.item.into(),
                        text: i.text,
                        added_by: i.added_by.map(|k| bytes::<32>(&k, "addedBy")).transpose()?,
                        done_by: i.done_by.map(|k| bytes::<32>(&k, "doneBy")).transpose()?,
                    })
                })
                .collect::<Result<_, FfiError>>()?,
            closed_by: s.closed_by,
        })
    }
}

/// Fold a checklist (ADR-0092 section 5): author edits, adds, ticks and the
/// author's close, in DS `messageId` order. Independent of input order.
///
/// # Errors
///
/// [`FfiError::Content`] for a wrong-length key.
#[uniffi::export]
pub fn fold_checklist(
    body: ChecklistBody,
    author: Vec<u8>,
    checklist_message_id: i64,
    edits: Vec<ChecklistEditRecord>,
    adds: Vec<ChecklistAddRecord>,
    ticks: Vec<ChecklistTickRecord>,
    closes: Vec<ChecklistCloseRecord>,
) -> Result<ChecklistState, FfiError> {
    let author = bytes::<32>(&author, "author")?;
    let edits = edits
        .into_iter()
        .map(|e| {
            Ok(checklist::ChecklistEditRecord {
                message_id: e.message_id,
                editor: bytes::<32>(&e.editor, "editor")?,
                body: e.body.into(),
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let adds = adds
        .into_iter()
        .map(|a| {
            Ok(checklist::ChecklistAddRecord {
                message_id: a.message_id,
                adder: bytes::<32>(&a.adder, "adder")?,
                privileged: a.privileged,
                text: a.text,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let ticks = ticks
        .into_iter()
        .map(|t| {
            Ok(checklist::ChecklistTickRecord {
                message_id: t.message_id,
                ticker: bytes::<32>(&t.ticker, "ticker")?,
                privileged: t.privileged,
                item: t.item.into(),
                done: t.done,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let closes = closes
        .into_iter()
        .map(|c| {
            Ok(checklist::ChecklistCloseRecord {
                message_id: c.message_id,
                closer: bytes::<32>(&c.closer, "closer")?,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(checklist::fold(
        &body.into(),
        &author,
        checklist_message_id,
        &edits,
        &adds,
        &ticks,
        &closes,
    )
    .into())
}

// ============================================================================
// Location, contact card
// ============================================================================

/// A shared location (ADR-0092 section 6), coordinates in 1e-7 degrees.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SharedLocation {
    /// Latitude, 1e-7 degrees.
    pub lat_e7: i32,
    /// Longitude, 1e-7 degrees.
    pub lon_e7: i32,
    /// Accuracy radius, metres.
    pub accuracy_m: u32,
    /// When the fix was taken, unix milliseconds.
    pub fix_at_ms: i64,
    /// A place name, or `None`.
    pub name: Option<String>,
}

impl From<SharedLocation> for inner::Location {
    fn from(l: SharedLocation) -> Self {
        Self {
            lat_e7: l.lat_e7,
            lon_e7: l.lon_e7,
            accuracy_m: l.accuracy_m,
            fix_at_ms: l.fix_at_ms,
            name: l.name,
        }
    }
}

impl From<inner::Location> for SharedLocation {
    fn from(l: inner::Location) -> Self {
        Self {
            lat_e7: l.lat_e7,
            lon_e7: l.lon_e7,
            accuracy_m: l.accuracy_m,
            fix_at_ms: l.fix_at_ms,
            name: l.name,
        }
    }
}

/// A contact card (ADR-0092 section 7). Carries no reach grant.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SharedContactCard {
    /// The display name as the sender knows it.
    pub name: String,
    /// The canonical username.
    pub username: String,
    /// The 32-byte account signature key.
    pub account_key: Vec<u8>,
}

impl TryFrom<SharedContactCard> for inner::ContactCard {
    type Error = FfiError;
    fn try_from(c: SharedContactCard) -> Result<Self, FfiError> {
        Ok(Self {
            name: c.name,
            username: c.username,
            account_key: bytes::<32>(&c.account_key, "accountKey")?,
        })
    }
}

impl From<inner::ContactCard> for SharedContactCard {
    fn from(c: inner::ContactCard) -> Self {
        Self {
            name: c.name,
            username: c.username,
            account_key: c.account_key.to_vec(),
        }
    }
}

// ============================================================================
// Article
// ============================================================================

/// A run of inline article text.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum ArticleRun {
    /// Plain text.
    Text {
        /// Bold.
        bold: bool,
        /// Italic.
        italic: bool,
        /// The text.
        text: String,
    },
    /// A link (`https://` only).
    Link {
        /// The URL.
        url: String,
        /// Bold.
        bold: bool,
        /// Italic.
        italic: bool,
        /// The visible text.
        text: String,
    },
}

/// Heading level.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArticleHeadingLevel {
    /// Heading.
    H1,
    /// Subheading.
    H2,
}

/// One list item.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ArticleListItem {
    /// 0 or 1.
    pub depth: u8,
    /// The runs.
    pub runs: Vec<ArticleRun>,
}

/// An article block.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum ArticleBlock {
    /// A paragraph.
    Paragraph {
        /// The runs.
        runs: Vec<ArticleRun>,
    },
    /// A heading.
    Heading {
        /// Level.
        level: ArticleHeadingLevel,
        /// The runs.
        runs: Vec<ArticleRun>,
    },
    /// A quote.
    Quote {
        /// The runs.
        runs: Vec<ArticleRun>,
    },
    /// A list. Named `ListBlock`, not `List`: in Kotlin a data variant is a
    /// nested class, and `ArticleBlock.List` would shadow
    /// `kotlin.collections.List` inside the sealed class.
    ListBlock {
        /// Numbered.
        ordered: bool,
        /// The items.
        items: Vec<ArticleListItem>,
    },
    /// An image: an attachment of the same message.
    Image {
        /// Index into the message's attachments.
        attachment_index: u8,
        /// Caption runs, possibly none.
        caption: Vec<ArticleRun>,
    },
    /// A divider.
    Divider,
}

/// An article (ADR-0092 section 8).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct Article {
    /// The title.
    pub title: String,
    /// The blocks.
    pub blocks: Vec<ArticleBlock>,
}

fn run_to_core(r: ArticleRun) -> article::Run {
    match r {
        ArticleRun::Text { bold, italic, text } => article::Run::Text {
            marks: article::Marks { bold, italic },
            text,
        },
        ArticleRun::Link {
            url,
            bold,
            italic,
            text,
        } => article::Run::Link {
            url,
            marks: article::Marks { bold, italic },
            text,
        },
    }
}

fn run_from_core(r: article::Run) -> ArticleRun {
    match r {
        article::Run::Text { marks, text } => ArticleRun::Text {
            bold: marks.bold,
            italic: marks.italic,
            text,
        },
        article::Run::Link { url, marks, text } => ArticleRun::Link {
            url,
            bold: marks.bold,
            italic: marks.italic,
            text,
        },
    }
}

fn runs_to_core(runs: Vec<ArticleRun>) -> Vec<article::Run> {
    runs.into_iter().map(run_to_core).collect()
}

fn runs_from_core(runs: Vec<article::Run>) -> Vec<ArticleRun> {
    runs.into_iter().map(run_from_core).collect()
}

impl From<Article> for inner::Article {
    fn from(a: Article) -> Self {
        Self {
            title: a.title,
            blocks: a
                .blocks
                .into_iter()
                .map(|b| match b {
                    ArticleBlock::Paragraph { runs } => {
                        article::Block::Paragraph(runs_to_core(runs))
                    },
                    ArticleBlock::Heading { level, runs } => article::Block::Heading {
                        level: match level {
                            ArticleHeadingLevel::H1 => article::HeadingLevel::H1,
                            ArticleHeadingLevel::H2 => article::HeadingLevel::H2,
                        },
                        runs: runs_to_core(runs),
                    },
                    ArticleBlock::Quote { runs } => article::Block::Quote(runs_to_core(runs)),
                    ArticleBlock::ListBlock { ordered, items } => article::Block::List {
                        ordered,
                        items: items
                            .into_iter()
                            .map(|i| article::ListItem {
                                depth: i.depth,
                                runs: runs_to_core(i.runs),
                            })
                            .collect(),
                    },
                    ArticleBlock::Image {
                        attachment_index,
                        caption,
                    } => article::Block::Image {
                        attachment_index,
                        caption: runs_to_core(caption),
                    },
                    ArticleBlock::Divider => article::Block::Divider,
                })
                .collect(),
        }
    }
}

impl From<inner::Article> for Article {
    fn from(a: inner::Article) -> Self {
        Self {
            title: a.title,
            blocks: a
                .blocks
                .into_iter()
                .map(|b| match b {
                    article::Block::Paragraph(runs) => ArticleBlock::Paragraph {
                        runs: runs_from_core(runs),
                    },
                    article::Block::Heading { level, runs } => ArticleBlock::Heading {
                        level: match level {
                            article::HeadingLevel::H1 => ArticleHeadingLevel::H1,
                            article::HeadingLevel::H2 => ArticleHeadingLevel::H2,
                        },
                        runs: runs_from_core(runs),
                    },
                    article::Block::Quote(runs) => ArticleBlock::Quote {
                        runs: runs_from_core(runs),
                    },
                    article::Block::List { ordered, items } => ArticleBlock::ListBlock {
                        ordered,
                        items: items
                            .into_iter()
                            .map(|i| ArticleListItem {
                                depth: i.depth,
                                runs: runs_from_core(i.runs),
                            })
                            .collect(),
                    },
                    article::Block::Image {
                        attachment_index,
                        caption,
                    } => ArticleBlock::Image {
                        attachment_index,
                        caption: runs_from_core(caption),
                    },
                    article::Block::Divider => ArticleBlock::Divider,
                })
                .collect(),
        }
    }
}

/// The bubble excerpt of an article: its text, first 200 characters.
#[uniffi::export]
#[must_use]
pub fn article_excerpt(article: Article) -> String {
    inner::Article::from(article).excerpt()
}

/// Check an article's bounds and canonical rules (not its images, which
/// need the attachments: [`compose_content_envelope`] checks them).
///
/// # Errors
///
/// [`FfiError::Content`] with the first refusal.
#[uniffi::export]
pub fn validate_article(article: Article) -> Result<(), FfiError> {
    inner::Article::from(article)
        .validate()
        .map_err(content_err)
}

/// Whether `url` is a link an article may carry (`https://`, one whole link).
#[uniffi::export]
#[must_use]
pub fn article_url_valid(url: String) -> bool {
    article::article_url_valid(&url)
}

// ============================================================================
// Content
// ============================================================================

/// One ADR-0092 content payload.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum MessageContent {
    /// A poll.
    Poll {
        /// The poll.
        poll: SignedPoll,
    },
    /// A checklist.
    Checklist {
        /// The checklist.
        checklist: SignedChecklist,
    },
    /// A location.
    Location {
        /// The location.
        location: SharedLocation,
    },
    /// A contact card.
    ContactCard {
        /// The card.
        card: SharedContactCard,
    },
    /// An article; its image blocks point at the message's attachments.
    Article {
        /// The article.
        article: Article,
    },
}

impl TryFrom<MessageContent> for inner::Content {
    type Error = FfiError;
    fn try_from(c: MessageContent) -> Result<Self, FfiError> {
        Ok(match c {
            MessageContent::Poll { poll } => Self::Poll(poll.try_into()?),
            MessageContent::Checklist { checklist } => Self::Checklist(checklist.try_into()?),
            MessageContent::Location { location } => Self::Location(location.into()),
            MessageContent::ContactCard { card } => Self::ContactCard(card.try_into()?),
            MessageContent::Article { article } => Self::Article(article.into()),
        })
    }
}

impl From<inner::Content> for MessageContent {
    fn from(c: inner::Content) -> Self {
        match c {
            inner::Content::Poll(poll) => Self::Poll { poll: poll.into() },
            inner::Content::Checklist(c) => Self::Checklist {
                checklist: c.into(),
            },
            inner::Content::Location(l) => Self::Location { location: l.into() },
            inner::Content::ContactCard(c) => Self::ContactCard { card: c.into() },
            inner::Content::Article(a) => Self::Article { article: a.into() },
        }
    }
}

/// The reply and forward parts a content message may carry; every field
/// defaults to `None`.
#[derive(uniffi::Record, Default)]
pub struct ContentEnvelopeParts {
    /// The DS `messageId` this message replies to.
    #[uniffi(default = None)]
    pub reply_to: Option<i64>,
    /// The quoted author's display name, a snapshot.
    #[uniffi(default = None)]
    pub reply_author: Option<String>,
    /// An excerpt of the quoted message, a snapshot.
    #[uniffi(default = None)]
    pub reply_excerpt: Option<String>,
    /// On a forward, the original author's display name.
    #[uniffi(default = None)]
    pub forwarded_from: Option<String>,
    /// On a forward, base64 of the origin group id (with the message id).
    #[uniffi(default = None)]
    pub forward_origin_group_b64: Option<String>,
    /// On a forward, the original's DS `messageId`.
    #[uniffi(default = None)]
    pub forward_origin_message_id: Option<i64>,
}

/// Build an ADR-0092 content message: the content, attachments (an article
/// only: its images), and an optional reply / forward. Refuses exactly what
/// a receiver refuses.
///
/// # Errors
///
/// [`FfiError::Content`] for a payload that breaks a bound, an article image
/// that is not an image attachment, or attachments beside a payload that
/// takes none; [`FfiError::Attachment`] for a wrong-length attachment field.
#[uniffi::export]
pub fn compose_content_envelope(
    content: MessageContent,
    attachments: Vec<AttachmentRef>,
    parts: ContentEnvelopeParts,
) -> Result<Vec<u8>, FfiError> {
    let content = inner::Content::try_from(content)?;
    let reply = parts.reply_to.map(|target| inner::Reply {
        target,
        author: parts.reply_author,
        excerpt: parts.reply_excerpt,
    });
    let origin = match (
        parts.forward_origin_group_b64,
        parts.forward_origin_message_id,
    ) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ForwardOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    let forward = (parts.forwarded_from.is_some() || origin.is_some()).then_some(inner::Forward {
        author: parts.forwarded_from,
        origin,
    });
    inner::compose_content(
        &content,
        refs_from_ffi(attachments)?,
        reply.as_ref(),
        forward.as_ref(),
    )
    .map_err(Into::into)
}

/// Build a non-anonymous VOTE on the poll `target` (choices strictly
/// ascending; empty retracts).
///
/// # Errors
///
/// [`FfiError::Content`] `choices.outOfRange`.
#[uniffi::export]
pub fn compose_poll_vote_envelope(target: i64, choices: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    inner::compose_poll_vote(target, &choices).map_err(Into::into)
}

/// Build a CLOSE of the poll or checklist `target`. A channel poll's close
/// carries its frozen tally (`encodeChannelPollTally`); `None` elsewhere.
///
/// # Errors
///
/// [`FfiError::Content`] `tally.outOfRange` for an empty or over-long tally.
#[uniffi::export(default(tally = None))]
pub fn compose_poll_close_envelope(
    target: i64,
    tally: Option<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    inner::compose_poll_close(target, tally.as_deref()).map_err(Into::into)
}

/// Build a TICK of `item` in the checklist `target`.
///
/// # Errors
///
/// [`FfiError::Attachment`] on serialisation failure.
#[uniffi::export]
pub fn compose_checklist_tick_envelope(
    target: i64,
    item: ChecklistItemRef,
    done: bool,
) -> Result<Vec<u8>, FfiError> {
    inner::compose_checklist_tick(target, item.into(), done).map_err(Into::into)
}

/// Build an ADD-ITEM to the checklist `target`.
///
/// # Errors
///
/// [`FfiError::Content`] when `text` breaks the item rule.
#[uniffi::export]
pub fn compose_checklist_add_envelope(target: i64, text: String) -> Result<Vec<u8>, FfiError> {
    inner::compose_checklist_add(target, &text).map_err(Into::into)
}

/// Build an EDIT of the checklist `target` (the author's, signed).
///
/// # Errors
///
/// [`FfiError::Content`] for a body that breaks a bound.
#[uniffi::export]
pub fn compose_checklist_edit_envelope(
    target: i64,
    checklist: SignedChecklist,
) -> Result<Vec<u8>, FfiError> {
    inner::compose_checklist_edit(target, &checklist.try_into()?).map_err(Into::into)
}

/// Build an EDIT of the article `target`, replacing it whole.
///
/// # Errors
///
/// [`FfiError::Content`] for an article that breaks a bound or an image
/// block that does not point at an image attachment.
#[uniffi::export]
pub fn compose_article_edit_envelope(
    target: i64,
    article: Article,
    attachments: Vec<AttachmentRef>,
) -> Result<Vec<u8>, FfiError> {
    inner::compose_article_edit(target, &article.into(), refs_from_ffi(attachments)?)
        .map_err(Into::into)
}

// ============================================================================
// State snapshot (history bundle, vault)
// ============================================================================

/// How a saved poll stood.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollClosedState {
    /// Open.
    Open,
    /// Closed by its automatic close time.
    ByTime,
    /// Closed by the close op with this DS `messageId`.
    ByOp {
        /// The close row's DS `messageId`.
        message_id: i64,
    },
}

/// An item someone added to a saved checklist.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct AddedChecklistItem {
    /// DS `messageId` of the add row.
    pub message_id: i64,
    /// The text.
    pub text: String,
}

/// A done item of a saved checklist.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct DoneChecklistItem {
    /// Which item.
    pub item: ChecklistItemRef,
    /// Who marked it (account key).
    pub by: Vec<u8>,
}

/// A poll's or checklist's folded state as the saving device saw it.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum ContentState {
    /// A poll.
    Poll {
        /// Votes per option.
        counts: Vec<u32>,
        /// Voters with a counted vote.
        voters: u32,
        /// Open or how it closed.
        closed: PollClosedState,
    },
    /// A checklist.
    Checklist {
        /// DS `messageId` of the author's close, if closed.
        closed_by: Option<i64>,
        /// Added items, ascending by id.
        added: Vec<AddedChecklistItem>,
        /// Done items, ascending by item.
        done: Vec<DoneChecklistItem>,
    },
}

impl TryFrom<ContentState> for cs::ContentState {
    type Error = FfiError;
    fn try_from(s: ContentState) -> Result<Self, FfiError> {
        Ok(match s {
            ContentState::Poll {
                counts,
                voters,
                closed,
            } => Self::Poll(cs::PollState {
                counts,
                voters,
                closed: match closed {
                    PollClosedState::Open => cs::PollClosed::Open,
                    PollClosedState::ByTime => cs::PollClosed::ByTime,
                    PollClosedState::ByOp { message_id } => cs::PollClosed::ByOp(message_id),
                },
            }),
            ContentState::Checklist {
                closed_by,
                added,
                done,
            } => Self::Checklist(cs::ChecklistSnapshot {
                closed_by,
                added: added
                    .into_iter()
                    .map(|a| cs::AddedItemSnapshot {
                        message_id: a.message_id,
                        text: a.text,
                    })
                    .collect(),
                done: done
                    .into_iter()
                    .map(|d| Ok((d.item.into(), bytes::<32>(&d.by, "doneBy")?)))
                    .collect::<Result<_, FfiError>>()?,
            }),
        })
    }
}

impl From<cs::ContentState> for ContentState {
    fn from(s: cs::ContentState) -> Self {
        match s {
            cs::ContentState::Poll(p) => Self::Poll {
                counts: p.counts,
                voters: p.voters,
                closed: match p.closed {
                    cs::PollClosed::Open => PollClosedState::Open,
                    cs::PollClosed::ByTime => PollClosedState::ByTime,
                    cs::PollClosed::ByOp(message_id) => PollClosedState::ByOp { message_id },
                },
            },
            cs::ContentState::Checklist(c) => Self::Checklist {
                closed_by: c.closed_by,
                added: c
                    .added
                    .into_iter()
                    .map(|a| AddedChecklistItem {
                        message_id: a.message_id,
                        text: a.text,
                    })
                    .collect(),
                done: c
                    .done
                    .into_iter()
                    .map(|(item, by)| DoneChecklistItem {
                        item: item.into(),
                        by: by.to_vec(),
                    })
                    .collect(),
            },
        }
    }
}

/// The saved state of a non-anonymous poll's tally at `now_ms`.
///
/// # Errors
///
/// [`FfiError::Content`] for a wrong-length voter key.
#[uniffi::export]
pub fn poll_state_from_tally(tally: PollTally, now_ms: i64) -> Result<ContentState, FfiError> {
    let tally = poll::PollTally::try_from(tally)?;
    Ok(cs::ContentState::Poll(cs::PollState::from_tally(&tally, now_ms)).into())
}

/// The saved state of an anonymous poll's ballot tally (counts and voters
/// from `foldPollBallots`).
#[uniffi::export]
#[must_use]
pub fn poll_state_from_counts(
    counts: Vec<u32>,
    voters: u32,
    closed: PollClosedState,
) -> ContentState {
    ContentState::Poll {
        counts,
        voters,
        closed,
    }
}

/// The saved state of a folded checklist.
///
/// # Errors
///
/// [`FfiError::Content`] for a wrong-length key.
#[uniffi::export]
pub fn checklist_state_snapshot(state: ChecklistState) -> Result<ContentState, FfiError> {
    let state = checklist::ChecklistState::try_from(state)?;
    Ok(cs::ContentState::Checklist(cs::ChecklistSnapshot::from_state(&state)).into())
}

/// A content message as a history bundle or the vault carries it: the
/// content (the latest edit) and, for a poll or checklist, its saved state.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContentMessage {
    /// The content.
    pub content: MessageContent,
    /// The saved state, or `None`.
    pub state: Option<ContentState>,
}

/// Encode a [`ContentMessage`] to the `(kind, payload, state)` triple both
/// record codecs carry. A state that does not belong to the content is
/// refused.
pub(crate) fn content_message_to_core(
    m: ContentMessage,
) -> Result<(u8, Vec<u8>, Vec<u8>), FfiError> {
    let content = inner::Content::try_from(m.content)?;
    let payload = content.to_payload().map_err(content_err)?;
    let state = match m.state {
        None => Vec::new(),
        Some(s) => {
            let s = cs::ContentState::try_from(s)?;
            if !s.matches(&content) {
                return Err(FfiError::Content(
                    "state.mismatch: the state does not belong to the content".into(),
                ));
            }
            s.encode().map_err(content_err)?
        },
    };
    Ok((content.kind_byte(), payload, state))
}

/// Decode the `(kind, payload, state)` triple with the envelope's own
/// decoders. A payload that does not decode gives `None` (the row shows as
/// unsupported); a state that does not decode or does not belong to the
/// content is dropped.
pub(crate) fn content_message_from_core(
    kind: u8,
    payload: &[u8],
    state: &[u8],
    attachment_mimes: &[Option<String>],
) -> Option<ContentMessage> {
    let refs: Vec<inner::AttachmentRef> = attachment_mimes
        .iter()
        .map(|mime| inner::AttachmentRef {
            blob_id: String::new(),
            key: String::new(),
            nonce: String::new(),
            mime: mime.clone(),
            size: 0,
            name: None,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
            title: None,
            artist: None,
            inline_thumb: None,
        })
        .collect();
    let content = inner::Content::from_payload(kind, payload, &refs).ok()?;
    let state = if state.is_empty() {
        None
    } else {
        cs::ContentState::decode(state)
            .ok()
            .filter(|s| s.matches(&content))
            .map(Into::into)
    };
    Some(ContentMessage {
        content: content.into(),
        state,
    })
}

// ============================================================================
// Limits
// ============================================================================

/// The editor limits of ADR-0092, from the core constants.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContentLimits {
    /// Longest poll question, UTF-8 bytes.
    pub max_poll_question_bytes: u32,
    /// Fewest poll options.
    pub min_poll_options: u32,
    /// Most poll options.
    pub max_poll_options: u32,
    /// Longest poll option, bytes.
    pub max_poll_option_bytes: u32,
    /// Longest quiz explanation, bytes.
    pub max_poll_explanation_bytes: u32,
    /// Longest checklist title, bytes.
    pub max_checklist_title_bytes: u32,
    /// Most checklist items (author's and added).
    pub max_checklist_items: u32,
    /// Longest checklist item, bytes.
    pub max_checklist_item_bytes: u32,
    /// Longest location name, bytes.
    pub max_location_name_bytes: u32,
    /// Longest article title, bytes.
    pub max_article_title_bytes: u32,
    /// Most article text, bytes.
    pub max_article_text_bytes: u32,
    /// Most article blocks.
    pub max_article_blocks: u32,
    /// Most article images.
    pub max_article_images: u32,
    /// Longest article link, bytes.
    pub max_article_url_bytes: u32,
    /// Longest audio title or artist, bytes.
    pub max_audio_tag_bytes: u32,
    /// Longest audio attachment, milliseconds.
    pub max_audio_duration_ms: u32,
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The ADR-0092 editor limits.
#[uniffi::export]
#[must_use]
pub fn content_limits() -> ContentLimits {
    ContentLimits {
        max_poll_question_bytes: u32_of(poll::MAX_POLL_QUESTION_BYTES),
        min_poll_options: u32_of(poll::MIN_POLL_OPTIONS),
        max_poll_options: u32_of(poll::MAX_POLL_OPTIONS),
        max_poll_option_bytes: u32_of(poll::MAX_POLL_OPTION_BYTES),
        max_poll_explanation_bytes: u32_of(poll::MAX_POLL_EXPLANATION_BYTES),
        max_checklist_title_bytes: u32_of(checklist::MAX_CHECKLIST_TITLE_BYTES),
        max_checklist_items: u32_of(checklist::MAX_CHECKLIST_ITEMS),
        max_checklist_item_bytes: u32_of(checklist::MAX_CHECKLIST_ITEM_BYTES),
        max_location_name_bytes: u32_of(inner::location::MAX_LOCATION_NAME_BYTES),
        max_article_title_bytes: u32_of(article::MAX_ARTICLE_TITLE_BYTES),
        max_article_text_bytes: u32_of(article::MAX_ARTICLE_TEXT_BYTES),
        max_article_blocks: u32_of(article::MAX_ARTICLE_BLOCKS),
        max_article_images: u32_of(article::MAX_ARTICLE_IMAGES),
        max_article_url_bytes: u32_of(article::MAX_ARTICLE_URL_BYTES),
        max_audio_tag_bytes: u32_of(inner::MAX_AUDIO_TAG_BYTES),
        max_audio_duration_ms: inner::MAX_AUDIO_DURATION_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::{parse_body_envelope, MessageBodyKind};

    fn body() -> PollBody {
        PollBody {
            id: vec![3; 16],
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Soup".into()],
            multiple: false,
            quiz: None,
            close_at_ms: None,
            anonymous: None,
        }
    }

    #[test]
    fn a_signed_poll_round_trips_through_the_envelope_and_verifies() {
        let seed = vec![7; 32];
        let author = lakko_crypto::signing::SigningKey::from_seed(&[7; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let poll = sign_poll(body(), vec![1; 32], seed).unwrap();
        let bytes = compose_content_envelope(
            MessageContent::Poll { poll: poll.clone() },
            Vec::new(),
            ContentEnvelopeParts::default(),
        )
        .unwrap();
        let parsed = parse_body_envelope(bytes);
        assert_eq!(parsed.kind, MessageBodyKind::Poll);
        let Some(MessageContent::Poll { poll: got }) = parsed.content else {
            panic!("expected a poll");
        };
        assert_eq!(got, poll);
        verify_poll(got.clone(), vec![1; 32], author.clone()).unwrap();
        let Err(FfiError::Content(m)) = verify_poll(got, vec![2; 32], author) else {
            panic!("expected a content error");
        };
        assert!(m.starts_with("signatureInvalid: "));
    }

    #[test]
    fn ops_parse_to_their_kinds() {
        let v = parse_body_envelope(compose_poll_vote_envelope(9, vec![0]).unwrap());
        assert_eq!(v.kind, MessageBodyKind::PollVote);
        assert_eq!(v.poll_choices, Some(vec![0]));
        let t = parse_body_envelope(
            compose_checklist_tick_envelope(9, ChecklistItemRef::Added { message_id: 12 }, true)
                .unwrap(),
        );
        assert_eq!(t.kind, MessageBodyKind::ChecklistTick);
        assert_eq!(
            t.checklist_item,
            Some(ChecklistItemRef::Added { message_id: 12 })
        );
        assert_eq!(t.checklist_done, Some(true));
        assert!(compose_poll_vote_envelope(9, vec![1, 1]).is_err());
    }

    #[test]
    fn a_content_message_round_trips_and_a_foreign_state_is_refused() {
        let poll = sign_poll(body(), vec![1; 32], vec![7; 32]).unwrap();
        let state = ContentState::Poll {
            counts: vec![1, 0],
            voters: 1,
            closed: PollClosedState::Open,
        };
        let m = ContentMessage {
            content: MessageContent::Poll { poll },
            state: Some(state),
        };
        let (kind, payload, st) = content_message_to_core(m.clone()).unwrap();
        assert_eq!(
            content_message_from_core(kind, &payload, &st, &[]),
            Some(m.clone())
        );
        let mut bad = m;
        bad.state = Some(ContentState::Poll {
            counts: vec![1, 0, 0],
            voters: 1,
            closed: PollClosedState::Open,
        });
        assert!(content_message_to_core(bad).is_err());
    }

    #[test]
    fn limits_come_from_the_core() {
        let l = content_limits();
        assert_eq!(l.max_poll_options, 10);
        assert_eq!(l.max_checklist_items, 30);
    }
}
