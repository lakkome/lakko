// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 section 3.9 on the UniFFI surface: the hand-over of open polls
//! and checklists, and of old ballot rows, to a device a commit admitted.
//!
//! Committer: build an [`OpenItem`] per held open poll / checklist
//! ([`open_item_for_poll`], [`open_item_for_checklist`]) when
//! [`open_items_handover_due`] says so, pack them with
//! [`plan_open_items_rows`] and post each row in the group; for an anonymous
//! poll also post [`plan_poll_ballots_rows`] of its stored ballot rows that
//! [`ballot_needs_handover`].
//!
//! Newcomer: a dispatched `OpenItems` row is kept with its sender's account
//! key when [`open_items_sender_trusted`]; [`assemble_open_items`] joins the
//! parts of every kept row, and [`adopt_open_item`] checks each whole item
//! (author signature, ring version 0, votes, ticks) and gives the records to
//! store. A dispatched `PollBallots` row is read with
//! [`decode_poll_ballots_envelope`].
//!
//! Errors: [`FfiError::OpenItems`] `"<code>: <reason>"` with the codes of
//! `lakko_contacts::OpenItemsError`, `PollBallotsError`,
//! `lakko_open_items::BuildError` / `AdoptError`, or `badLength`.

use std::collections::BTreeSet;
use std::sync::Arc;

use lakko_contacts::open_items as oi;
use lakko_contacts::poll_ballots as pb;

use crate::ballot::PollRing;
use crate::content::{
    ChecklistAddRecord, ChecklistState, ChecklistTickRecord, PollVoteRecord, SignedChecklist,
    SignedPoll,
};
use crate::FfiError;

fn err(code: &str, e: impl std::fmt::Display) -> FfiError {
    FfiError::OpenItems(format!("{code}: {e}"))
}

fn key32(v: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(v).map_err(|_| {
        FfiError::OpenItems(format!(
            "badLength: {what} must be 32 bytes, got {}",
            v.len()
        ))
    })
}

/// One open poll or checklist, built by the committer. Opaque.
#[derive(uniffi::Object)]
pub struct OpenItem {
    inner: oi::OpenItem,
}

#[uniffi::export]
impl OpenItem {
    /// The DS `messageId` of the poll or checklist row.
    #[must_use]
    pub fn poll_ref(&self) -> i64 {
        self.inner.header.poll_ref
    }
}

fn vote_records(
    votes: Vec<PollVoteRecord>,
) -> Result<Vec<lakko_attachments::poll::PollVoteRecord>, FfiError> {
    votes
        .into_iter()
        .map(|v| {
            Ok(lakko_attachments::poll::PollVoteRecord {
                message_id: v.message_id,
                created_at_ms: v.created_at_ms,
                voter: key32(&v.voter, "voter")?,
                choices: v.choices,
            })
        })
        .collect()
}

/// The hand-over item of a poll: an anonymous poll with its `ring`, a
/// non-anonymous one with its stored `votes` (ring `None`).
///
/// # Errors
///
/// `signatureMissing`, `body`, `ringMismatch`, `badLength`, or a
/// [`FfiError::Content`] / `LockPoisoned` from the arguments.
#[uniffi::export]
pub fn open_item_for_poll(
    poll_ref: i64,
    poll: SignedPoll,
    author: Vec<u8>,
    ring: Option<Arc<PollRing>>,
    votes: Vec<PollVoteRecord>,
) -> Result<Arc<OpenItem>, FfiError> {
    let poll = lakko_attachments::SignedPoll::try_from(poll)?;
    let author = key32(&author, "author")?;
    let votes = vote_records(votes)?;
    let built = match ring {
        Some(r) => {
            let guard = r.lock()?;
            lakko_open_items::poll_open_item(poll_ref, &poll, &author, Some(&guard), &votes)
        },
        None => lakko_open_items::poll_open_item(poll_ref, &poll, &author, None, &votes),
    }
    .map_err(|e| err(e.code(), &e))?;
    Ok(Arc::new(OpenItem { inner: built }))
}

/// The hand-over item of a checklist: its current (latest edit) signed
/// form, its folded state and the stored ticks.
///
/// # Errors
///
/// `signatureMissing`, `body`, `stateMismatch`, `badLength`, or a
/// [`FfiError::Content`] from the arguments.
#[uniffi::export]
pub fn open_item_for_checklist(
    poll_ref: i64,
    current: SignedChecklist,
    author: Vec<u8>,
    state: ChecklistState,
    ticks: Vec<ChecklistTickRecord>,
) -> Result<Arc<OpenItem>, FfiError> {
    let current = lakko_attachments::SignedChecklist::try_from(current)?;
    let author = key32(&author, "author")?;
    let state = lakko_attachments::checklist::ChecklistState::try_from(state)?;
    let ticks = ticks
        .into_iter()
        .map(|t| {
            Ok(lakko_attachments::checklist::ChecklistTickRecord {
                message_id: t.message_id,
                ticker: key32(&t.ticker, "ticker")?,
                privileged: t.privileged,
                item: t.item.into(),
                done: t.done,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let built = lakko_open_items::checklist_open_item(poll_ref, &current, &author, &state, &ticks)
        .map_err(|e| err(e.code(), &e))?;
    Ok(Arc::new(OpenItem { inner: built }))
}

/// Whether a commit calls for a hand-over: it added a leaf and the
/// committer holds something to hand over.
#[uniffi::export]
#[must_use]
pub fn open_items_handover_due(added_leaves: u32, held_items: u32) -> bool {
    oi::open_items_handover_due(added_leaves as usize, held_items as usize)
}

/// Pack the items into open-items envelopes to post in the group, in
/// `poll_ref` order, splitting a large ring or vote list.
///
/// # Errors
///
/// `duplicateItem` or the first rule an item breaks.
#[uniffi::export]
pub fn plan_open_items_rows(items: Vec<Arc<OpenItem>>) -> Result<Vec<Vec<u8>>, FfiError> {
    let items: Vec<oi::OpenItem> = items.iter().map(|i| i.inner.clone()).collect();
    oi::plan_open_items_rows(&items).map_err(|e| err(e.code(), e))
}

/// Whether to keep a hand-over from `sender_account_key` (its MLS sender at
/// receipt): the account that admitted this device, or an owner / admin.
///
/// # Errors
///
/// `badLength`.
#[uniffi::export]
pub fn open_items_sender_trusted(
    sender_account_key: Vec<u8>,
    admitting_account_key: Option<Vec<u8>>,
    sender_is_owner_or_admin: bool,
) -> Result<bool, FfiError> {
    let sender = key32(&sender_account_key, "sender")?;
    let admitting = admitting_account_key
        .map(|k| key32(&k, "admitting"))
        .transpose()?;
    Ok(oi::open_items_sender_trusted(
        &sender,
        admitting.as_ref(),
        sender_is_owner_or_admin,
    ))
}

/// Whether `bytes` is an open-items envelope.
#[uniffi::export]
#[must_use]
pub fn is_open_items_envelope(bytes: Vec<u8>) -> bool {
    oi::is_open_items_envelope(&bytes)
}

/// One kept open-items row: the envelope and its sender's account key.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ReceivedOpenItemsRow {
    /// The sender's account key, resolved at receipt.
    pub sender_account_key: Vec<u8>,
    /// The whole envelope.
    pub envelope: Vec<u8>,
}

/// A whole item put back together from its parts. Opaque; pass it to
/// [`adopt_open_item`].
#[derive(uniffi::Object)]
pub struct AssembledOpenItem {
    inner: oi::AssembledItem,
}

#[uniffi::export]
impl AssembledOpenItem {
    /// The DS `messageId` of the poll or checklist row.
    #[must_use]
    pub fn poll_ref(&self) -> i64 {
        self.inner.header.poll_ref
    }

    /// Who handed it over.
    #[must_use]
    pub fn sender_account_key(&self) -> Vec<u8> {
        self.inner.sender_account_key.to_vec()
    }
}

/// Join the parts of every kept row into whole items, skipping items whose
/// `poll_ref` is in `held` (this device already has them). A row that does
/// not decode is skipped; a group of parts still missing a slice is not
/// returned yet.
///
/// # Errors
///
/// `badLength` for a sender key.
#[uniffi::export]
pub fn assemble_open_items(
    rows: Vec<ReceivedOpenItemsRow>,
    held: Vec<i64>,
) -> Result<Vec<Arc<AssembledOpenItem>>, FfiError> {
    let mut decoded: Vec<([u8; 32], oi::OpenItems)> = Vec::with_capacity(rows.len());
    for r in rows {
        let sender = key32(&r.sender_account_key, "sender")?;
        if let Ok(items) = oi::decode_open_items(&r.envelope) {
            decoded.push((sender, items));
        }
    }
    let received: Vec<oi::ReceivedOpenItems<'_>> = decoded
        .iter()
        .map(|(sender, items)| oi::ReceivedOpenItems {
            sender_account_key: *sender,
            items,
        })
        .collect();
    let held: BTreeSet<i64> = held.into_iter().collect();
    Ok(oi::assemble_open_items(&received, &held)
        .into_iter()
        .map(|inner| Arc::new(AssembledOpenItem { inner }))
        .collect())
}

/// What a newcomer stores for an adopted item.
#[derive(uniffi::Enum)]
pub enum AdoptedOpenItem {
    /// A poll: the signed poll, its ring (anonymous) or votes.
    Poll {
        /// The poll row's DS `messageId`.
        poll_ref: i64,
        /// The author's account key.
        author: Vec<u8>,
        /// The signed poll.
        poll: SignedPoll,
        /// The ring, for an anonymous poll.
        ring: Option<Arc<PollRing>>,
        /// The counted votes, for a non-anonymous poll.
        votes: Vec<PollVoteRecord>,
    },
    /// A checklist: the signed current form and its adds and ticks.
    Checklist {
        /// The checklist row's DS `messageId`.
        poll_ref: i64,
        /// The author's account key.
        author: Vec<u8>,
        /// The signed checklist (the latest edit).
        checklist: SignedChecklist,
        /// The added items.
        adds: Vec<ChecklistAddRecord>,
        /// The ticks.
        ticks: Vec<ChecklistTickRecord>,
    },
}

/// Check an assembled item for the group `group_id` (32 bytes) and give the
/// records to store.
///
/// # Errors
///
/// `body`, `signature`, `kindMismatch`, `ring`, `vote`, `tick`, `badLength`.
#[uniffi::export]
pub fn adopt_open_item(
    group_id: Vec<u8>,
    item: Arc<AssembledOpenItem>,
) -> Result<AdoptedOpenItem, FfiError> {
    let group = key32(&group_id, "groupId")?;
    let adopted =
        lakko_open_items::adopt_open_item(&group, &item.inner).map_err(|e| err(e.code(), &e))?;
    Ok(match adopted {
        lakko_open_items::AdoptedItem::Poll {
            poll_ref,
            author,
            poll,
            ring,
            votes,
        } => AdoptedOpenItem::Poll {
            poll_ref,
            author: author.to_vec(),
            poll: poll.into(),
            ring: ring.map(|r| PollRing::from_core(*r)),
            votes: votes
                .into_iter()
                .map(|v| PollVoteRecord {
                    message_id: v.message_id,
                    created_at_ms: v.created_at_ms,
                    voter: v.voter.to_vec(),
                    choices: v.choices,
                })
                .collect(),
        },
        lakko_open_items::AdoptedItem::Checklist {
            poll_ref,
            author,
            checklist,
            adds,
            ticks,
        } => AdoptedOpenItem::Checklist {
            poll_ref,
            author: author.to_vec(),
            checklist: checklist.into(),
            adds: adds
                .into_iter()
                .map(|a| ChecklistAddRecord {
                    message_id: a.message_id,
                    adder: a.adder.to_vec(),
                    privileged: a.privileged,
                    text: a.text,
                })
                .collect(),
            ticks: ticks
                .into_iter()
                .map(|t| ChecklistTickRecord {
                    message_id: t.message_id,
                    ticker: t.ticker.to_vec(),
                    privileged: t.privileged,
                    item: t.item.into(),
                    done: t.done,
                })
                .collect(),
        },
    })
}

// ============================================================================
// Old ballot rows
// ============================================================================

/// A decoded poll-ballots envelope.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollBallotsEnvelope {
    /// The poll row's DS `messageId`.
    pub poll_ref: i64,
    /// The sealed ballot rows.
    pub rows: Vec<Vec<u8>>,
}

/// Whether `bytes` is a poll-ballots envelope.
#[uniffi::export]
#[must_use]
pub fn is_poll_ballots_envelope(bytes: Vec<u8>) -> bool {
    pb::is_poll_ballots_envelope(&bytes)
}

/// Decode a poll-ballots envelope.
///
/// # Errors
///
/// The `PollBallotsError` code.
#[uniffi::export]
pub fn decode_poll_ballots_envelope(bytes: Vec<u8>) -> Result<PollBallotsEnvelope, FfiError> {
    let b = pb::decode_poll_ballots(&bytes).map_err(|e| err(e.code(), e))?;
    Ok(PollBallotsEnvelope {
        poll_ref: b.poll_ref,
        rows: b.rows,
    })
}

/// Whether the committer hands over a stored ballot row created at
/// `created_at_ms` (the journal may no longer hold it).
#[uniffi::export]
#[must_use]
pub fn ballot_needs_handover(created_at_ms: i64, now_ms: i64) -> bool {
    pb::ballot_needs_handover(created_at_ms, now_ms)
}

/// The committer's old-ballot envelopes for one poll.
#[uniffi::export]
#[must_use]
pub fn plan_poll_ballots_rows(poll_ref: i64, rows: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    pb::plan_poll_ballots_rows(poll_ref, &rows)
}
