// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 section 3.9 on the wasm surface: the open-items and old-ballot
//! hand-overs. The UniFFI mirror (with the flow) is
//! `lakko-ffi-uniffi/src/open_items.rs`.
//!
//! Errors throw an `Error` whose message is `"<code>: <reason>"` with the
//! codes of `OpenItemsError`, `PollBallotsError`, `BuildError`,
//! `AdoptError`, or `badLength`; content-shaped arguments may throw the
//! content module's codes.

use std::collections::BTreeSet;

use lakko_contacts::open_items as oi;
use lakko_contacts::poll_ballots as pb;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::ballot::PollRing;
use crate::content::{
    from_js, to_js, JsChecklistState, JsChecklistTickRecords, JsPollVoteRecords, JsSignedChecklist,
    JsSignedPoll, WAdd, WChecklistState, WSignedChecklist, WSignedPoll, WTick, WVote,
};

fn err(code: &str, e: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("{code}: {e}"))
}

fn key32(v: &[u8], what: &str) -> Result<[u8; 32], JsError> {
    <[u8; 32]>::try_from(v).map_err(|_| {
        JsError::new(&format!(
            "badLength: {what} must be 32 bytes, got {}",
            v.len()
        ))
    })
}

/// One open poll or checklist, built by the committer. Opaque.
#[wasm_bindgen]
#[derive(Clone)]
pub struct OpenItem {
    inner: oi::OpenItem,
}

#[wasm_bindgen]
impl OpenItem {
    /// The poll or checklist row's DS `messageId` (`bigint`).
    #[wasm_bindgen(getter, js_name = pollRef)]
    #[must_use]
    pub fn poll_ref(&self) -> i64 {
        self.inner.header.poll_ref
    }
}

/// The hand-over item of a poll: anonymous with its `ring`, otherwise with
/// its stored `votes` (`PollVoteRecord[]`, ring `undefined`).
///
/// # Errors
///
/// `signatureMissing`, `body`, `ringMismatch`, `badLength`, a shape error.
#[wasm_bindgen(js_name = openItemForPoll)]
pub fn open_item_for_poll(
    poll_ref: i64,
    poll: &JsSignedPoll,
    author: &[u8],
    ring: Option<PollRing>,
    votes: &JsPollVoteRecords,
) -> Result<OpenItem, JsError> {
    let poll: WSignedPoll = from_js(poll)?;
    let poll = poll.to_core()?;
    let author = key32(author, "author")?;
    let votes: Vec<WVote> = from_js(votes)?;
    let votes = votes
        .iter()
        .map(WVote::to_core)
        .collect::<Result<Vec<_>, _>>()?;
    let built = lakko_open_items::poll_open_item(
        poll_ref,
        &poll,
        &author,
        ring.as_ref().map(|r| &r.inner),
        &votes,
    )
    .map_err(|e| err(e.code(), &e))?;
    Ok(OpenItem { inner: built })
}

/// The hand-over item of a checklist: its current signed form, its folded
/// `ChecklistState` and the stored `ChecklistTickRecord[]`.
///
/// # Errors
///
/// `signatureMissing`, `body`, `stateMismatch`, `badLength`, a shape error.
#[wasm_bindgen(js_name = openItemForChecklist)]
pub fn open_item_for_checklist(
    poll_ref: i64,
    current: &JsSignedChecklist,
    author: &[u8],
    state: &JsChecklistState,
    ticks: &JsChecklistTickRecords,
) -> Result<OpenItem, JsError> {
    let current: WSignedChecklist = from_js(current)?;
    let current = current.to_core()?;
    let author = key32(author, "author")?;
    let state: WChecklistState = from_js(state)?;
    let state = state.to_core()?;
    let ticks: Vec<WTick> = from_js(ticks)?;
    let ticks = ticks
        .iter()
        .map(WTick::to_core)
        .collect::<Result<Vec<_>, _>>()?;
    let built = lakko_open_items::checklist_open_item(poll_ref, &current, &author, &state, &ticks)
        .map_err(|e| err(e.code(), &e))?;
    Ok(OpenItem { inner: built })
}

/// Whether a commit calls for a hand-over.
#[wasm_bindgen(js_name = openItemsHandoverDue)]
#[must_use]
pub fn open_items_handover_due(added_leaves: u32, held_items: u32) -> bool {
    oi::open_items_handover_due(added_leaves as usize, held_items as usize)
}

/// Pack items into open-items envelopes to post in the group.
///
/// # Errors
///
/// `duplicateItem` or the first rule an item breaks.
#[wasm_bindgen(js_name = planOpenItemsRows)]
pub fn plan_open_items_rows(items: Vec<OpenItem>) -> Result<Vec<js_sys::Uint8Array>, JsError> {
    let items: Vec<oi::OpenItem> = items.into_iter().map(|i| i.inner).collect();
    Ok(oi::plan_open_items_rows(&items)
        .map_err(|e| err(e.code(), e))?
        .iter()
        .map(|r| js_sys::Uint8Array::from(r.as_slice()))
        .collect())
}

/// Whether to keep a hand-over from `senderAccountKey`.
///
/// # Errors
///
/// `badLength`.
#[wasm_bindgen(js_name = openItemsSenderTrusted)]
pub fn open_items_sender_trusted(
    sender_account_key: &[u8],
    admitting_account_key: Option<Vec<u8>>,
    sender_is_owner_or_admin: bool,
) -> Result<bool, JsError> {
    let sender = key32(sender_account_key, "sender")?;
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
#[wasm_bindgen(js_name = isOpenItemsEnvelope)]
#[must_use]
pub fn is_open_items_envelope(bytes: &[u8]) -> bool {
    oi::is_open_items_envelope(bytes)
}

/// One kept open-items row.
#[wasm_bindgen]
#[derive(Clone)]
pub struct ReceivedOpenItemsRow {
    sender_account_key: Vec<u8>,
    envelope: Vec<u8>,
}

#[wasm_bindgen]
impl ReceivedOpenItemsRow {
    /// The sender's account key (resolved at receipt) and the envelope.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(sender_account_key: Vec<u8>, envelope: Vec<u8>) -> Self {
        Self {
            sender_account_key,
            envelope,
        }
    }
}

/// A whole item, put back together. Opaque; pass to `adoptOpenItem`.
#[wasm_bindgen]
pub struct AssembledOpenItem {
    inner: oi::AssembledItem,
}

#[wasm_bindgen]
impl AssembledOpenItem {
    /// The poll or checklist row's DS `messageId` (`bigint`).
    #[wasm_bindgen(getter, js_name = pollRef)]
    #[must_use]
    pub fn poll_ref(&self) -> i64 {
        self.inner.header.poll_ref
    }

    /// Who handed it over.
    #[wasm_bindgen(getter, js_name = senderAccountKey)]
    #[must_use]
    pub fn sender_account_key(&self) -> Vec<u8> {
        self.inner.sender_account_key.to_vec()
    }
}

/// Join the parts of every kept row into whole items, skipping `held`
/// (`BigInt64Array` of poll refs this device already has). A row that does
/// not decode is skipped.
///
/// # Errors
///
/// `badLength` for a sender key.
#[wasm_bindgen(js_name = assembleOpenItems)]
pub fn assemble_open_items(
    rows: Vec<ReceivedOpenItemsRow>,
    held: Vec<i64>,
) -> Result<Vec<AssembledOpenItem>, JsError> {
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
        .map(|inner| AssembledOpenItem { inner })
        .collect())
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum WAdopted {
    Poll {
        poll_ref: i64,
        author: String,
        poll: WSignedPoll,
        votes: Vec<WVote>,
    },
    Checklist {
        poll_ref: i64,
        author: String,
        checklist: WSignedChecklist,
        adds: Vec<WAdd>,
        ticks: Vec<WTick>,
    },
}

/// An adopted item: `item` is a plain object (`{ kind: "poll", pollRef,
/// author, poll, votes }` or `{ kind: "checklist", pollRef, author,
/// checklist, adds, ticks }`, byte fields base64) and `ring` the anonymous
/// poll's ring, or `undefined`.
#[wasm_bindgen]
pub struct AdoptedOpenItem {
    item: JsValue,
    ring: Option<lakko_ballot::Ring>,
}

#[wasm_bindgen]
impl AdoptedOpenItem {
    /// The records to store.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn item(&self) -> JsValue {
        self.item.clone()
    }

    /// Take the ring (an anonymous poll's); `undefined` after the first
    /// call or for any other item.
    #[wasm_bindgen(js_name = takeRing)]
    pub fn take_ring(&mut self) -> Option<PollRing> {
        self.ring.take().map(|inner| PollRing { inner })
    }
}

/// Check an assembled item for the 32-byte `groupId` and give the records
/// to store.
///
/// # Errors
///
/// `body`, `signature`, `kindMismatch`, `ring`, `vote`, `tick`, `badLength`.
#[wasm_bindgen(js_name = adoptOpenItem)]
pub fn adopt_open_item(
    group_id: &[u8],
    item: &AssembledOpenItem,
) -> Result<AdoptedOpenItem, JsError> {
    let group = key32(group_id, "groupId")?;
    let adopted =
        lakko_open_items::adopt_open_item(&group, &item.inner).map_err(|e| err(e.code(), &e))?;
    let b = |k: &[u8; 32]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, k);
    Ok(match adopted {
        lakko_open_items::AdoptedItem::Poll {
            poll_ref,
            author,
            poll,
            ring,
            votes,
        } => AdoptedOpenItem {
            item: to_js(&WAdopted::Poll {
                poll_ref,
                author: b(&author),
                poll: WSignedPoll::of(&poll),
                votes: votes.iter().map(WVote::of).collect(),
            })?,
            ring: ring.map(|r| *r),
        },
        lakko_open_items::AdoptedItem::Checklist {
            poll_ref,
            author,
            checklist,
            adds,
            ticks,
        } => AdoptedOpenItem {
            item: to_js(&WAdopted::Checklist {
                poll_ref,
                author: b(&author),
                checklist: WSignedChecklist::of(&checklist),
                adds: adds.iter().map(WAdd::of).collect(),
                ticks: ticks.iter().map(WTick::of).collect(),
            })?,
            ring: None,
        },
    })
}

/// A decoded poll-ballots envelope.
#[wasm_bindgen]
pub struct PollBallotsEnvelope {
    poll_ref: i64,
    rows: Vec<Vec<u8>>,
}

#[wasm_bindgen]
impl PollBallotsEnvelope {
    /// The poll row's DS `messageId` (`bigint`).
    #[wasm_bindgen(getter, js_name = pollRef)]
    #[must_use]
    pub fn poll_ref(&self) -> i64 {
        self.poll_ref
    }

    /// The sealed ballot rows.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rows(&self) -> Vec<js_sys::Uint8Array> {
        self.rows
            .iter()
            .map(|r| js_sys::Uint8Array::from(r.as_slice()))
            .collect()
    }
}

/// Whether `bytes` is a poll-ballots envelope.
#[wasm_bindgen(js_name = isPollBallotsEnvelope)]
#[must_use]
pub fn is_poll_ballots_envelope(bytes: &[u8]) -> bool {
    pb::is_poll_ballots_envelope(bytes)
}

/// Decode a poll-ballots envelope.
///
/// # Errors
///
/// The `PollBallotsError` code.
#[wasm_bindgen(js_name = decodePollBallotsEnvelope)]
pub fn decode_poll_ballots_envelope(bytes: &[u8]) -> Result<PollBallotsEnvelope, JsError> {
    let b = pb::decode_poll_ballots(bytes).map_err(|e| err(e.code(), e))?;
    Ok(PollBallotsEnvelope {
        poll_ref: b.poll_ref,
        rows: b.rows,
    })
}

/// Whether the committer hands over a stored ballot row created at
/// `createdAtMs` (`bigint`).
#[wasm_bindgen(js_name = ballotNeedsHandover)]
#[must_use]
pub fn ballot_needs_handover(created_at_ms: i64, now_ms: i64) -> bool {
    pb::ballot_needs_handover(created_at_ms, now_ms)
}

/// The committer's old-ballot envelopes for one poll (`rows` an array of
/// `Uint8Array`).
#[wasm_bindgen(js_name = planPollBallotsRows)]
#[must_use]
pub fn plan_poll_ballots_rows(
    poll_ref: i64,
    rows: Vec<js_sys::Uint8Array>,
) -> Vec<js_sys::Uint8Array> {
    let rows: Vec<Vec<u8>> = rows.iter().map(js_sys::Uint8Array::to_vec).collect();
    pb::plan_poll_ballots_rows(poll_ref, &rows)
        .iter()
        .map(|r| js_sys::Uint8Array::from(r.as_slice()))
        .collect()
}
