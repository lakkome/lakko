// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The newcomer's side: check an assembled item and turn it into the
//! records the device stores (ADR-0092 section 3.9, "What the newcomer
//! trusts").
//!
//! Checked here, so that the sender cannot invent or alter an item in the
//! author's name: the body decodes strictly, the author signature verifies
//! over it for this group under the stated author key, an anonymous item is
//! an anonymous poll and its ring decodes with version 0 hashing to the
//! signed `ring0_hash`, a non-anonymous item is a non-anonymous poll and
//! every vote's choices are valid for it, every tick of an author item names
//! an item of the body. Any failure refuses the whole item.
//!
//! Not checkable here, and resting on the sender's word (accepted by the
//! ADR): the ring extensions, the earlier votes, the added items and the
//! ticks. The added items and ticks come back as `privileged` records: the
//! committer's fold already applied the authority rule under the flags as
//! they were, which this device never saw, so the newcomer's fold must not
//! judge them again against the current flags.

use lakko_attachments::checklist::{
    ChecklistAddRecord, ChecklistBody, ChecklistItemRef, ChecklistTickRecord,
};
use lakko_attachments::poll::{PollBody, PollVoteRecord};
use lakko_attachments::{ContentError, SignedChecklist, SignedPoll};
use lakko_ballot::Ring;
use lakko_contacts::open_items::{AssembledContent, AssembledItem, HandoverItemRef};
use thiserror::Error;

/// Why an item was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdoptError {
    /// The body does not decode as the item's kind.
    #[error("item body does not decode: {0}")]
    Body(ContentError),
    /// The author signature does not verify.
    #[error("item author signature does not verify")]
    Signature,
    /// A ring for a non-anonymous poll, or votes for an anonymous one.
    #[error("item kind does not match its poll")]
    KindMismatch,
    /// The ring does not decode, or its version 0 is not the signed one.
    #[error("item ring is not the poll's")]
    Ring,
    /// A vote's choices are not valid for the poll.
    #[error("item vote is not valid for the poll")]
    Vote,
    /// A tick names an author item the body does not have.
    #[error("item tick names no item")]
    Tick,
}

impl AdoptError {
    /// Stable reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Body(_) => "body",
            Self::Signature => "signature",
            Self::KindMismatch => "kindMismatch",
            Self::Ring => "ring",
            Self::Vote => "vote",
            Self::Tick => "tick",
        }
    }
}

/// An adopted item: what the newcomer stores for it.
#[derive(Clone, Debug)]
pub enum AdoptedItem {
    /// A poll.
    Poll {
        /// DS `messageId` of the poll row.
        poll_ref: i64,
        /// The author's account key.
        author: [u8; 32],
        /// The signed poll.
        poll: SignedPoll,
        /// The ring, for an anonymous poll; the device extends it on later
        /// commits as any member does.
        ring: Option<Box<Ring>>,
        /// The earlier votes, for a non-anonymous poll.
        votes: Vec<PollVoteRecord>,
    },
    /// A checklist.
    Checklist {
        /// DS `messageId` of the checklist row.
        poll_ref: i64,
        /// The author's account key.
        author: [u8; 32],
        /// The signed current body.
        checklist: SignedChecklist,
        /// The added items, as `privileged` add records.
        adds: Vec<ChecklistAddRecord>,
        /// The ticks, as `privileged` tick records.
        ticks: Vec<ChecklistTickRecord>,
    },
}

/// Check `item` for the group `group_id` and turn it into stored records.
///
/// # Errors
///
/// Every [`AdoptError`]; the item is then dropped whole.
pub fn adopt_open_item(
    group_id: &[u8; 32],
    item: &AssembledItem,
) -> Result<AdoptedItem, AdoptError> {
    let header = &item.header;
    let author = header.author_account_key;
    match &item.content {
        AssembledContent::AnonymousPoll { ring } => {
            let poll = signed_poll(group_id, item)?;
            let anon = poll
                .body
                .anonymous
                .as_ref()
                .ok_or(AdoptError::KindMismatch)?;
            let ring = Ring::decode(ring).map_err(|_| AdoptError::Ring)?;
            if ring.version_hash(0) != Some(anon.ring0_hash) {
                return Err(AdoptError::Ring);
            }
            Ok(AdoptedItem::Poll {
                poll_ref: header.poll_ref,
                author,
                poll,
                ring: Some(Box::new(ring)),
                votes: Vec::new(),
            })
        },
        AssembledContent::Poll { votes } => {
            let poll = signed_poll(group_id, item)?;
            if poll.body.anonymous.is_some() {
                return Err(AdoptError::KindMismatch);
            }
            let mut records = Vec::with_capacity(votes.len());
            for v in votes {
                if v.choices.is_empty() || !poll.body.choices_valid(&v.choices) {
                    return Err(AdoptError::Vote);
                }
                records.push(PollVoteRecord {
                    message_id: v.message_id,
                    created_at_ms: v.created_at_ms,
                    voter: v.voter,
                    choices: v.choices.clone(),
                });
            }
            Ok(AdoptedItem::Poll {
                poll_ref: header.poll_ref,
                author,
                poll,
                ring: None,
                votes: records,
            })
        },
        AssembledContent::Checklist { added, ticks } => {
            let body = ChecklistBody::decode(&header.body).map_err(AdoptError::Body)?;
            let checklist = SignedChecklist {
                body,
                author_sig: Some(header.author_sig),
            };
            checklist
                .verify(group_id, &author)
                .map_err(|_| AdoptError::Signature)?;
            let adds = added
                .iter()
                .map(|a| ChecklistAddRecord {
                    message_id: a.message_id,
                    adder: a.adder,
                    privileged: true,
                    text: a.text.clone(),
                })
                .collect();
            let mut records = Vec::with_capacity(ticks.len());
            for t in ticks {
                let target = match t.item {
                    HandoverItemRef::Author(id) => {
                        if !checklist.body.items.iter().any(|i| i.id == id) {
                            return Err(AdoptError::Tick);
                        }
                        ChecklistItemRef::Author(id)
                    },
                    HandoverItemRef::Added(m) => ChecklistItemRef::Added(m),
                };
                records.push(ChecklistTickRecord {
                    message_id: t.message_id,
                    ticker: t.ticker,
                    privileged: true,
                    item: target,
                    done: t.done,
                });
            }
            Ok(AdoptedItem::Checklist {
                poll_ref: header.poll_ref,
                author,
                checklist,
                adds,
                ticks: records,
            })
        },
    }
}

fn signed_poll(group_id: &[u8; 32], item: &AssembledItem) -> Result<SignedPoll, AdoptError> {
    let body = PollBody::decode(&item.header.body).map_err(AdoptError::Body)?;
    let poll = SignedPoll {
        body,
        author_sig: Some(item.header.author_sig),
    };
    poll.verify(group_id, &item.header.author_account_key)
        .map_err(|_| AdoptError::Signature)?;
    Ok(poll)
}
