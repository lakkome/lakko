// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Open-items hand-over — [ADR-0090] section 3.9.
//!
//! A device admitted to a group reads nothing sealed before its admission,
//! so it would not see an open poll or checklist. The device that commits an
//! admission (any commit that adds a leaf, a new device of a member included)
//! therefore sends the group, after the commit merged and sealed at the new
//! epoch, the open items it holds: each poll or checklist with its author's
//! signature, an anonymous poll's ring and its versions, a non-anonymous
//! poll's counted votes, a checklist's added items and done marks.
//!
//! This module is the wire codec and the two pure halves around it:
//!
//! * [`plan_open_items_rows`] — the committer's side: packs the open items it
//!   holds into envelopes that each fit [`OPEN_ITEMS_ENVELOPE_MAX_LEN`],
//!   splitting a large ring or vote list into parts.
//! * [`open_items_sender_trusted`] and [`assemble_open_items`] — the
//!   receiver's side: whose hand-over to take, and how parts from several
//!   rows are put back together into whole items.
//!
//! The poll and checklist bodies, the ring and the votes stay bytes here:
//! `lakko-attachments` depends on this crate, so this crate cannot decode a
//! `PollBody` (a dependency cycle), and decoding the ring through
//! `lakko-ballot` would pull its curve arithmetic into the iOS notification
//! extension, which links this crate. The typed check (the author signature,
//! the body, the ring and its version-0 hash, a tick's item) is
//! `lakko-open-items`, which only the full FFI surfaces link.
//!
//! ## Wire format
//!
//! ```text
//! envelope = "lakko/open-items/v1\0"(20) || 0x01 || item_count(u8: 0..=64) || item*
//! item     = poll_ref(i64 BE) || kind(u8) || author_account_key(32) || author_sig(64)
//!            || body_len(u16 BE: 1..=4096) || body || part
//! kind 1, anonymous poll part (body starts "lakko/poll/v1\0"):
//!            ring_total(u32 BE: 1..=262144)
//!            || version_count(u16 BE: 1..) || length(u32 BE)*      strictly increasing,
//!                                                                  the last == ring_total
//!            || key_offset(u32 BE) || key_count(u32 BE: 1..) || key(32)*
//!                                                                  offset + count <= ring_total
//! kind 2, non-anonymous poll part (body starts "lakko/poll/v1\0"):
//!            vote_total(u32 BE: 0..=262144) || vote_offset(u32 BE) || vote_count(u32 BE)
//!            || vote*                                              offset + count <= total;
//!                                                                  count >= 1 unless total == 0
//!            vote = voter_key(32) || message_id(i64 BE) || created_at_ms(i64 BE)
//!                   || choice_count(u8: 1..=10) || choice(u8)*     choices < 10, ascending
//!            votes strictly ascending by voter key
//! kind 3, checklist (body starts "lakko/checklist/v1\0"):
//!            added_count(u8: 0..=30)
//!            || (message_id(i64 BE) || adder_key(32) || len(u8: 1..=100) || text)*
//!                                                                  message ids ascending
//!            || tick_count(u8: 0..=30)
//!            || (item_tag(u8) || item || ticker_key(32) || message_id(i64 BE) || done(u8: 0|1))*
//!               item_tag 1: author item_id(u32 BE); item_tag 2: added item, the
//!               message_id(i64 BE) of its add row, which must be in this item's list
//!               ticks strictly ascending by item (author items first)
//! ```
//!
//! Items in one envelope are strictly ascending by `poll_ref` (one part of a
//! poll per envelope). Every `message_id` of a vote, an added item or a tick
//! is above `poll_ref` (it follows the item's own row). The encoding is
//! canonical: [`compose_open_items`] refuses exactly what
//! [`decode_open_items`] refuses, and whatever decodes re-composes to the
//! same bytes.
//!
//! A tick names an added item by the `messageId` of its add row
//! (`lakko_attachments::checklist::ChecklistItemRef::Added`), so an item added
//! after the checklist was written can be named; the ADR's first layout had a
//! `u32` item id only.
//!
//! ## Bounds
//!
//! All chosen, not measured: at most [`OPEN_ITEMS_MAX_ITEMS`] items and
//! [`OPEN_ITEMS_ENVELOPE_MAX_LEN`] bytes per envelope (headroom under the
//! 1 MiB DS row for the MLS framing), a body of at most
//! [`OPEN_ITEMS_MAX_BODY_LEN`] bytes (above the largest poll and checklist
//! body `lakko-attachments` encodes; pinned by a test in `lakko-open-items`),
//! a ring of at most [`OPEN_ITEMS_MAX_RING_LEN`] keys (`lakko-ballot`'s cap),
//! as many votes as ring keys. A ring at the cap (8 MiB) spans about ten
//! envelopes.
//!
//! [ADR-0090]: ../../docs/adr/0090-attachment-kinds.md

use core::result::Result as CoreResult;
use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::profile::is_format_char;

/// 20-byte domain separator. Frozen.
pub const OPEN_ITEMS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/open-items/v1\x00";

/// Schema-version byte carried first after the separator.
pub const OPEN_ITEMS_VERSION_V1: u8 = 0x01;

/// Most items in one envelope. Chosen.
pub const OPEN_ITEMS_MAX_ITEMS: usize = 64;

/// Longest envelope: 900 KiB. Chosen, leaving room under the DS's 1 MiB row
/// for the MLS framing around the application message.
pub const OPEN_ITEMS_ENVELOPE_MAX_LEN: usize = 900 * 1024;

/// Longest poll or checklist body an item carries. Chosen, above the largest
/// body `lakko-attachments` encodes.
pub const OPEN_ITEMS_MAX_BODY_LEN: usize = 4096;

/// Most keys in a ring (`lakko_ballot::ring::MAX_RING_LEN`).
pub const OPEN_ITEMS_MAX_RING_LEN: usize = 1 << 18;

/// Most ring versions (`lakko_ballot::ring::MAX_VERSIONS`).
pub const OPEN_ITEMS_MAX_VERSIONS: usize = u16::MAX as usize;

/// Most votes a non-anonymous poll item carries (one per voter).
pub const OPEN_ITEMS_MAX_VOTES: usize = 1 << 18;

/// Most options of a poll (`lakko_attachments::poll::MAX_POLL_OPTIONS`).
pub const OPEN_ITEMS_MAX_CHOICES: usize = 10;

/// Most items of a checklist (`lakko_attachments::checklist::MAX_CHECKLIST_ITEMS`).
pub const OPEN_ITEMS_MAX_CHECKLIST_ITEMS: usize = 30;

/// Longest checklist item text, in bytes
/// (`lakko_attachments::checklist::MAX_CHECKLIST_ITEM_BYTES`).
pub const OPEN_ITEMS_MAX_ITEM_TEXT_BYTES: usize = 100;

/// The domain a poll body starts with (`lakko_attachments::poll::POLL_DOMAIN`).
pub const OPEN_ITEMS_POLL_BODY_PREFIX: &[u8] = b"lakko/poll/v1\x00";

/// The domain a checklist body starts with
/// (`lakko_attachments::checklist::CHECKLIST_DOMAIN`).
pub const OPEN_ITEMS_CHECKLIST_BODY_PREFIX: &[u8] = b"lakko/checklist/v1\x00";

/// Shortest envelope: no items.
pub const OPEN_ITEMS_ENVELOPE_MIN_LEN: usize = OPEN_ITEMS_ENVELOPE_DOMAIN_V1.len() + 2;

const KEY_LEN: usize = 32;
const SIG_LEN: usize = 64;
const KIND_ANONYMOUS_POLL: u8 = 1;
const KIND_POLL: u8 = 2;
const KIND_CHECKLIST: u8 = 3;
const TICK_AUTHOR: u8 = 1;
const TICK_ADDED: u8 = 2;

/// The part of an item every part of it repeats: which item, by whom, and
/// its signed body.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OpenItemHeader {
    /// The DS `messageId` of the poll or checklist row.
    pub poll_ref: i64,
    /// The author's account signature key.
    pub author_account_key: [u8; KEY_LEN],
    /// The author's signature over the body (`lakko-attachments`' author
    /// signature, bound to the group id).
    pub author_sig: [u8; SIG_LEN],
    /// The encoded poll or checklist body; for a checklist the current one
    /// (after the author's edits), which its signature covers.
    pub body: Vec<u8>,
}

impl core::fmt::Debug for OpenItemHeader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenItemHeader")
            .field("poll_ref", &self.poll_ref)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// One counted vote of a non-anonymous poll.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandoverVote {
    /// The voter's account key.
    pub voter: [u8; KEY_LEN],
    /// DS `messageId` of the vote row.
    pub message_id: i64,
    /// DS `created_at` of the vote row, milliseconds.
    pub created_at_ms: i64,
    /// The choices, strictly ascending, 1..=10 of them (a retracted vote is
    /// not handed over).
    pub choices: Vec<u8>,
}

/// An item someone added to a checklist.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandoverAdded {
    /// DS `messageId` of the add row; the added item's id.
    pub message_id: i64,
    /// The adder's account key.
    pub adder: [u8; KEY_LEN],
    /// The item text, 1..=100 bytes of display text.
    pub text: String,
}

/// Which checklist item a tick names (the same split as
/// `lakko_attachments::checklist::ChecklistItemRef`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HandoverItemRef {
    /// An item of the author's body, by its `item_id`.
    Author(u32),
    /// An added item, by the DS `messageId` of its add row.
    Added(i64),
}

/// A checklist tick as the committer folded it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandoverTick {
    /// Which item.
    pub item: HandoverItemRef,
    /// Who marked it.
    pub ticker: [u8; KEY_LEN],
    /// DS `messageId` of the tick row.
    pub message_id: i64,
    /// Done or not.
    pub done: bool,
}

/// The kind-specific part of one item in one envelope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OpenItemPart {
    /// A slice of an anonymous poll's ring.
    AnonymousPoll {
        /// Keys in the whole ring.
        ring_total: u32,
        /// `L_k` of every version, strictly increasing, the last
        /// `ring_total`.
        lengths: Vec<u32>,
        /// Position in the ring of `keys[0]`.
        key_offset: u32,
        /// The keys `key_offset..key_offset + keys.len()` of the ring.
        keys: Vec<[u8; KEY_LEN]>,
    },
    /// A slice of a non-anonymous poll's counted votes.
    Poll {
        /// Votes in the whole list.
        vote_total: u32,
        /// Position of `votes[0]` in the list (sorted by voter key).
        vote_offset: u32,
        /// The votes.
        votes: Vec<HandoverVote>,
    },
    /// A whole checklist's added items and ticks.
    Checklist {
        /// Added items, ascending by `message_id`.
        added: Vec<HandoverAdded>,
        /// Ticks, ascending by item.
        ticks: Vec<HandoverTick>,
    },
}

/// One item (or one part of a large one) in an envelope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpenItemEntry {
    /// Which item and its signed body.
    pub header: OpenItemHeader,
    /// The kind-specific part.
    pub part: OpenItemPart,
}

/// A decoded open-items envelope.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct OpenItems {
    /// The items, strictly ascending by `poll_ref`.
    pub items: Vec<OpenItemEntry>,
}

/// Why an open-items envelope was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OpenItemsError {
    /// The plaintext does not carry the domain separator. Only from
    /// [`decode_open_items`].
    #[error("plaintext does not carry the open-items domain separator")]
    WrongDomain,
    /// The version byte is not [`OPEN_ITEMS_VERSION_V1`].
    #[error("open-items envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The bytes ended before the layout did, or bytes follow it.
    #[error("open-items envelope does not have its layout's length")]
    BadLength,
    /// The encoded envelope is longer than [`OPEN_ITEMS_ENVELOPE_MAX_LEN`].
    #[error("open-items envelope is longer than {OPEN_ITEMS_ENVELOPE_MAX_LEN} bytes")]
    TooLong,
    /// More than [`OPEN_ITEMS_MAX_ITEMS`] items.
    #[error("open-items envelope carries too many items")]
    TooManyItems,
    /// Items not strictly ascending by `poll_ref`.
    #[error("open-items items are not strictly ascending")]
    ItemOrder,
    /// An unknown item kind.
    #[error("open-items item kind {0} is unknown")]
    UnknownKind(u8),
    /// A body empty, too long, or not starting with its kind's domain.
    #[error("open-items item body is malformed")]
    Body,
    /// A ring part outside its bounds (sizes, versions, offsets).
    #[error("open-items ring part is malformed")]
    Ring,
    /// A vote part outside its bounds, or votes out of order.
    #[error("open-items vote part is malformed")]
    Votes,
    /// A checklist part outside its bounds, out of order, or naming an
    /// added item it does not carry.
    #[error("open-items checklist part is malformed")]
    Checklist,
    /// A message id at or below the item's own row.
    #[error("open-items op precedes its item")]
    OpOrder,
    /// Two items in one call of [`plan_open_items_rows`] name one `poll_ref`.
    #[error("two open items name the same row")]
    DuplicateItem,
}

impl OpenItemsError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadLength => "badLength",
            Self::TooLong => "tooLong",
            Self::TooManyItems => "tooManyItems",
            Self::ItemOrder => "itemOrder",
            Self::UnknownKind(_) => "unknownKind",
            Self::Body => "body",
            Self::Ring => "ring",
            Self::Votes => "votes",
            Self::Checklist => "checklist",
            Self::OpOrder => "opOrder",
            Self::DuplicateItem => "duplicateItem",
        }
    }
}

type Result<T> = CoreResult<T, OpenItemsError>;

const fn kind_of(part: &OpenItemPart) -> u8 {
    match part {
        OpenItemPart::AnonymousPoll { .. } => KIND_ANONYMOUS_POLL,
        OpenItemPart::Poll { .. } => KIND_POLL,
        OpenItemPart::Checklist { .. } => KIND_CHECKLIST,
    }
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn display_text_ok(text: &str) -> bool {
    (1..=OPEN_ITEMS_MAX_ITEM_TEXT_BYTES).contains(&text.len())
        && !text.chars().any(|c| c.is_control() || is_format_char(c))
}

fn choices_ok(choices: &[u8]) -> bool {
    (1..=OPEN_ITEMS_MAX_CHOICES).contains(&choices.len())
        && choices
            .iter()
            .all(|&c| usize::from(c) < OPEN_ITEMS_MAX_CHOICES)
        && choices.windows(2).all(|w| w[0] < w[1])
}

/// Check one entry against every rule the decoder enforces.
fn validate_entry(entry: &OpenItemEntry) -> Result<()> {
    let header = &entry.header;
    let prefix = match entry.part {
        OpenItemPart::Checklist { .. } => OPEN_ITEMS_CHECKLIST_BODY_PREFIX,
        _ => OPEN_ITEMS_POLL_BODY_PREFIX,
    };
    if header.body.is_empty()
        || header.body.len() > OPEN_ITEMS_MAX_BODY_LEN
        || !header.body.starts_with(prefix)
    {
        return Err(OpenItemsError::Body);
    }
    let after = |m: i64| m > header.poll_ref;
    match &entry.part {
        OpenItemPart::AnonymousPoll {
            ring_total,
            lengths,
            key_offset,
            keys,
        } => {
            let total = *ring_total as usize;
            let ok = (1..=OPEN_ITEMS_MAX_RING_LEN).contains(&total)
                && (1..=OPEN_ITEMS_MAX_VERSIONS).contains(&lengths.len())
                && lengths.first().is_some_and(|&l| l >= 1)
                && lengths.windows(2).all(|w| w[0] < w[1])
                && lengths.last() == Some(ring_total)
                && !keys.is_empty()
                && (*key_offset as usize)
                    .checked_add(keys.len())
                    .is_some_and(|end| end <= total);
            if !ok {
                return Err(OpenItemsError::Ring);
            }
        },
        OpenItemPart::Poll {
            vote_total,
            vote_offset,
            votes,
        } => {
            let total = *vote_total as usize;
            let shape = total <= OPEN_ITEMS_MAX_VOTES
                && (*vote_offset as usize)
                    .checked_add(votes.len())
                    .is_some_and(|end| end <= total)
                && (total == 0 || !votes.is_empty())
                && votes.windows(2).all(|w| w[0].voter < w[1].voter)
                && votes.iter().all(|v| choices_ok(&v.choices));
            if !shape {
                return Err(OpenItemsError::Votes);
            }
            if !votes.iter().all(|v| after(v.message_id)) {
                return Err(OpenItemsError::OpOrder);
            }
        },
        OpenItemPart::Checklist { added, ticks } => {
            let added_ids: BTreeSet<i64> = added.iter().map(|a| a.message_id).collect();
            let shape = added.len() <= OPEN_ITEMS_MAX_CHECKLIST_ITEMS
                && ticks.len() <= OPEN_ITEMS_MAX_CHECKLIST_ITEMS
                && added.windows(2).all(|w| w[0].message_id < w[1].message_id)
                && added.iter().all(|a| display_text_ok(&a.text))
                && ticks.windows(2).all(|w| w[0].item < w[1].item)
                && ticks.iter().all(|t| match t.item {
                    HandoverItemRef::Author(_) => true,
                    HandoverItemRef::Added(m) => added_ids.contains(&m),
                });
            if !shape {
                return Err(OpenItemsError::Checklist);
            }
            if !added.iter().all(|a| after(a.message_id))
                || !ticks.iter().all(|t| after(t.message_id))
            {
                return Err(OpenItemsError::OpOrder);
            }
        },
    }
    Ok(())
}

fn encode_entry(out: &mut Vec<u8>, entry: &OpenItemEntry) {
    let h = &entry.header;
    out.extend_from_slice(&h.poll_ref.to_be_bytes());
    out.push(kind_of(&entry.part));
    out.extend_from_slice(&h.author_account_key);
    out.extend_from_slice(&h.author_sig);
    out.extend_from_slice(
        &u16::try_from(h.body.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(&h.body);
    match &entry.part {
        OpenItemPart::AnonymousPoll {
            ring_total,
            lengths,
            key_offset,
            keys,
        } => {
            out.extend_from_slice(&ring_total.to_be_bytes());
            out.extend_from_slice(
                &u16::try_from(lengths.len())
                    .unwrap_or(u16::MAX)
                    .to_be_bytes(),
            );
            for l in lengths {
                out.extend_from_slice(&l.to_be_bytes());
            }
            out.extend_from_slice(&key_offset.to_be_bytes());
            out.extend_from_slice(&len_u32(keys.len()).to_be_bytes());
            for k in keys {
                out.extend_from_slice(k);
            }
        },
        OpenItemPart::Poll {
            vote_total,
            vote_offset,
            votes,
        } => {
            out.extend_from_slice(&vote_total.to_be_bytes());
            out.extend_from_slice(&vote_offset.to_be_bytes());
            out.extend_from_slice(&len_u32(votes.len()).to_be_bytes());
            for v in votes {
                encode_vote(out, v);
            }
        },
        OpenItemPart::Checklist { added, ticks } => {
            out.push(u8::try_from(added.len()).unwrap_or(u8::MAX));
            for a in added {
                out.extend_from_slice(&a.message_id.to_be_bytes());
                out.extend_from_slice(&a.adder);
                out.push(u8::try_from(a.text.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(a.text.as_bytes());
            }
            out.push(u8::try_from(ticks.len()).unwrap_or(u8::MAX));
            for t in ticks {
                match t.item {
                    HandoverItemRef::Author(id) => {
                        out.push(TICK_AUTHOR);
                        out.extend_from_slice(&id.to_be_bytes());
                    },
                    HandoverItemRef::Added(m) => {
                        out.push(TICK_ADDED);
                        out.extend_from_slice(&m.to_be_bytes());
                    },
                }
                out.extend_from_slice(&t.ticker);
                out.extend_from_slice(&t.message_id.to_be_bytes());
                out.push(u8::from(t.done));
            }
        },
    }
}

fn encode_vote(out: &mut Vec<u8>, v: &HandoverVote) {
    out.extend_from_slice(&v.voter);
    out.extend_from_slice(&v.message_id.to_be_bytes());
    out.extend_from_slice(&v.created_at_ms.to_be_bytes());
    out.push(u8::try_from(v.choices.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(&v.choices);
}

fn vote_len(v: &HandoverVote) -> usize {
    KEY_LEN + 8 + 8 + 1 + v.choices.len()
}

/// Compose an envelope: the plaintext of one MLS application message to the
/// group. [`decode_open_items`] accepts exactly what this produces.
///
/// # Errors
///
/// Every [`OpenItemsError`] the decoder would return for the result, except
/// [`OpenItemsError::WrongDomain`], [`OpenItemsError::UnknownVersion`] and
/// [`OpenItemsError::BadLength`].
pub fn compose_open_items(items: &OpenItems) -> Result<Vec<u8>> {
    if items.items.len() > OPEN_ITEMS_MAX_ITEMS {
        return Err(OpenItemsError::TooManyItems);
    }
    if !items
        .items
        .windows(2)
        .all(|w| w[0].header.poll_ref < w[1].header.poll_ref)
    {
        return Err(OpenItemsError::ItemOrder);
    }
    let mut out = Vec::new();
    out.extend_from_slice(OPEN_ITEMS_ENVELOPE_DOMAIN_V1);
    out.push(OPEN_ITEMS_VERSION_V1);
    out.push(u8::try_from(items.items.len()).unwrap_or(u8::MAX));
    for entry in &items.items {
        validate_entry(entry)?;
        encode_entry(&mut out, entry);
        if out.len() > OPEN_ITEMS_ENVELOPE_MAX_LEN {
            return Err(OpenItemsError::TooLong);
        }
    }
    Ok(out)
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.bytes.len() < n {
            return Err(OpenItemsError::BadLength);
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i64(&mut self) -> Result<i64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(i64::from_be_bytes(a))
    }
    fn key(&mut self) -> Result<[u8; KEY_LEN]> {
        let mut a = [0u8; KEY_LEN];
        a.copy_from_slice(self.take(KEY_LEN)?);
        Ok(a)
    }
    /// `count` fixed-size elements must fit what is left before reading.
    fn check_room(&self, count: usize, each: usize) -> Result<()> {
        match count.checked_mul(each) {
            Some(n) if n <= self.bytes.len() => Ok(()),
            _ => Err(OpenItemsError::BadLength),
        }
    }
}

#[allow(clippy::too_many_lines)] // one linear layout; splitting it hides the order
fn decode_entry(r: &mut Reader<'_>) -> Result<OpenItemEntry> {
    let poll_ref = r.i64()?;
    let kind = r.u8()?;
    if !matches!(kind, KIND_ANONYMOUS_POLL | KIND_POLL | KIND_CHECKLIST) {
        return Err(OpenItemsError::UnknownKind(kind));
    }
    let author_account_key = r.key()?;
    let mut author_sig = [0u8; SIG_LEN];
    author_sig.copy_from_slice(r.take(SIG_LEN)?);
    let body_len = usize::from(r.u16()?);
    if body_len == 0 || body_len > OPEN_ITEMS_MAX_BODY_LEN {
        return Err(OpenItemsError::Body);
    }
    let body = r.take(body_len)?.to_vec();
    let part = match kind {
        KIND_ANONYMOUS_POLL => {
            let ring_total = r.u32()?;
            let versions = usize::from(r.u16()?);
            r.check_room(versions, 4)?;
            let mut lengths = Vec::with_capacity(versions);
            for _ in 0..versions {
                lengths.push(r.u32()?);
            }
            let key_offset = r.u32()?;
            let count = r.u32()? as usize;
            if count > OPEN_ITEMS_MAX_RING_LEN {
                return Err(OpenItemsError::Ring);
            }
            r.check_room(count, KEY_LEN)?;
            let mut keys = Vec::with_capacity(count);
            for _ in 0..count {
                keys.push(r.key()?);
            }
            OpenItemPart::AnonymousPoll {
                ring_total,
                lengths,
                key_offset,
                keys,
            }
        },
        KIND_POLL => {
            let vote_total = r.u32()?;
            let vote_offset = r.u32()?;
            let count = r.u32()? as usize;
            if count > OPEN_ITEMS_MAX_VOTES {
                return Err(OpenItemsError::Votes);
            }
            r.check_room(count, KEY_LEN + 8 + 8 + 1)?;
            let mut votes = Vec::with_capacity(count);
            for _ in 0..count {
                let voter_key = r.key()?;
                let message_id = r.i64()?;
                let created_at_ms = r.i64()?;
                let n = usize::from(r.u8()?);
                if n > OPEN_ITEMS_MAX_CHOICES {
                    return Err(OpenItemsError::Votes);
                }
                let choices = r.take(n)?.to_vec();
                votes.push(HandoverVote {
                    voter: voter_key,
                    message_id,
                    created_at_ms,
                    choices,
                });
            }
            OpenItemPart::Poll {
                vote_total,
                vote_offset,
                votes,
            }
        },
        _ => {
            let added_count = usize::from(r.u8()?);
            if added_count > OPEN_ITEMS_MAX_CHECKLIST_ITEMS {
                return Err(OpenItemsError::Checklist);
            }
            let mut added = Vec::with_capacity(added_count);
            for _ in 0..added_count {
                let message_id = r.i64()?;
                let adder_key = r.key()?;
                let len = usize::from(r.u8()?);
                let text = core::str::from_utf8(r.take(len)?)
                    .map_err(|_| OpenItemsError::Checklist)?
                    .to_owned();
                added.push(HandoverAdded {
                    message_id,
                    adder: adder_key,
                    text,
                });
            }
            let tick_count = usize::from(r.u8()?);
            if tick_count > OPEN_ITEMS_MAX_CHECKLIST_ITEMS {
                return Err(OpenItemsError::Checklist);
            }
            let mut ticks = Vec::with_capacity(tick_count);
            for _ in 0..tick_count {
                let item = match r.u8()? {
                    TICK_AUTHOR => HandoverItemRef::Author(r.u32()?),
                    TICK_ADDED => HandoverItemRef::Added(r.i64()?),
                    _ => return Err(OpenItemsError::Checklist),
                };
                let ticker = r.key()?;
                let message_id = r.i64()?;
                let done = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(OpenItemsError::Checklist),
                };
                ticks.push(HandoverTick {
                    item,
                    ticker,
                    message_id,
                    done,
                });
            }
            OpenItemPart::Checklist { added, ticks }
        },
    };
    let entry = OpenItemEntry {
        header: OpenItemHeader {
            poll_ref,
            author_account_key,
            author_sig,
            body,
        },
        part,
    };
    validate_entry(&entry)?;
    Ok(entry)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(items))` — the separator matched and the envelope is
///   well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed; dropped.
///
/// # Errors
///
/// Every [`OpenItemsError`] but [`OpenItemsError::WrongDomain`] and
/// [`OpenItemsError::DuplicateItem`]. The version is checked before any
/// length.
pub fn try_unwrap_open_items(plaintext: &[u8]) -> Result<Option<OpenItems>> {
    let Some(payload) = plaintext.strip_prefix(OPEN_ITEMS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(OpenItemsError::BadLength)?;
    if version != OPEN_ITEMS_VERSION_V1 {
        return Err(OpenItemsError::UnknownVersion(version));
    }
    if plaintext.len() > OPEN_ITEMS_ENVELOPE_MAX_LEN {
        return Err(OpenItemsError::TooLong);
    }
    let mut r = Reader { bytes: rest };
    let count = usize::from(r.u8()?);
    if count > OPEN_ITEMS_MAX_ITEMS {
        return Err(OpenItemsError::TooManyItems);
    }
    let mut items: Vec<OpenItemEntry> = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = decode_entry(&mut r)?;
        if items
            .last()
            .is_some_and(|prev| prev.header.poll_ref >= entry.header.poll_ref)
        {
            return Err(OpenItemsError::ItemOrder);
        }
        items.push(entry);
    }
    if !r.bytes.is_empty() {
        return Err(OpenItemsError::BadLength);
    }
    Ok(Some(OpenItems { items }))
}

/// Strict standalone decoder.
///
/// # Errors
///
/// [`OpenItemsError::WrongDomain`] when `bytes` does not start with
/// [`OPEN_ITEMS_ENVELOPE_DOMAIN_V1`], else what [`try_unwrap_open_items`]
/// returns.
pub fn decode_open_items(bytes: &[u8]) -> Result<OpenItems> {
    try_unwrap_open_items(bytes)?.ok_or(OpenItemsError::WrongDomain)
}

/// Whether `plaintext` carries the open-items domain (the notification
/// extension's suppress test; nothing is decoded).
#[must_use]
pub fn is_open_items_envelope(plaintext: &[u8]) -> bool {
    plaintext.starts_with(OPEN_ITEMS_ENVELOPE_DOMAIN_V1)
}

// =============================================================================
// The committer's side
// =============================================================================

/// What the committer holds about one open item, whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenItemContent {
    /// An anonymous poll: its whole ring in ring order and its version
    /// lengths (the `lakko_ballot::Ring` the committer keeps).
    AnonymousPoll {
        /// Every key, in ring order.
        keys: Vec<[u8; KEY_LEN]>,
        /// `L_k` of every version.
        lengths: Vec<u32>,
    },
    /// A non-anonymous poll: the counted votes, one per voter (any order;
    /// the planner sorts them by voter key).
    Poll {
        /// The votes.
        votes: Vec<HandoverVote>,
    },
    /// A checklist: its added items and ticks (any order; sorted by the
    /// planner).
    Checklist {
        /// Added items.
        added: Vec<HandoverAdded>,
        /// Ticks.
        ticks: Vec<HandoverTick>,
    },
}

/// One open item the committer hands over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenItem {
    /// Which item and its signed body.
    pub header: OpenItemHeader,
    /// Everything the newcomer needs besides the body.
    pub content: OpenItemContent,
}

/// Whether a commit calls for a hand-over: it added at least one leaf (a new
/// account, or a new device of an account already in the group; ADR-0090
/// section 3.9, "When it is sent") and the committer holds something to
/// hand over (an open item, or an old ballot row).
#[must_use]
pub const fn open_items_handover_due(added_leaves: usize, held_items: usize) -> bool {
    added_leaves > 0 && held_items > 0
}

fn encoded_entry_len(entry: &OpenItemEntry) -> usize {
    let mut v = Vec::new();
    encode_entry(&mut v, entry);
    v.len()
}

struct Packer {
    rows: Vec<OpenItems>,
    current: OpenItems,
    current_len: usize,
}

impl Packer {
    fn flush(&mut self) {
        if !self.current.items.is_empty() {
            self.rows.push(core::mem::take(&mut self.current));
        }
        self.current_len = OPEN_ITEMS_ENVELOPE_MIN_LEN;
    }
    fn room(&self) -> usize {
        if self.current.items.len() >= OPEN_ITEMS_MAX_ITEMS {
            0
        } else {
            OPEN_ITEMS_ENVELOPE_MAX_LEN.saturating_sub(self.current_len)
        }
    }
    fn push(&mut self, entry: OpenItemEntry, len: usize) {
        self.current_len += len;
        self.current.items.push(entry);
    }
}

/// Split `elements` into parts that each fit a row. `make` builds a part
/// from a slice and its offset; `each` is one element's encoded length.
fn pack_split<T: Clone>(
    packer: &mut Packer,
    elements: &[T],
    each: impl Fn(&T) -> usize,
    make: &dyn Fn(u32, Vec<T>) -> OpenItemEntry,
) -> Result<()> {
    let empty_len = encoded_entry_len(&make(0, Vec::new()));
    let mut offset = 0usize;
    while offset < elements.len() {
        let room = packer.room();
        let mut take = 0usize;
        let mut used = empty_len;
        while offset + take < elements.len() {
            let n = each(&elements[offset + take]);
            if used + n > room {
                break;
            }
            used += n;
            take += 1;
        }
        if take == 0 {
            if packer.current.items.is_empty() {
                // Not even one element fits an empty row: cannot happen
                // within the bounds, refused rather than looping.
                return Err(OpenItemsError::TooLong);
            }
            packer.flush();
            continue;
        }
        let entry = make(len_u32(offset), elements[offset..offset + take].to_vec());
        packer.push(entry, used);
        offset += take;
        if offset < elements.len() {
            packer.flush();
        }
    }
    Ok(())
}

/// The committer's hand-over: the envelopes to post after an admission
/// commit merged, packing `items` in `poll_ref` order. Each envelope fits
/// [`OPEN_ITEMS_ENVELOPE_MAX_LEN`] and [`OPEN_ITEMS_MAX_ITEMS`]; a ring or
/// vote list that does not fit one envelope is split into parts, each
/// repeating the item header. Empty `items` gives no envelope.
///
/// # Errors
///
/// [`OpenItemsError::DuplicateItem`] when two items name one `poll_ref`,
/// else the first rule an item breaks ([`compose_open_items`]'s).
pub fn plan_open_items_rows(items: &[OpenItem]) -> Result<Vec<Vec<u8>>> {
    let mut sorted: Vec<&OpenItem> = items.iter().collect();
    sorted.sort_by_key(|i| i.header.poll_ref);
    if sorted
        .windows(2)
        .any(|w| w[0].header.poll_ref == w[1].header.poll_ref)
    {
        return Err(OpenItemsError::DuplicateItem);
    }
    let mut packer = Packer {
        rows: Vec::new(),
        current: OpenItems::default(),
        current_len: OPEN_ITEMS_ENVELOPE_MIN_LEN,
    };
    for item in sorted {
        let header = item.header.clone();
        match &item.content {
            OpenItemContent::Checklist { added, ticks } => {
                let mut added = added.clone();
                added.sort_by_key(|a| a.message_id);
                let mut ticks = ticks.clone();
                ticks.sort_by_key(|t| t.item);
                let entry = OpenItemEntry {
                    header,
                    part: OpenItemPart::Checklist { added, ticks },
                };
                validate_entry(&entry)?;
                let len = encoded_entry_len(&entry);
                if len > packer.room() {
                    packer.flush();
                }
                if len > packer.room() {
                    return Err(OpenItemsError::TooLong);
                }
                packer.push(entry, len);
            },
            OpenItemContent::Poll { votes } => {
                let mut votes = votes.clone();
                votes.sort_by_key(|v| v.voter);
                let total = len_u32(votes.len());
                let make = |offset: u32, slice: Vec<HandoverVote>| OpenItemEntry {
                    header: header.clone(),
                    part: OpenItemPart::Poll {
                        vote_total: total,
                        vote_offset: offset,
                        votes: slice,
                    },
                };
                if votes.is_empty() {
                    let entry = make(0, Vec::new());
                    validate_entry(&entry)?;
                    let len = encoded_entry_len(&entry);
                    if len > packer.room() {
                        packer.flush();
                    }
                    packer.push(entry, len);
                } else {
                    validate_entry(&make(0, votes.clone()))?;
                    pack_split(&mut packer, &votes, vote_len, &make)?;
                }
            },
            OpenItemContent::AnonymousPoll { keys, lengths } => {
                let total = len_u32(keys.len());
                let make = |offset: u32, slice: Vec<[u8; KEY_LEN]>| OpenItemEntry {
                    header: header.clone(),
                    part: OpenItemPart::AnonymousPoll {
                        ring_total: total,
                        lengths: lengths.clone(),
                        key_offset: offset,
                        keys: slice,
                    },
                };
                validate_entry(&make(0, keys.clone()))?;
                pack_split(&mut packer, keys, |_| KEY_LEN, &make)?;
            },
        }
    }
    packer.flush();
    packer
        .rows
        .iter()
        .map(compose_open_items)
        .collect::<Result<Vec<_>>>()
}

// =============================================================================
// The receiver's side
// =============================================================================

/// Whether to take a hand-over from `sender_account_key` (the account its
/// MLS sender resolves to at receipt, C-68). ADR-0090 section 3.9 rests the
/// ring extensions and earlier votes and ticks on the committer's word, and
/// the committer is the account that committed this device's admission (the
/// sender of its Welcome, `admitting_account_key`) or an owner or admin of
/// the live role table. A plain member that did not admit this device is not
/// taken: it could otherwise add keys to this device's ring.
#[must_use]
pub fn open_items_sender_trusted(
    sender_account_key: &[u8; KEY_LEN],
    admitting_account_key: Option<&[u8; KEY_LEN]>,
    sender_is_owner_or_admin: bool,
) -> bool {
    sender_is_owner_or_admin || admitting_account_key == Some(sender_account_key)
}

/// One received envelope, from a sender the caller trusted.
#[derive(Clone, Copy, Debug)]
pub struct ReceivedOpenItems<'a> {
    /// The sender's account key, resolved at receipt.
    pub sender_account_key: [u8; KEY_LEN],
    /// The decoded envelope.
    pub items: &'a OpenItems,
}

/// A whole item, put back together from its parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssembledContent {
    /// An anonymous poll's ring, encoded in `lakko_ballot::Ring::encode`'s
    /// layout (`ring_count(u32 BE) || keys || version_count(u16 BE) ||
    /// lengths`), for `Ring::decode`.
    AnonymousPoll {
        /// The encoded ring.
        ring: Vec<u8>,
    },
    /// A non-anonymous poll's votes, strictly ascending by voter.
    Poll {
        /// The votes.
        votes: Vec<HandoverVote>,
    },
    /// A checklist's added items and ticks.
    Checklist {
        /// Added items.
        added: Vec<HandoverAdded>,
        /// Ticks.
        ticks: Vec<HandoverTick>,
    },
}

/// A whole item a newcomer can check and adopt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledItem {
    /// Which item and its signed body.
    pub header: OpenItemHeader,
    /// Who handed it over.
    pub sender_account_key: [u8; KEY_LEN],
    /// Its content.
    pub content: AssembledContent,
}

/// Parts of one item from one sender that agree on everything but their
/// slice.
struct Bucket {
    header: OpenItemHeader,
    sender: [u8; KEY_LEN],
    kind: u8,
    total: usize,
    lengths: Vec<u32>,
    keys: BTreeMap<usize, Vec<[u8; KEY_LEN]>>,
    votes: BTreeMap<usize, Vec<HandoverVote>>,
    poisoned: bool,
}

impl Bucket {
    fn same(&self, sender: &[u8; KEY_LEN], entry: &OpenItemEntry) -> bool {
        if self.sender != *sender
            || self.header != entry.header
            || self.kind != kind_of(&entry.part)
        {
            return false;
        }
        match &entry.part {
            OpenItemPart::AnonymousPoll {
                ring_total,
                lengths,
                ..
            } => self.total == *ring_total as usize && self.lengths == *lengths,
            OpenItemPart::Poll { vote_total, .. } => self.total == *vote_total as usize,
            OpenItemPart::Checklist { .. } => true,
        }
    }

    /// Add a slice; an identical repeat is ignored, any other overlap
    /// poisons the bucket.
    fn add<T: Clone + PartialEq>(
        map: &mut BTreeMap<usize, Vec<T>>,
        offset: usize,
        slice: &[T],
    ) -> bool {
        if let Some(existing) = map.get(&offset) {
            return existing.as_slice() == slice;
        }
        let end = offset + slice.len();
        let overlaps = map
            .range(..end)
            .next_back()
            .is_some_and(|(&o, s)| o + s.len() > offset);
        if overlaps {
            return false;
        }
        map.insert(offset, slice.to_vec());
        true
    }

    fn tiles<T>(map: &BTreeMap<usize, Vec<T>>, total: usize) -> bool {
        let mut next = 0usize;
        for (&o, s) in map {
            if o != next {
                return false;
            }
            next += s.len();
        }
        next == total
    }

    fn finish(&self) -> Option<AssembledContent> {
        if self.poisoned {
            return None;
        }
        match self.kind {
            KIND_ANONYMOUS_POLL if Self::tiles(&self.keys, self.total) => {
                let mut ring =
                    Vec::with_capacity(6 + self.total * KEY_LEN + self.lengths.len() * 4);
                ring.extend_from_slice(&len_u32(self.total).to_be_bytes());
                for k in self.keys.values().flatten() {
                    ring.extend_from_slice(k);
                }
                ring.extend_from_slice(
                    &u16::try_from(self.lengths.len())
                        .unwrap_or(u16::MAX)
                        .to_be_bytes(),
                );
                for l in &self.lengths {
                    ring.extend_from_slice(&l.to_be_bytes());
                }
                Some(AssembledContent::AnonymousPoll { ring })
            },
            KIND_POLL if Self::tiles(&self.votes, self.total) => {
                let votes: Vec<HandoverVote> = self.votes.values().flatten().cloned().collect();
                if votes.windows(2).all(|w| w[0].voter < w[1].voter) {
                    Some(AssembledContent::Poll { votes })
                } else {
                    None
                }
            },
            _ => None,
        }
    }
}

/// Put received parts back together (the newcomer's side of ADR-0090
/// section 3.9). `received` is every hand-over envelope the caller accepted
/// through [`open_items_sender_trusted`], in journal order; `held` the
/// `poll_ref`s this device already holds, which are skipped (a member that
/// holds an item ignores a hand-over of it).
///
/// Parts are grouped per item and sender, and only parts that agree on the
/// header, the signed body, the totals and the version lengths are joined. A
/// group whose slices overlap with different content is dropped (a sender
/// contradicting itself). For each item the first group to become whole, in
/// the order of `received`, is returned; a group still missing a slice is
/// not returned. The result is a pure function of the arguments.
///
/// The bodies, signatures and ring are not checked here: that is
/// `lakko-open-items`' adoption.
#[must_use]
pub fn assemble_open_items(
    received: &[ReceivedOpenItems<'_>],
    held: &BTreeSet<i64>,
) -> Vec<AssembledItem> {
    let mut buckets: BTreeMap<i64, Vec<Bucket>> = BTreeMap::new();
    let mut done: BTreeSet<i64> = BTreeSet::new();
    let mut out = Vec::new();
    for r in received {
        for entry in &r.items.items {
            let poll_ref = entry.header.poll_ref;
            if held.contains(&poll_ref) || done.contains(&poll_ref) {
                continue;
            }
            if let OpenItemPart::Checklist { added, ticks } = &entry.part {
                done.insert(poll_ref);
                out.push(AssembledItem {
                    header: entry.header.clone(),
                    sender_account_key: r.sender_account_key,
                    content: AssembledContent::Checklist {
                        added: added.clone(),
                        ticks: ticks.clone(),
                    },
                });
                continue;
            }
            let list = buckets.entry(poll_ref).or_default();
            let index = if let Some(i) = list
                .iter()
                .position(|b| b.same(&r.sender_account_key, entry))
            {
                i
            } else {
                let (total, lengths) = match &entry.part {
                    OpenItemPart::AnonymousPoll {
                        ring_total,
                        lengths,
                        ..
                    } => (*ring_total as usize, lengths.clone()),
                    OpenItemPart::Poll { vote_total, .. } => (*vote_total as usize, Vec::new()),
                    OpenItemPart::Checklist { .. } => (0, Vec::new()),
                };
                list.push(Bucket {
                    header: entry.header.clone(),
                    sender: r.sender_account_key,
                    kind: kind_of(&entry.part),
                    total,
                    lengths,
                    keys: BTreeMap::new(),
                    votes: BTreeMap::new(),
                    poisoned: false,
                });
                list.len() - 1
            };
            let bucket = &mut list[index];
            let fits = match &entry.part {
                OpenItemPart::AnonymousPoll {
                    key_offset, keys, ..
                } => Bucket::add(&mut bucket.keys, *key_offset as usize, keys),
                OpenItemPart::Poll {
                    vote_offset, votes, ..
                } => Bucket::add(&mut bucket.votes, *vote_offset as usize, votes),
                OpenItemPart::Checklist { .. } => true,
            };
            if !fits {
                bucket.poisoned = true;
            }
            if let Some(content) = bucket.finish() {
                done.insert(poll_ref);
                out.push(AssembledItem {
                    header: bucket.header.clone(),
                    sender_account_key: bucket.sender,
                    content,
                });
                buckets.remove(&poll_ref);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
