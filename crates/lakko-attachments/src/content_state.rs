// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A content message as it travels outside the journal (ADR-0090 section
//! 3.11, "History bundles and the vault"): the content payload and a
//! snapshot of a poll's or a checklist's folded state as the saving device
//! saw it.
//!
//! The history placeholder (`lakko_mls::history_placeholder`) and the vault
//! record (`lakko_vault::record`) carry both as opaque, length-bounded bytes;
//! the FFI decodes them here, with the same decoders the envelope route uses,
//! so the two routes cannot disagree.
//!
//! ## Payload
//!
//! The JSON value of the envelope's content field (`poll`, `checklist`,
//! `location`, `contact_card`, `article`) serialised as JSON: exactly what
//! the envelope carried, read back by the envelope's own decoders. For an
//! edited checklist or article it is the latest replacement the device
//! shows. The article's images are the message's attachments.
//!
//! ## State
//!
//! ```text
//! state     := version(0x01) || kind u8 (1 poll, 2 checklist) || body
//! poll      := option_count u8 (2..=10) || count u32 BE * option_count
//!              || voters u32 BE || closed u8 (0 open, 1 by its time, 2 by an op)
//!              || [closed = 2: close_message_id i64 BE]
//! checklist := closed u8 (0 | 1) || [closed = 1: close_message_id i64 BE]
//!              || added_count u8 (<= 30)
//!              || (message_id i64 BE || text_len u8 || text)*      ascending ids
//!              || done_count u8 (<= 30)
//!              || (ref_tag u8 (0 author, 1 added) || id (u32 | i64 BE)
//!                  || done_by(32))*                                ascending refs
//! ```
//!
//! Strict and canonical: no count above the voter count, added item text
//! under the item rule, no repeated or unordered entry, no trailing byte.
//! Checked against its content with [`ContentState::matches`].

use crate::checklist::{
    ChecklistItemRef, ChecklistState, MAX_CHECKLIST_ITEMS, MAX_CHECKLIST_ITEM_BYTES,
};
use crate::content::{check_display_text, push_len_u8, ContentError, Reader, ACCOUNT_KEY_LEN};
use crate::poll::{PollTally, MAX_POLL_OPTIONS, MIN_POLL_OPTIONS};
use crate::{AttachmentRef, Content};

/// Wire byte of a poll payload.
pub const CONTENT_KIND_POLL: u8 = 1;
/// Wire byte of a checklist payload.
pub const CONTENT_KIND_CHECKLIST: u8 = 2;
/// Wire byte of a location payload.
pub const CONTENT_KIND_LOCATION: u8 = 3;
/// Wire byte of a contact-card payload.
pub const CONTENT_KIND_CONTACT_CARD: u8 = 4;
/// Wire byte of an article payload.
pub const CONTENT_KIND_ARTICLE: u8 = 5;

/// Largest content payload, bytes (chosen: above the largest article, whose
/// base64 JSON string is at most 174,766 bytes).
pub const MAX_CONTENT_PAYLOAD_BYTES: usize = 196_608;
/// Largest state snapshot, bytes (chosen; a full checklist snapshot is about
/// 4.5 KiB).
pub const MAX_CONTENT_STATE_BYTES: usize = 8_192;

/// The only state version this build reads.
pub const CONTENT_STATE_VERSION: u8 = 0x01;

impl Content {
    /// The wire byte of this content's kind.
    #[must_use]
    pub const fn kind_byte(&self) -> u8 {
        match self {
            Self::Poll(_) => CONTENT_KIND_POLL,
            Self::Checklist(_) => CONTENT_KIND_CHECKLIST,
            Self::Location(_) => CONTENT_KIND_LOCATION,
            Self::ContactCard(_) => CONTENT_KIND_CONTACT_CARD,
            Self::Article(_) => CONTENT_KIND_ARTICLE,
        }
    }

    /// The payload a history bundle or the vault carries: the JSON value of
    /// the envelope's content field, serialised.
    ///
    /// # Errors
    ///
    /// What the content's encoder refuses, or [`ContentError::TooLarge`].
    pub fn to_payload(&self) -> Result<Vec<u8>, ContentError> {
        let value = crate::content_field_value(self)?;
        let bytes =
            serde_json::to_vec(&value).map_err(|_| ContentError::Malformed { what: "payload" })?;
        if bytes.len() > MAX_CONTENT_PAYLOAD_BYTES {
            return Err(ContentError::TooLarge);
        }
        Ok(bytes)
    }

    /// Read a payload back with the envelope's own decoders. `attachments`
    /// are the message's attachments (an article's images point at them).
    ///
    /// # Errors
    ///
    /// The decoder's refusal, [`ContentError::TooLarge`], or
    /// [`ContentError::Malformed`] for an unknown kind or invalid JSON.
    pub fn from_payload(
        kind: u8,
        payload: &[u8],
        attachments: &[AttachmentRef],
    ) -> Result<Self, ContentError> {
        if payload.len() > MAX_CONTENT_PAYLOAD_BYTES {
            return Err(ContentError::TooLarge);
        }
        let value: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|_| ContentError::Malformed { what: "payload" })?;
        crate::content_from_field_value(kind, value, attachments)
    }
}

/// How a poll stood when it was saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollClosed {
    /// Open.
    Open,
    /// Closed by its automatic close time.
    ByTime,
    /// Closed by the close op with this DS `messageId`.
    ByOp(i64),
}

/// A poll's folded state as the saving device saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollState {
    /// Votes per option.
    pub counts: Vec<u32>,
    /// Voters with a counted vote.
    pub voters: u32,
    /// Open or how it closed.
    pub closed: PollClosed,
}

impl PollState {
    /// The snapshot of a non-anonymous poll's tally at `now_ms`.
    #[must_use]
    pub fn from_tally(tally: &PollTally, now_ms: i64) -> Self {
        let closed = match tally.closed_by {
            Some(id) => PollClosed::ByOp(id),
            None if tally.is_closed(now_ms) => PollClosed::ByTime,
            None => PollClosed::Open,
        };
        Self {
            counts: tally.counts.clone(),
            voters: tally.voters,
            closed,
        }
    }
}

/// An item someone added to a checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedItemSnapshot {
    /// DS `messageId` of the add row (the item's id).
    pub message_id: i64,
    /// The item text.
    pub text: String,
}

/// A checklist's folded state as the saving device saw it. Title, flags and
/// the author's items come from the payload (the latest edit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistSnapshot {
    /// DS `messageId` of the author's close, if closed.
    pub closed_by: Option<i64>,
    /// Added items, ascending by id.
    pub added: Vec<AddedItemSnapshot>,
    /// Done items with who marked them, ascending by item.
    pub done: Vec<(ChecklistItemRef, [u8; ACCOUNT_KEY_LEN])>,
}

impl ChecklistSnapshot {
    /// The snapshot of a folded checklist.
    #[must_use]
    pub fn from_state(state: &ChecklistState) -> Self {
        let mut added: Vec<AddedItemSnapshot> = state
            .items
            .iter()
            .filter_map(|i| match i.item {
                ChecklistItemRef::Added(m) => Some(AddedItemSnapshot {
                    message_id: m,
                    text: i.text.clone(),
                }),
                ChecklistItemRef::Author(_) => None,
            })
            .collect();
        added.sort_by_key(|a| a.message_id);
        let mut done: Vec<(ChecklistItemRef, [u8; ACCOUNT_KEY_LEN])> = state
            .items
            .iter()
            .filter_map(|i| i.done_by.map(|k| (i.item, k)))
            .collect();
        done.sort_by_key(|a| a.0);
        Self {
            closed_by: state.closed_by,
            added,
            done,
        }
    }
}

/// A saved poll's or checklist's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentState {
    /// A poll.
    Poll(PollState),
    /// A checklist.
    Checklist(ChecklistSnapshot),
}

const STATE_POLL: u8 = 1;
const STATE_CHECKLIST: u8 = 2;

impl ContentState {
    /// Check every bound and canonical rule.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        match self {
            Self::Poll(p) => {
                if !(MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS).contains(&p.counts.len()) {
                    return Err(ContentError::OutOfRange {
                        field: "optionCount",
                    });
                }
                if p.counts.iter().any(|&c| c > p.voters) {
                    return Err(ContentError::OutOfRange { field: "counts" });
                }
            },
            Self::Checklist(c) => {
                if c.added.len() > MAX_CHECKLIST_ITEMS || c.done.len() > MAX_CHECKLIST_ITEMS {
                    return Err(ContentError::OutOfRange { field: "itemCount" });
                }
                for a in &c.added {
                    check_display_text(&a.text, 1, MAX_CHECKLIST_ITEM_BYTES, "item")?;
                }
                if c.added
                    .windows(2)
                    .any(|w| w[0].message_id >= w[1].message_id)
                {
                    return Err(ContentError::NotCanonical { what: "added" });
                }
                if c.done.windows(2).any(|w| w[0].0 >= w[1].0) {
                    return Err(ContentError::NotCanonical { what: "done" });
                }
            },
        }
        Ok(())
    }

    /// Whether this state belongs to `content`: a poll state with as many
    /// counts as the poll has options (and a time close only for a poll
    /// with a close time); a checklist state whose items fit
    /// [`MAX_CHECKLIST_ITEMS`] with the author's and whose done items exist.
    #[must_use]
    pub fn matches(&self, content: &Content) -> bool {
        match (self, content) {
            (Self::Poll(s), Content::Poll(p)) => {
                s.counts.len() == p.body.options.len()
                    && (s.closed != PollClosed::ByTime || p.body.close_at_ms.is_some())
            },
            (Self::Checklist(s), Content::Checklist(c)) => {
                c.body.items.len() + s.added.len() <= MAX_CHECKLIST_ITEMS
                    && s.done.iter().all(|(item, _)| match item {
                        ChecklistItemRef::Author(id) => c.body.items.iter().any(|i| i.id == *id),
                        ChecklistItemRef::Added(m) => s.added.iter().any(|a| a.message_id == *m),
                    })
            },
            _ => false,
        }
    }

    /// The canonical encoding.
    ///
    /// # Errors
    ///
    /// What [`Self::validate`] refuses, or [`ContentError::TooLarge`].
    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        self.validate()?;
        let mut out = vec![CONTENT_STATE_VERSION];
        match self {
            Self::Poll(p) => {
                out.push(STATE_POLL);
                push_len_u8(&mut out, p.counts.len(), "optionCount")?;
                for c in &p.counts {
                    out.extend_from_slice(&c.to_be_bytes());
                }
                out.extend_from_slice(&p.voters.to_be_bytes());
                match p.closed {
                    PollClosed::Open => out.push(0),
                    PollClosed::ByTime => out.push(1),
                    PollClosed::ByOp(id) => {
                        out.push(2);
                        out.extend_from_slice(&id.to_be_bytes());
                    },
                }
            },
            Self::Checklist(c) => {
                out.push(STATE_CHECKLIST);
                match c.closed_by {
                    None => out.push(0),
                    Some(id) => {
                        out.push(1);
                        out.extend_from_slice(&id.to_be_bytes());
                    },
                }
                push_len_u8(&mut out, c.added.len(), "itemCount")?;
                for a in &c.added {
                    out.extend_from_slice(&a.message_id.to_be_bytes());
                    push_len_u8(&mut out, a.text.len(), "item")?;
                    out.extend_from_slice(a.text.as_bytes());
                }
                push_len_u8(&mut out, c.done.len(), "itemCount")?;
                for (item, by) in &c.done {
                    match item {
                        ChecklistItemRef::Author(id) => {
                            out.push(0);
                            out.extend_from_slice(&id.to_be_bytes());
                        },
                        ChecklistItemRef::Added(m) => {
                            out.push(1);
                            out.extend_from_slice(&m.to_be_bytes());
                        },
                    }
                    out.extend_from_slice(by);
                }
            },
        }
        if out.len() > MAX_CONTENT_STATE_BYTES {
            return Err(ContentError::TooLarge);
        }
        Ok(out)
    }

    /// Decode a canonical state.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.len() > MAX_CONTENT_STATE_BYTES {
            return Err(ContentError::TooLarge);
        }
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != CONTENT_STATE_VERSION {
            return Err(ContentError::UnknownVersion(version));
        }
        let state = match r.u8()? {
            STATE_POLL => {
                let n = usize::from(r.u8()?);
                if !(MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS).contains(&n) {
                    return Err(ContentError::OutOfRange {
                        field: "optionCount",
                    });
                }
                let mut counts = Vec::with_capacity(n);
                for _ in 0..n {
                    counts.push(r.u32()?);
                }
                let voters = r.u32()?;
                let closed = match r.u8()? {
                    0 => PollClosed::Open,
                    1 => PollClosed::ByTime,
                    2 => PollClosed::ByOp(r.i64()?),
                    _ => return Err(ContentError::OutOfRange { field: "closed" }),
                };
                Self::Poll(PollState {
                    counts,
                    voters,
                    closed,
                })
            },
            STATE_CHECKLIST => {
                let closed_by = match r.u8()? {
                    0 => None,
                    1 => Some(r.i64()?),
                    _ => return Err(ContentError::OutOfRange { field: "closed" }),
                };
                let n = usize::from(r.u8()?);
                if n > MAX_CHECKLIST_ITEMS {
                    return Err(ContentError::OutOfRange { field: "itemCount" });
                }
                let mut added = Vec::with_capacity(n);
                for _ in 0..n {
                    let message_id = r.i64()?;
                    let len = usize::from(r.u8()?);
                    added.push(AddedItemSnapshot {
                        message_id,
                        text: r.str(len, "item")?.to_owned(),
                    });
                }
                let n = usize::from(r.u8()?);
                if n > MAX_CHECKLIST_ITEMS {
                    return Err(ContentError::OutOfRange { field: "itemCount" });
                }
                let mut done = Vec::with_capacity(n);
                for _ in 0..n {
                    let item = match r.u8()? {
                        0 => ChecklistItemRef::Author(r.u32()?),
                        1 => ChecklistItemRef::Added(r.i64()?),
                        _ => return Err(ContentError::OutOfRange { field: "itemRef" }),
                    };
                    done.push((item, r.array::<ACCOUNT_KEY_LEN>()?));
                }
                Self::Checklist(ChecklistSnapshot {
                    closed_by,
                    added,
                    done,
                })
            },
            _ => return Err(ContentError::Malformed { what: "stateKind" }),
        };
        r.finish()?;
        state.validate()?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn any_ref() -> impl Strategy<Value = ChecklistItemRef> {
        prop_oneof![
            any::<u32>().prop_map(ChecklistItemRef::Author),
            any::<i64>().prop_map(ChecklistItemRef::Added),
        ]
    }

    fn any_state() -> impl Strategy<Value = ContentState> {
        let poll = (
            proptest::collection::vec(0u32..1000, MIN_POLL_OPTIONS..=MAX_POLL_OPTIONS),
            0u32..10,
            prop_oneof![
                Just(PollClosed::Open),
                Just(PollClosed::ByTime),
                any::<i64>().prop_map(PollClosed::ByOp)
            ],
        )
            .prop_map(|(counts, extra, closed)| {
                let voters = counts.iter().copied().max().unwrap_or(0) + extra;
                ContentState::Poll(PollState {
                    counts,
                    voters,
                    closed,
                })
            });
        let checklist = (
            proptest::option::of(any::<i64>()),
            proptest::collection::btree_map(any::<i64>(), "[a-z ]{1,20}", 0..=MAX_CHECKLIST_ITEMS),
            proptest::collection::btree_map(any_ref(), any::<[u8; 32]>(), 0..=MAX_CHECKLIST_ITEMS),
        )
            .prop_map(|(closed_by, added, done)| {
                ContentState::Checklist(ChecklistSnapshot {
                    closed_by,
                    added: added
                        .into_iter()
                        .map(|(message_id, text)| AddedItemSnapshot { message_id, text })
                        .collect(),
                    done: done.into_iter().collect(),
                })
            });
        prop_oneof![poll, checklist]
    }

    proptest! {
        #[test]
        fn round_trip_and_canonical(s in any_state()) {
            let bytes = s.encode().unwrap();
            prop_assert!(bytes.len() <= MAX_CONTENT_STATE_BYTES);
            let back = ContentState::decode(&bytes).unwrap();
            prop_assert_eq!(&back, &s);
            prop_assert_eq!(back.encode().unwrap(), bytes);
        }

        #[test]
        fn hostile_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            if let Ok(s) = ContentState::decode(&bytes) {
                prop_assert_eq!(s.encode().unwrap(), bytes);
            }
        }
    }

    #[test]
    fn refusals() {
        let over = ContentState::Poll(PollState {
            counts: vec![3, 1],
            voters: 2,
            closed: PollClosed::Open,
        });
        assert!(over.encode().is_err(), "a count above the voters");
        let unordered = ContentState::Checklist(ChecklistSnapshot {
            closed_by: None,
            added: vec![
                AddedItemSnapshot {
                    message_id: 5,
                    text: "a".into(),
                },
                AddedItemSnapshot {
                    message_id: 4,
                    text: "b".into(),
                },
            ],
            done: vec![],
        });
        assert!(unordered.encode().is_err());
        assert_eq!(
            ContentState::decode(&[2, 1]),
            Err(ContentError::UnknownVersion(2))
        );
        assert!(ContentState::decode(&[1, 3]).is_err());
    }
}
