// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Checklists (ADR-0090 section 5): the canonical body, the author signature
//! over it, and the fold of edits, ticks, added items and closes.
//!
//! ```text
//! checklist_body = "lakko/checklist/v1\0" || 0x01 || flags(u8: bit0 others_add,
//!                  bit1 others_mark) || title_len(u8) || title
//!                  || item_count(u8: 1..=30) || (item_id(u32 BE) || len(u8) || text)*
//! author_sig     = Ed25519 by the author's account key over
//!                  "lakko/checklist/author-sig/v1\0" || group_id || checklist_body
//! ```
//!
//! An item the author wrote is named by its `item_id`; an item someone added
//! later is named by the DS `messageId` of its add row. [`ChecklistItemRef`]
//! keeps the two apart, so an author id can never collide with a message id.

use std::collections::{BTreeMap, BTreeSet};

use crate::content::{
    check_display_text, push_len_u8, sign_author, verify_author, ContentError, Reader,
    ACCOUNT_KEY_LEN, AUTHOR_SIG_LEN, GROUP_ID_LEN,
};

/// Domain separator that opens a checklist body. Frozen.
pub const CHECKLIST_DOMAIN: &[u8] = b"lakko/checklist/v1\0";
/// The only checklist body version this build reads.
pub const CHECKLIST_VERSION: u8 = 0x01;
/// Domain of the author signature. Frozen.
pub const CHECKLIST_AUTHOR_SIG_DOMAIN: &[u8] = b"lakko/checklist/author-sig/v1\0";
/// Longest title, in bytes (at least 1).
pub const MAX_CHECKLIST_TITLE_BYTES: usize = 255;
/// Most items: the author's, and in the fold the author's plus added ones.
pub const MAX_CHECKLIST_ITEMS: usize = 30;
/// Longest item text, in bytes (at least 1).
pub const MAX_CHECKLIST_ITEM_BYTES: usize = 100;

const FLAG_OTHERS_ADD: u8 = 0x01;
const FLAG_OTHERS_MARK: u8 = 0x02;
const KNOWN_FLAGS: u8 = FLAG_OTHERS_ADD | FLAG_OTHERS_MARK;

/// The largest encoded checklist body.
pub const MAX_CHECKLIST_BODY_LEN: usize = CHECKLIST_DOMAIN.len()
    + 1
    + 1
    + 1
    + MAX_CHECKLIST_TITLE_BYTES
    + 1
    + MAX_CHECKLIST_ITEMS * (4 + 1 + MAX_CHECKLIST_ITEM_BYTES);

/// One item the author wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistItem {
    /// Unique within the checklist.
    pub id: u32,
    /// 1..=[`MAX_CHECKLIST_ITEM_BYTES`] bytes of display text.
    pub text: String,
}

/// A checklist body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistBody {
    /// 1..=[`MAX_CHECKLIST_TITLE_BYTES`] bytes of display text.
    pub title: String,
    /// Other members may add items.
    pub others_add: bool,
    /// Other members may mark items done.
    pub others_mark: bool,
    /// 1..=[`MAX_CHECKLIST_ITEMS`] items, ids unique, in display order.
    pub items: Vec<ChecklistItem>,
}

impl ChecklistBody {
    /// Check every bound the encoding needs.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        check_display_text(&self.title, 1, MAX_CHECKLIST_TITLE_BYTES, "title")?;
        if !(1..=MAX_CHECKLIST_ITEMS).contains(&self.items.len()) {
            return Err(ContentError::OutOfRange { field: "itemCount" });
        }
        let mut seen = BTreeSet::new();
        for item in &self.items {
            check_display_text(&item.text, 1, MAX_CHECKLIST_ITEM_BYTES, "item")?;
            if !seen.insert(item.id) {
                return Err(ContentError::NotCanonical { what: "itemId" });
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
        let mut out = Vec::with_capacity(MAX_CHECKLIST_BODY_LEN);
        out.extend_from_slice(CHECKLIST_DOMAIN);
        out.push(CHECKLIST_VERSION);
        let mut flags = 0u8;
        if self.others_add {
            flags |= FLAG_OTHERS_ADD;
        }
        if self.others_mark {
            flags |= FLAG_OTHERS_MARK;
        }
        out.push(flags);
        push_len_u8(&mut out, self.title.len(), "title")?;
        out.extend_from_slice(self.title.as_bytes());
        push_len_u8(&mut out, self.items.len(), "itemCount")?;
        for item in &self.items {
            out.extend_from_slice(&item.id.to_be_bytes());
            push_len_u8(&mut out, item.text.len(), "item")?;
            out.extend_from_slice(item.text.as_bytes());
        }
        Ok(out)
    }

    /// Decode a canonical checklist body.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.len() > MAX_CHECKLIST_BODY_LEN {
            return Err(ContentError::TooLarge);
        }
        let mut r = Reader::new(bytes);
        if r.take(CHECKLIST_DOMAIN.len())? != CHECKLIST_DOMAIN {
            return Err(ContentError::WrongDomain);
        }
        let version = r.u8()?;
        if version != CHECKLIST_VERSION {
            return Err(ContentError::UnknownVersion(version));
        }
        let flags = r.u8()?;
        if flags & !KNOWN_FLAGS != 0 {
            return Err(ContentError::UnknownFlags);
        }
        let title_len = usize::from(r.u8()?);
        let title = r.str(title_len, "title")?.to_owned();
        let count = usize::from(r.u8()?);
        if !(1..=MAX_CHECKLIST_ITEMS).contains(&count) {
            return Err(ContentError::OutOfRange { field: "itemCount" });
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let id = r.u32()?;
            let len = usize::from(r.u8()?);
            items.push(ChecklistItem {
                id,
                text: r.str(len, "item")?.to_owned(),
            });
        }
        r.finish()?;
        let body = Self {
            title,
            others_add: flags & FLAG_OTHERS_ADD != 0,
            others_mark: flags & FLAG_OTHERS_MARK != 0,
            items,
        };
        body.validate()?;
        Ok(body)
    }
}

/// The checklist as it rides in the envelope. The signature is absent only
/// in a channel post (the post signature authenticates the publisher).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedChecklist {
    /// The checklist.
    pub body: ChecklistBody,
    /// Ed25519 by the author's account key; `None` in a channel post.
    pub author_sig: Option<[u8; AUTHOR_SIG_LEN]>,
}

impl SignedChecklist {
    /// Sign `body` for `group_id` with the author's account seed.
    ///
    /// # Errors
    ///
    /// What [`ChecklistBody::encode`] refuses.
    pub fn sign(
        body: ChecklistBody,
        group_id: &[u8; GROUP_ID_LEN],
        author_seed: &[u8; 32],
    ) -> Result<Self, ContentError> {
        let bytes = body.encode()?;
        let sig = sign_author(CHECKLIST_AUTHOR_SIG_DOMAIN, group_id, &bytes, author_seed);
        Ok(Self {
            body,
            author_sig: Some(sig),
        })
    }

    /// A channel checklist: no author signature.
    #[must_use]
    pub const fn unsigned(body: ChecklistBody) -> Self {
        Self {
            body,
            author_sig: None,
        }
    }

    /// Check the author signature.
    ///
    /// # Errors
    ///
    /// [`ContentError::SignatureMissing`], [`ContentError::SignatureInvalid`],
    /// or what [`ChecklistBody::encode`] refuses.
    pub fn verify(
        &self,
        group_id: &[u8; GROUP_ID_LEN],
        author_key: &[u8; ACCOUNT_KEY_LEN],
    ) -> Result<(), ContentError> {
        let sig = self.author_sig.ok_or(ContentError::SignatureMissing)?;
        let bytes = self.body.encode()?;
        verify_author(
            CHECKLIST_AUTHOR_SIG_DOMAIN,
            group_id,
            &bytes,
            author_key,
            &sig,
        )
    }
}

/// Which item a tick names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChecklistItemRef {
    /// An item of the author's body, by its `item_id`.
    Author(u32),
    /// An item someone added, by the DS `messageId` of its add row.
    Added(i64),
}

/// An edit of the checklist (`edit_of` with a `checklist` payload), stored at
/// receipt after its author signature verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistEditRecord {
    /// DS `messageId` of the edit row.
    pub message_id: i64,
    /// The editor's account key, resolved at receipt (C-68).
    pub editor: [u8; ACCOUNT_KEY_LEN],
    /// The replacement body.
    pub body: ChecklistBody,
}

/// A tick op, stored at receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistTickRecord {
    /// DS `messageId` of the tick row.
    pub message_id: i64,
    /// The ticker's account key, resolved at receipt (C-68).
    pub ticker: [u8; ACCOUNT_KEY_LEN],
    /// `true` for a channel publisher, who may always tick (ADR-0090
    /// section 5); `false` in a group or one-to-one.
    pub privileged: bool,
    /// Which item.
    pub item: ChecklistItemRef,
    /// Done or not.
    pub done: bool,
}

/// An add-item op, stored at receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistAddRecord {
    /// DS `messageId` of the add row; the added item's id.
    pub message_id: i64,
    /// The adder's account key, resolved at receipt (C-68).
    pub adder: [u8; ACCOUNT_KEY_LEN],
    /// `true` for a channel publisher, who may always add.
    pub privileged: bool,
    /// The item text.
    pub text: String,
}

/// A close op (`poll_close_of` aimed at a checklist), stored at receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistCloseRecord {
    /// DS `messageId` of the close row.
    pub message_id: i64,
    /// The closer's account key, resolved at receipt (C-68).
    pub closer: [u8; ACCOUNT_KEY_LEN],
}

/// One item of the folded checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistStateItem {
    /// Which item.
    pub item: ChecklistItemRef,
    /// Its text.
    pub text: String,
    /// Who added it, for an added item.
    pub added_by: Option<[u8; ACCOUNT_KEY_LEN]>,
    /// Who marked it done, when done.
    pub done_by: Option<[u8; ACCOUNT_KEY_LEN]>,
}

/// The folded state of a checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistState {
    /// Current title.
    pub title: String,
    /// Current "others can add" flag.
    pub others_add: bool,
    /// Current "others can mark" flag.
    pub others_mark: bool,
    /// The author's items in the current order, then added items in DS order.
    pub items: Vec<ChecklistStateItem>,
    /// DS `messageId` of the author's close, if any. Final.
    pub closed_by: Option<i64>,
}

impl ChecklistState {
    /// How many items are done.
    #[must_use]
    pub fn done_count(&self) -> usize {
        self.items.iter().filter(|i| i.done_by.is_some()).count()
    }
}

enum Event<'a> {
    Edit(&'a ChecklistEditRecord),
    Add(&'a ChecklistAddRecord),
    Tick(&'a ChecklistTickRecord),
}

impl Event<'_> {
    const fn rank(&self) -> u8 {
        match self {
            Self::Edit(_) => 0,
            Self::Add(_) => 1,
            Self::Tick(_) => 2,
        }
    }
    const fn message_id(&self) -> i64 {
        match self {
            Self::Edit(e) => e.message_id,
            Self::Add(a) => a.message_id,
            Self::Tick(t) => t.message_id,
        }
    }
}

/// Fold a checklist (ADR-0090 section 5) in DS `messageId` order:
///
/// - only the author edits; an edit replaces title, flags, item texts and
///   order, ticks stay with item ids and a tick on an item the edit removed
///   is dropped; added items stay;
/// - the author, a privileged actor (a channel publisher), or anyone while
///   `others_add` is set adds an item, up to [`MAX_CHECKLIST_ITEMS`] in all;
/// - the author, a privileged actor, or anyone while `others_mark` is set
///   ticks an existing item, last writer wins per item;
/// - the author's earliest close ends everything after it.
///
/// Only ops after the checklist's own row count. The result does not depend
/// on the order of the inputs.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn fold(
    body: &ChecklistBody,
    author: &[u8; ACCOUNT_KEY_LEN],
    checklist_message_id: i64,
    edits: &[ChecklistEditRecord],
    adds: &[ChecklistAddRecord],
    ticks: &[ChecklistTickRecord],
    closes: &[ChecklistCloseRecord],
) -> ChecklistState {
    let closed_by = closes
        .iter()
        .filter(|c| c.message_id > checklist_message_id && c.closer == *author)
        .map(|c| c.message_id)
        .min();
    let in_window = |m: i64| m > checklist_message_id && closed_by.map_or(true, |c| m < c);

    let mut events: Vec<Event<'_>> = Vec::new();
    events.extend(
        edits
            .iter()
            .filter(|e| in_window(e.message_id) && e.editor == *author && e.body.validate().is_ok())
            .map(Event::Edit),
    );
    events.extend(
        adds.iter()
            .filter(|a| {
                in_window(a.message_id)
                    && check_display_text(&a.text, 1, MAX_CHECKLIST_ITEM_BYTES, "item").is_ok()
            })
            .map(Event::Add),
    );
    events.extend(
        ticks
            .iter()
            .filter(|t| in_window(t.message_id))
            .map(Event::Tick),
    );
    // A total order so equal message ids (never honest) still fold the same.
    events.sort_by(|a, b| {
        (a.message_id(), a.rank())
            .cmp(&(b.message_id(), b.rank()))
            .then_with(|| event_bytes(a).cmp(&event_bytes(b)))
    });

    let mut current = body.clone();
    let mut added: Vec<(i64, [u8; ACCOUNT_KEY_LEN], String)> = Vec::new();
    let mut done: BTreeMap<ChecklistItemRef, [u8; ACCOUNT_KEY_LEN]> = BTreeMap::new();

    for event in events {
        match event {
            Event::Edit(e) => {
                let keep: BTreeSet<u32> = e.body.items.iter().map(|i| i.id).collect();
                done.retain(|k, _| match k {
                    ChecklistItemRef::Author(id) => keep.contains(id),
                    ChecklistItemRef::Added(_) => true,
                });
                current = e.body.clone();
                // An edit may grow the author's list past room for every
                // added item; the earliest added items are kept.
                let room = MAX_CHECKLIST_ITEMS.saturating_sub(current.items.len());
                for (m, _, _) in added.iter().skip(room) {
                    done.remove(&ChecklistItemRef::Added(*m));
                }
                added.truncate(room);
            },
            Event::Add(a) => {
                let allowed = a.adder == *author || a.privileged || current.others_add;
                if allowed && current.items.len() + added.len() < MAX_CHECKLIST_ITEMS {
                    added.push((a.message_id, a.adder, a.text.clone()));
                }
            },
            Event::Tick(t) => {
                let allowed = t.ticker == *author || t.privileged || current.others_mark;
                let exists = match t.item {
                    ChecklistItemRef::Author(id) => current.items.iter().any(|i| i.id == id),
                    ChecklistItemRef::Added(m) => added.iter().any(|(am, _, _)| *am == m),
                };
                if allowed && exists {
                    if t.done {
                        done.insert(t.item, t.ticker);
                    } else {
                        done.remove(&t.item);
                    }
                }
            },
        }
    }

    let mut items: Vec<ChecklistStateItem> = current
        .items
        .iter()
        .map(|i| {
            let item = ChecklistItemRef::Author(i.id);
            ChecklistStateItem {
                item,
                text: i.text.clone(),
                added_by: None,
                done_by: done.get(&item).copied(),
            }
        })
        .collect();
    items.extend(added.into_iter().map(|(m, adder, text)| {
        let item = ChecklistItemRef::Added(m);
        ChecklistStateItem {
            item,
            text,
            added_by: Some(adder),
            done_by: done.get(&item).copied(),
        }
    }));
    ChecklistState {
        title: current.title,
        others_add: current.others_add,
        others_mark: current.others_mark,
        items,
        closed_by,
    }
}

/// A byte key for tie-breaking events with equal message ids.
fn event_bytes(e: &Event<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    match e {
        Event::Edit(x) => {
            out.extend_from_slice(&x.editor);
            out.extend_from_slice(&x.body.encode().unwrap_or_default());
        },
        Event::Add(x) => {
            out.extend_from_slice(&x.adder);
            out.push(u8::from(x.privileged));
            out.extend_from_slice(x.text.as_bytes());
        },
        Event::Tick(x) => {
            out.extend_from_slice(&x.ticker);
            out.push(u8::from(x.privileged));
            match x.item {
                ChecklistItemRef::Author(id) => {
                    out.push(0);
                    out.extend_from_slice(&id.to_be_bytes());
                },
                ChecklistItemRef::Added(m) => {
                    out.push(1);
                    out.extend_from_slice(&m.to_be_bytes());
                },
            }
            out.push(u8::from(x.done));
        },
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn body(add: bool, mark: bool) -> ChecklistBody {
        ChecklistBody {
            title: "Trip".into(),
            others_add: add,
            others_mark: mark,
            items: vec![
                ChecklistItem {
                    id: 1,
                    text: "Tickets".into(),
                },
                ChecklistItem {
                    id: 2,
                    text: "Hotel".into(),
                },
            ],
        }
    }

    prop_compose! {
        fn any_body()(
            title in "[a-zA-Z0-9 é]{1,60}",
            add in any::<bool>(), mark in any::<bool>(),
            items in proptest::collection::btree_map(any::<u32>(), "[a-z0-9 日]{1,30}", 1..=MAX_CHECKLIST_ITEMS),
        ) -> ChecklistBody {
            ChecklistBody {
                title, others_add: add, others_mark: mark,
                items: items.into_iter().map(|(id, text)| ChecklistItem { id, text }).collect(),
            }
        }
    }

    proptest! {
        #[test]
        fn round_trip_and_canonical(b in any_body()) {
            let bytes = b.encode().unwrap();
            prop_assert!(bytes.len() <= MAX_CHECKLIST_BODY_LEN);
            let back = ChecklistBody::decode(&bytes).unwrap();
            prop_assert_eq!(back.encode().unwrap(), bytes);
            prop_assert_eq!(back, b);
        }

        #[test]
        fn hostile_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4000), prefix in any::<bool>()) {
            let mut input = if prefix { CHECKLIST_DOMAIN.to_vec() } else { Vec::new() };
            if prefix { input.push(CHECKLIST_VERSION); }
            input.extend_from_slice(&bytes);
            if let Ok(b) = ChecklistBody::decode(&input) {
                prop_assert_eq!(b.encode().unwrap(), input);
            }
        }

        #[test]
        fn fold_is_independent_of_input_order(
            ticks in proptest::collection::vec((1i64..30, 0u8..3, 1u32..4, any::<bool>()), 0..20),
            adds in proptest::collection::vec((1i64..30, 0u8..3), 0..5),
        ) {
            let author = [0u8; 32];
            let b = body(true, true);
            let mut t: Vec<ChecklistTickRecord> = ticks.iter().map(|(m, who, id, done)| ChecklistTickRecord {
                message_id: *m, ticker: [*who; 32], privileged: false, item: ChecklistItemRef::Author(*id), done: *done,
            }).collect();
            let mut a: Vec<ChecklistAddRecord> = adds.iter().map(|(m, who)| ChecklistAddRecord {
                message_id: *m, adder: [*who; 32], privileged: false, text: format!("x{m}"),
            }).collect();
            let s1 = fold(&b, &author, 0, &[], &a, &t, &[]);
            t.reverse();
            a.reverse();
            let s2 = fold(&b, &author, 0, &[], &a, &t, &[]);
            prop_assert_eq!(s1, s2);
        }
    }

    #[test]
    fn encode_refuses_duplicate_ids_and_bad_counts() {
        let mut b = body(false, false);
        b.items[1].id = 1;
        assert!(b.encode().is_err());
        let mut b = body(false, false);
        b.items.clear();
        assert!(b.encode().is_err());
        let mut b = body(false, false);
        b.title = "a\tb".into();
        assert!(b.encode().is_err());
    }

    #[test]
    fn permissions_follow_the_flags_and_the_author() {
        let author = [0u8; 32];
        let other = [1u8; 32];
        let ticks = [
            ChecklistTickRecord {
                message_id: 2,
                ticker: other,
                privileged: false,
                item: ChecklistItemRef::Author(1),
                done: true,
            },
            ChecklistTickRecord {
                message_id: 3,
                ticker: author,
                privileged: false,
                item: ChecklistItemRef::Author(2),
                done: true,
            },
        ];
        let adds = [ChecklistAddRecord {
            message_id: 4,
            adder: other,
            privileged: false,
            text: "Car".into(),
        }];
        let s = fold(&body(false, false), &author, 1, &[], &adds, &ticks, &[]);
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.items[0].done_by, None);
        assert_eq!(s.items[1].done_by, Some(author));
        let s = fold(&body(true, true), &author, 1, &[], &adds, &ticks, &[]);
        assert_eq!(s.items.len(), 3);
        assert_eq!(s.items[0].done_by, Some(other));
        assert_eq!(s.items[2].item, ChecklistItemRef::Added(4));
        assert_eq!(s.done_count(), 2);
    }

    #[test]
    fn an_edit_drops_ticks_on_removed_items_and_a_close_is_final() {
        let author = [0u8; 32];
        let ticks = [
            ChecklistTickRecord {
                message_id: 2,
                ticker: author,
                privileged: false,
                item: ChecklistItemRef::Author(2),
                done: true,
            },
            ChecklistTickRecord {
                message_id: 6,
                ticker: author,
                privileged: false,
                item: ChecklistItemRef::Author(1),
                done: true,
            },
        ];
        let mut edited = body(false, false);
        edited.items.retain(|i| i.id == 1);
        edited.title = "Trip 2".into();
        let edits = [
            ChecklistEditRecord {
                message_id: 3,
                editor: [7; 32],
                body: body(true, true),
            },
            ChecklistEditRecord {
                message_id: 4,
                editor: author,
                body: edited,
            },
        ];
        let closes = [ChecklistCloseRecord {
            message_id: 5,
            closer: author,
        }];
        let s = fold(
            &body(false, false),
            &author,
            1,
            &edits,
            &[],
            &ticks,
            &closes,
        );
        assert_eq!(s.title, "Trip 2");
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].done_by, None);
        assert_eq!(s.closed_by, Some(5));
    }

    #[test]
    fn the_author_signature_binds_group_and_author() {
        let seed = [5u8; 32];
        let key = lakko_crypto::signing::SigningKey::from_seed(&seed)
            .verifying_key()
            .to_bytes();
        let g = [1u8; GROUP_ID_LEN];
        let s = SignedChecklist::sign(body(true, false), &g, &seed).unwrap();
        assert!(s.verify(&g, &key).is_ok());
        assert!(s.verify(&[0u8; GROUP_ID_LEN], &key).is_err());
        assert_eq!(
            SignedChecklist::unsigned(body(true, false)).verify(&g, &key),
            Err(ContentError::SignatureMissing)
        );
    }
}
