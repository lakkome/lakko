// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The committer's side: one held open item to a `lakko_contacts::OpenItem`
//! (ADR-0090 section 3.9).
//!
//! Which items are open is the caller's reading (a poll that is not closed
//! by a close op or its time, a checklist with no close); this module turns
//! one into what the hand-over carries:
//!
//! * a poll: its author signature and body, and either its ring (anonymous)
//!   or each voter's latest vote (non-anonymous; a voter whose latest vote
//!   retracted is left out, which the newcomer's fold reads the same way);
//! * a checklist: its current signed body (after the author's edits), the
//!   added items the fold kept, and one tick per done item.

use std::collections::BTreeMap;

use lakko_attachments::checklist::{ChecklistItemRef, ChecklistState, ChecklistTickRecord};
use lakko_attachments::poll::PollVoteRecord;
use lakko_attachments::{ContentError, SignedChecklist, SignedPoll};
use lakko_ballot::Ring;
use lakko_contacts::open_items::{
    HandoverAdded, HandoverItemRef, HandoverTick, HandoverVote, OpenItem, OpenItemContent,
    OpenItemHeader,
};
use thiserror::Error;

/// Why an item could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
    /// The body carries no author signature (a channel item is never handed
    /// over).
    #[error("item has no author signature")]
    SignatureMissing,
    /// The body does not encode.
    #[error("item body does not encode: {0}")]
    Body(ContentError),
    /// An anonymous poll without a ring, or a ring for a non-anonymous poll.
    #[error("ring given for the wrong kind of poll")]
    RingMismatch,
    /// The folded checklist state does not belong to the given body.
    #[error("checklist state does not match its body")]
    StateMismatch,
}

impl BuildError {
    /// Stable reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SignatureMissing => "signatureMissing",
            Self::Body(_) => "body",
            Self::RingMismatch => "ringMismatch",
            Self::StateMismatch => "stateMismatch",
        }
    }
}

fn header(
    poll_ref: i64,
    author: &[u8; 32],
    sig: Option<[u8; 64]>,
    body: Vec<u8>,
) -> Result<OpenItemHeader, BuildError> {
    Ok(OpenItemHeader {
        poll_ref,
        author_account_key: *author,
        author_sig: sig.ok_or(BuildError::SignatureMissing)?,
        body,
    })
}

/// A poll the committer holds as an open item. `ring` is the poll's ring
/// for an anonymous poll and `None` otherwise; `votes` are the vote records
/// the committer stored at receipt (ignored for an anonymous poll). Only
/// votes after the poll row with choices valid for the poll count; each
/// voter's latest such vote in DS order is carried, a retract carries none.
///
/// # Errors
///
/// [`BuildError::SignatureMissing`], [`BuildError::Body`],
/// [`BuildError::RingMismatch`].
pub fn poll_open_item(
    poll_ref: i64,
    poll: &SignedPoll,
    author: &[u8; 32],
    ring: Option<&Ring>,
    votes: &[PollVoteRecord],
) -> Result<OpenItem, BuildError> {
    let body = poll.body.encode().map_err(BuildError::Body)?;
    let header = header(poll_ref, author, poll.author_sig, body)?;
    let content = match (&poll.body.anonymous, ring) {
        (Some(_), Some(ring)) => OpenItemContent::AnonymousPoll {
            keys: ring.keys().to_vec(),
            lengths: (0..ring.version_count())
                .filter_map(|v| ring.version_len(v))
                .map(|l| u32::try_from(l).unwrap_or(u32::MAX))
                .collect(),
        },
        (None, None) => {
            let mut latest: BTreeMap<[u8; 32], &PollVoteRecord> = BTreeMap::new();
            for v in votes {
                if v.message_id <= poll_ref || !poll.body.choices_valid(&v.choices) {
                    continue;
                }
                let newer = latest.get(&v.voter).map_or(true, |prev| {
                    (v.message_id, &v.choices) > (prev.message_id, &prev.choices)
                });
                if newer {
                    latest.insert(v.voter, v);
                }
            }
            OpenItemContent::Poll {
                votes: latest
                    .into_values()
                    .filter(|v| !v.choices.is_empty())
                    .map(|v| HandoverVote {
                        voter: v.voter,
                        message_id: v.message_id,
                        created_at_ms: v.created_at_ms,
                        choices: v.choices.clone(),
                    })
                    .collect(),
            }
        },
        _ => return Err(BuildError::RingMismatch),
    };
    Ok(OpenItem { header, content })
}

/// A checklist the committer holds as an open item. `current` is the signed
/// body as it now stands (the original, or the author's latest edit with
/// its signature); `state` the fold of its ops
/// (`lakko_attachments::checklist::fold`); `ticks` the tick records the
/// fold was given, from which each done item's tick row id is taken.
///
/// # Errors
///
/// [`BuildError::SignatureMissing`], [`BuildError::Body`],
/// [`BuildError::StateMismatch`] when `state`'s title, flags or author items
/// are not `current`'s.
pub fn checklist_open_item(
    poll_ref: i64,
    current: &SignedChecklist,
    author: &[u8; 32],
    state: &ChecklistState,
    ticks: &[ChecklistTickRecord],
) -> Result<OpenItem, BuildError> {
    let body = current.body.encode().map_err(BuildError::Body)?;
    let header = header(poll_ref, author, current.author_sig, body)?;
    let author_items: Vec<(u32, &str)> = state
        .items
        .iter()
        .filter_map(|i| match i.item {
            ChecklistItemRef::Author(id) => Some((id, i.text.as_str())),
            ChecklistItemRef::Added(_) => None,
        })
        .collect();
    let body_items: Vec<(u32, &str)> = current
        .body
        .items
        .iter()
        .map(|i| (i.id, i.text.as_str()))
        .collect();
    if state.title != current.body.title
        || state.others_add != current.body.others_add
        || state.others_mark != current.body.others_mark
        || author_items != body_items
    {
        return Err(BuildError::StateMismatch);
    }
    let mut added = Vec::new();
    let mut done = Vec::new();
    for item in &state.items {
        if let (ChecklistItemRef::Added(m), Some(adder)) = (item.item, item.added_by) {
            added.push(HandoverAdded {
                message_id: m,
                adder,
                text: item.text.clone(),
            });
        }
        let Some(ticker) = item.done_by else {
            continue;
        };
        let row = ticks
            .iter()
            .filter(|t| {
                t.item == item.item && t.ticker == ticker && t.done && t.message_id > poll_ref
            })
            .map(|t| t.message_id)
            .max();
        if let Some(message_id) = row {
            done.push(HandoverTick {
                item: match item.item {
                    ChecklistItemRef::Author(id) => HandoverItemRef::Author(id),
                    ChecklistItemRef::Added(m) => HandoverItemRef::Added(m),
                },
                ticker,
                message_id,
                done: true,
            });
        }
    }
    Ok(OpenItem {
        header,
        content: OpenItemContent::Checklist { added, ticks: done },
    })
}
