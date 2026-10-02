// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The fold (ADR-0058 section 3): one state every device of the account
//! converges on, whatever order the entries reach it in.
//!
//! ## The rule
//!
//! Every register holds the entry with the greatest `(stamp, canonical
//! bytes)` pair it has seen: a strictly greater stamp wins, and on an equal
//! stamp the bytewise larger [`SyncEntry::canonical_bytes`] wins. An
//! identical entry is the same write and changes nothing. A folder tombstone
//! is final (the maintainer's call, ADR-0058 section 3): it hides every
//! register of that folder whatever its stamp; those registers are dropped
//! when the tombstone lands and refused when they arrive after it, so a
//! deleted folder never comes back and the stored state is a function of the
//! SET of entries seen and nothing else. That is what makes the fold commutative,
//! associative and idempotent, and the tests below pin it over random
//! permutations.
//!
//! Tombstones and `NONE` member registers are kept forever: pruning either
//! lets a device that was offline across the change resurrect what was
//! removed.
//!
//! ## Saved messages (ADR-0065 section 4)
//!
//! A saved message is one more register, keyed `(conversation, message
//! id)`, and it follows the rule above with nothing added: a save and a
//! removal are two values of the same register, so the greater stamp wins,
//! a removal is held as a tombstone, and a later save with a greater stamp
//! saves the message again. It is not folder-scoped: no folder deletion
//! touches it.

use std::collections::BTreeMap;

use super::entry::{
    take_bytes32, take_u32, take_u64, take_u8, BlobId, CodecError, ConversationEntry,
    ConversationOrderEntry, ConversationRef, DraftEntry, FlagsEntry, FolderChanges, FolderId,
    FolderMeta, MemberState, ReadCursorEntry, RegisterKey, SavedSnapshot, SavedValue, Stamped,
    StateSnapshot, SyncEntry,
};

/// The persistence format's version byte.
///
/// `2` since the saved-message register (ADR-0065 section 4) joined the
/// state. Version `1` is refused rather than read: nothing is deployed, and
/// a build that silently accepted the older layout would be a second
/// decoder nobody tests.
const STATE_VERSION: u8 = 2;

/// A stored register value with its stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    hlc: u64,
    entry: SyncEntry,
}

/// What [`SyncedState::apply`] did with an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The entry replaced what was held (or filled an empty register).
    Applied,
    /// The held entry is newer, or identical.
    Superseded,
    /// The entry's folder was deleted; a deletion is final.
    Hidden,
}

/// One folder as the clients read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderView {
    /// The folder.
    pub folder_id: FolderId,
    /// Name and toggles, with the stamp they were written at. A folder is
    /// shown only once its meta register has arrived, so there is never an
    /// unnamed folder to draw.
    pub meta: Stamped<FolderMeta>,
    /// Conversations added by hand, sorted by `(kind, id)`.
    pub included: Vec<ConversationRef>,
    /// Conversations excluded by hand, sorted by `(kind, id)`.
    pub excluded: Vec<ConversationRef>,
    /// The folder's own pins, in their stored order.
    pub pinned: Vec<ConversationRef>,
}

/// One saved message as the clients list it (ADR-0065 section 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedView {
    /// The stamp the save was written at.
    pub hlc: u64,
    /// The conversation the message is in.
    pub conversation: ConversationRef,
    /// The message's Delivery Service journal id.
    pub message_id: u64,
    /// Wall-clock millis of the save.
    pub saved_at_ms: u64,
    /// What a row shows before this device holds the message.
    pub snapshot: SavedSnapshot,
    /// The blob ids of the message's files and thumbnails.
    pub blob_ids: Vec<BlobId>,
}

/// The folded synced state of one device (ADR-0058).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncedState {
    registers: BTreeMap<RegisterKey, Held>,
}

/// `true` when `(a_hlc, a_bytes)` beats `(b_hlc, b_bytes)`.
fn beats(a_hlc: u64, a: &SyncEntry, b_hlc: u64, b: &SyncEntry) -> bool {
    match a_hlc.cmp(&b_hlc) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => a.canonical_bytes() > b.canonical_bytes(),
    }
}

impl SyncedState {
    /// An empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one stamped entry in.
    ///
    /// The entry must already satisfy [`SyncEntry::validate`] (the DeviceGroup
    /// decoder runs it); an invalid one is refused here too, as `Err`.
    ///
    /// # Errors
    ///
    /// The [`super::entry::BoundsError`] the entry breaks.
    pub fn apply(
        &mut self,
        hlc: u64,
        entry: SyncEntry,
    ) -> Result<ApplyOutcome, super::entry::BoundsError> {
        entry.validate()?;
        let key = entry.key();
        if let Some(fid) = key.folder_scoped() {
            if self.tombstone(fid).is_some() {
                return Ok(ApplyOutcome::Hidden);
            }
        }
        if let Some(held) = self.registers.get(&key) {
            if !beats(hlc, &entry, held.hlc, &held.entry) {
                return Ok(ApplyOutcome::Superseded);
            }
        }
        if let SyncEntry::FolderRemoved(fid) = &entry {
            let fid = *fid;
            self.registers
                .retain(|k, _| k.folder_scoped() != Some(&fid));
        }
        self.registers.insert(key, Held { hlc, entry });
        Ok(ApplyOutcome::Applied)
    }

    /// Fold every entry in; entries that break a bound are skipped. Returns
    /// whether anything changed.
    pub fn apply_all<I: IntoIterator<Item = (u64, SyncEntry)>>(&mut self, entries: I) -> bool {
        let mut changed = false;
        for (hlc, entry) in entries {
            changed |= matches!(self.apply(hlc, entry), Ok(ApplyOutcome::Applied));
        }
        changed
    }

    /// Fold every register another state holds into this one, each with its
    /// original stamp: the merge a restore from a backup needs. Because the
    /// fold is commutative and idempotent, `a.merge_from(&b)` and
    /// `b.merge_from(&a)` end in the same state. Returns whether anything
    /// changed here.
    pub fn merge_from(&mut self, other: &Self) -> bool {
        self.apply_all(other.entries())
    }

    fn tombstone(&self, fid: &FolderId) -> Option<u64> {
        self.registers
            .get(&RegisterKey::FolderRemoved(*fid))
            .map(|h| h.hlc)
    }

    /// Every held entry with its original stamp, tombstones and `NONE`
    /// members included, in register order.
    #[must_use]
    pub fn entries(&self) -> Vec<(u64, SyncEntry)> {
        self.registers
            .values()
            .map(|h| (h.hlc, h.entry.clone()))
            .collect()
    }

    /// Number of held registers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registers.len()
    }

    /// `true` when nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registers.is_empty()
    }

    /// The whole state as ONE unsplit snapshot (`part` 0 of 1). The
    /// DeviceGroup layer splits it by encoded size before sending
    /// (`lakko_mls::device_group_sync::split_state_snapshot`), because only
    /// it knows the wire encoding. `folders` (which also holds the saved
    /// registers) may exceed the per-part register cap here; the split
    /// respects it.
    #[must_use]
    pub fn snapshot(&self) -> StateSnapshot {
        let mut snap = StateSnapshot {
            entries: Vec::new(),
            conversation_order: None,
            folders: FolderChanges::default(),
            part: 0,
            parts: 1,
        };
        for h in self.registers.values() {
            match &h.entry {
                SyncEntry::Flags(f) => snap.entries.push(Stamped {
                    hlc: h.hlc,
                    value: ConversationEntry::Flags(f.clone()),
                }),
                SyncEntry::ReadCursor(c) => snap.entries.push(Stamped {
                    hlc: h.hlc,
                    value: ConversationEntry::ReadCursor(c.clone()),
                }),
                SyncEntry::Draft(d) => snap.entries.push(Stamped {
                    hlc: h.hlc,
                    value: ConversationEntry::Draft(d.clone()),
                }),
                SyncEntry::ConversationOrder(o) => {
                    snap.conversation_order = Some(Stamped {
                        hlc: h.hlc,
                        value: o.clone(),
                    });
                },
                other => {
                    let _ = snap.folders.push(h.hlc, other.clone());
                },
            }
        }
        snap
    }

    // -----------------------------------------------------------------
    // Queries.
    // -----------------------------------------------------------------

    fn folder_ids(&self) -> Vec<FolderId> {
        let mut ids: Vec<FolderId> = self
            .registers
            .keys()
            .filter_map(RegisterKey::folder_scoped)
            .copied()
            .collect();
        ids.dedup();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn view(&self, fid: FolderId) -> Option<FolderView> {
        let meta = match self.registers.get(&RegisterKey::FolderMeta(fid)) {
            Some(Held {
                hlc,
                entry: SyncEntry::FolderMeta(m),
            }) => Stamped {
                hlc: *hlc,
                value: m.clone(),
            },
            _ => return None,
        };
        let pinned = match self.registers.get(&RegisterKey::FolderPins(fid)) {
            Some(Held {
                entry: SyncEntry::FolderPins(p),
                ..
            }) => p.pinned.clone(),
            _ => Vec::new(),
        };
        let mut included = Vec::new();
        let mut excluded = Vec::new();
        for h in self.registers.values() {
            if let SyncEntry::FolderMember(m) = &h.entry {
                if m.folder_id == fid {
                    match m.state {
                        MemberState::Included => included.push(m.conversation.clone()),
                        MemberState::Excluded => excluded.push(m.conversation.clone()),
                        MemberState::None => {},
                    }
                }
            }
        }
        Some(FolderView {
            folder_id: fid,
            meta,
            included,
            excluded,
            pinned,
        })
    }

    /// Every live folder, in resolved order: the ids the folder order names
    /// that are live, first occurrence only, then the live folders it does
    /// not name, by [`Self::placement_stamp`] then folder id.
    #[must_use]
    pub fn folders(&self) -> Vec<FolderView> {
        let mut live: BTreeMap<FolderId, FolderView> = self
            .folder_ids()
            .into_iter()
            .filter_map(|fid| self.view(fid).map(|v| (fid, v)))
            .collect();
        let mut out = Vec::with_capacity(live.len());
        if let Some(Held {
            entry: SyncEntry::FolderOrder(o),
            ..
        }) = self.registers.get(&RegisterKey::FolderOrder)
        {
            for fid in &o.folder_ids {
                if let Some(v) = live.remove(fid) {
                    out.push(v);
                }
            }
        }
        let mut rest: Vec<FolderView> = live.into_values().collect();
        rest.sort_by(|a, b| {
            a.meta
                .hlc
                .cmp(&b.meta.hlc)
                .then_with(|| a.folder_id.cmp(&b.folder_id))
        });
        out.extend(rest);
        out
    }

    /// One folder, when it is live.
    #[must_use]
    pub fn folder(&self, folder_id: &FolderId) -> Option<FolderView> {
        self.view(*folder_id)
    }

    /// The flags held for a conversation.
    #[must_use]
    pub fn flags(&self, group_id: &[u8]) -> Option<Stamped<FlagsEntry>> {
        match self.registers.get(&RegisterKey::Flags(group_id.to_vec())) {
            Some(Held {
                hlc,
                entry: SyncEntry::Flags(f),
            }) => Some(Stamped {
                hlc: *hlc,
                value: f.clone(),
            }),
            _ => None,
        }
    }

    /// The read cursor held for a conversation.
    #[must_use]
    pub fn read_cursor(&self, group_id: &[u8]) -> Option<Stamped<ReadCursorEntry>> {
        match self
            .registers
            .get(&RegisterKey::ReadCursor(group_id.to_vec()))
        {
            Some(Held {
                hlc,
                entry: SyncEntry::ReadCursor(c),
            }) => Some(Stamped {
                hlc: *hlc,
                value: c.clone(),
            }),
            _ => None,
        }
    }

    /// The draft held for a conversation, including a cleared one (empty
    /// text) so a caller can tell "cleared at T" from "never written".
    #[must_use]
    pub fn draft(&self, group_id: &[u8]) -> Option<Stamped<DraftEntry>> {
        match self.registers.get(&RegisterKey::Draft(group_id.to_vec())) {
            Some(Held {
                hlc,
                entry: SyncEntry::Draft(d),
            }) => Some(Stamped {
                hlc: *hlc,
                value: d.clone(),
            }),
            _ => None,
        }
    }

    /// Every flags register.
    #[must_use]
    pub fn all_flags(&self) -> Vec<Stamped<FlagsEntry>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::Flags(f) => Some(Stamped {
                    hlc: h.hlc,
                    value: f.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Every read-cursor register.
    #[must_use]
    pub fn all_read_cursors(&self) -> Vec<Stamped<ReadCursorEntry>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ReadCursor(c) => Some(Stamped {
                    hlc: h.hlc,
                    value: c.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Every draft register, cleared ones included.
    #[must_use]
    pub fn all_drafts(&self) -> Vec<Stamped<DraftEntry>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::Draft(d) => Some(Stamped {
                    hlc: h.hlc,
                    value: d.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// The explicit conversation order, when held.
    #[must_use]
    pub fn conversation_order(&self) -> Option<Stamped<ConversationOrderEntry>> {
        match self.registers.get(&RegisterKey::ConversationOrder) {
            Some(Held {
                hlc,
                entry: SyncEntry::ConversationOrder(o),
            }) => Some(Stamped {
                hlc: *hlc,
                value: o.clone(),
            }),
            _ => None,
        }
    }

    /// The account's saved messages, most recently saved first (ADR-0065
    /// section 4). Removed ones are not listed.
    ///
    /// Ordered by `saved_at_ms` descending, then by stamp descending, then
    /// by `(conversation, message id)`, so two devices holding the same
    /// state draw the same list.
    #[must_use]
    pub fn saved(&self) -> Vec<SavedView> {
        let mut out: Vec<SavedView> = self
            .registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::Saved(m) => match &m.value {
                    SavedValue::Present {
                        saved_at_ms,
                        snapshot,
                        blob_ids,
                    } => Some(SavedView {
                        hlc: h.hlc,
                        conversation: m.conversation.clone(),
                        message_id: m.message_id,
                        saved_at_ms: *saved_at_ms,
                        snapshot: snapshot.clone(),
                        blob_ids: blob_ids.clone(),
                    }),
                    SavedValue::Removed => None,
                },
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| {
            b.saved_at_ms
                .cmp(&a.saved_at_ms)
                .then_with(|| b.hlc.cmp(&a.hlc))
                .then_with(|| a.conversation.cmp(&b.conversation))
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
        out
    }

    /// Whether this message is saved now. `false` for a message never
    /// saved and for one whose register holds a removal.
    #[must_use]
    pub fn is_saved(&self, conversation: &ConversationRef, message_id: u64) -> bool {
        matches!(
            self.registers
                .get(&RegisterKey::Saved(conversation.clone(), message_id)),
            Some(Held {
                entry: SyncEntry::Saved(m),
                ..
            }) if matches!(m.value, SavedValue::Present { .. })
        )
    }

    /// Every blob id a saved message names, each once, in bytewise order:
    /// what a device sends keeps for (ADR-0065 section 3). Removed messages
    /// contribute nothing.
    #[must_use]
    pub fn saved_blob_ids(&self) -> Vec<BlobId> {
        let mut ids = std::collections::BTreeSet::new();
        for h in self.registers.values() {
            if let SyncEntry::Saved(m) = &h.entry {
                if let SavedValue::Present { blob_ids, .. } = &m.value {
                    ids.extend(blob_ids.iter().copied());
                }
            }
        }
        ids.into_iter().collect()
    }

    /// The greatest stamp held, or 0. A client seeds its clock above it
    /// after a restore so its next write is newer than anything restored.
    #[must_use]
    pub fn max_hlc(&self) -> u64 {
        self.registers.values().map(|h| h.hlc).max().unwrap_or(0)
    }

    // -----------------------------------------------------------------
    // Persistence.
    // -----------------------------------------------------------------

    /// Encode for local persistence and for the backup payload.
    ///
    /// Layout: `version(1) = 2 ‖ count(u32 BE) ‖ count × { hlc(u64 BE) ‖
    /// len(u32 BE) ‖ canonical entry bytes }`, in register order. Only
    /// entries the fold holds are written, so [`Self::from_bytes`] of the
    /// result is the same state.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![STATE_VERSION];
        out.extend_from_slice(
            &u32::try_from(self.registers.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for h in self.registers.values() {
            let b = h.entry.canonical_bytes();
            out.extend_from_slice(&h.hlc.to_be_bytes());
            out.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_be_bytes());
            out.extend_from_slice(&b);
        }
        out
    }

    /// Read persisted bytes back by folding every entry in. Fail closed:
    /// any malformed or out-of-bounds entry refuses the whole state, and so
    /// does any version byte but the one this build writes (version 1, the
    /// layout before saved messages, included).
    ///
    /// # Errors
    ///
    /// [`CodecError`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut cur = bytes;
        if take_u8(&mut cur)? != STATE_VERSION {
            return Err(CodecError::UnknownVersion);
        }
        let n = take_u32(&mut cur)?;
        let mut state = Self::new();
        for _ in 0..n {
            let hlc = take_u64(&mut cur)?;
            let entry_bytes = take_bytes32(&mut cur)?;
            let entry = SyncEntry::from_canonical_bytes(&entry_bytes)?;
            state.apply(hlc, entry)?;
        }
        if !cur.is_empty() {
            return Err(CodecError::Trailing);
        }
        Ok(state)
    }
}

/// Whether this device publishes the catch-up snapshot after the
/// DeviceGroup grew (ADR-0058 section 5.1): the device with the lowest leaf
/// index among those that were members BEFORE the growth. A device that was
/// not among them (the newcomer) never sends.
///
/// The caller still waits a random delay of up to 30 seconds first; a
/// duplicate from another device is harmless because the fold is
/// idempotent.
#[must_use]
pub fn should_send_snapshot(own_leaf: u32, prior_member_leaves: &[u32]) -> bool {
    prior_member_leaves.iter().min() == Some(&own_leaf)
}

#[cfg(test)]
mod tests {
    use super::super::entry::{
        ConversationRefKind, FolderMember, FolderOrder, FolderPins, SavedMessage, EXCLUDE_ARCHIVED,
        INCLUDE_GROUPS,
    };
    use super::*;
    use proptest::prelude::*;

    fn fid(b: u8) -> FolderId {
        [b; 16]
    }
    fn meta(f: u8, name: &str) -> SyncEntry {
        SyncEntry::FolderMeta(FolderMeta {
            folder_id: fid(f),
            name: name.into(),
            include_kinds: INCLUDE_GROUPS,
            exclude_flags: EXCLUDE_ARCHIVED,
        })
    }
    fn cref(b: u8) -> ConversationRef {
        ConversationRef {
            kind: ConversationRefKind::Group,
            id: vec![b; 4],
        }
    }
    fn member(f: u8, c: u8, state: MemberState) -> SyncEntry {
        SyncEntry::FolderMember(FolderMember {
            folder_id: fid(f),
            conversation: cref(c),
            state,
        })
    }

    fn blob(b: u8) -> BlobId {
        [b; 32]
    }
    fn save_at(c: u8, message_id: u64, saved_at_ms: u64, blobs: &[u8]) -> SyncEntry {
        SyncEntry::Saved(SavedMessage {
            conversation: cref(c),
            message_id,
            value: SavedValue::Present {
                saved_at_ms,
                snapshot: SavedSnapshot {
                    author: "Ada".into(),
                    excerpt: format!("message {message_id}"),
                    attachment_kind: String::new(),
                },
                blob_ids: blobs.iter().copied().map(blob).collect(),
            },
        })
    }
    fn unsave(c: u8, message_id: u64) -> SyncEntry {
        SyncEntry::Saved(SavedMessage {
            conversation: cref(c),
            message_id,
            value: SavedValue::Removed,
        })
    }

    #[test]
    fn save_then_remove_then_save_again() {
        let mut s = SyncedState::new();
        assert!(!s.is_saved(&cref(1), 7));
        assert_eq!(
            s.apply(10, save_at(1, 7, 100, &[1])),
            Ok(ApplyOutcome::Applied)
        );
        assert!(s.is_saved(&cref(1), 7));
        // Another message of the same chat, and the same id in another
        // chat, are their own registers.
        assert!(!s.is_saved(&cref(1), 8));
        assert!(!s.is_saved(&cref(2), 7));

        assert_eq!(s.apply(11, unsave(1, 7)), Ok(ApplyOutcome::Applied));
        assert!(!s.is_saved(&cref(1), 7));
        assert!(s.saved().is_empty());
        assert!(s.saved_blob_ids().is_empty());
        // The removal is held as a tombstone, so a save from a device that
        // was offline across it (an older stamp) does not come back.
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.apply(10, save_at(1, 7, 100, &[1])),
            Ok(ApplyOutcome::Superseded)
        );
        assert_eq!(
            s.apply(9, save_at(1, 7, 999, &[1])),
            Ok(ApplyOutcome::Superseded)
        );
        assert!(!s.is_saved(&cref(1), 7));

        // Saved again with a greater stamp.
        assert_eq!(
            s.apply(12, save_at(1, 7, 300, &[2])),
            Ok(ApplyOutcome::Applied)
        );
        assert!(s.is_saved(&cref(1), 7));
        let list = s.saved();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].hlc, list[0].saved_at_ms), (12, 300));
        assert_eq!(list[0].blob_ids, vec![blob(2)]);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn on_an_equal_stamp_the_removal_wins_in_either_order() {
        let mut a = SyncedState::new();
        a.apply(5, save_at(1, 7, 100, &[0xFF])).ok();
        a.apply(5, unsave(1, 7)).ok();
        let mut b = SyncedState::new();
        b.apply(5, unsave(1, 7)).ok();
        b.apply(5, save_at(1, 7, 100, &[0xFF])).ok();
        assert_eq!(a, b);
        assert!(!a.is_saved(&cref(1), 7));
        // Two saves on one stamp: the larger canonical bytes, either order.
        let mut c = SyncedState::new();
        c.apply(5, save_at(1, 7, 100, &[1])).ok();
        c.apply(5, save_at(1, 7, 200, &[1])).ok();
        let mut d = SyncedState::new();
        d.apply(5, save_at(1, 7, 200, &[1])).ok();
        d.apply(5, save_at(1, 7, 100, &[1])).ok();
        assert_eq!(c, d);
        assert_eq!(c.saved()[0].saved_at_ms, 200);
    }

    #[test]
    fn the_saved_list_is_most_recently_saved_first() {
        let mut s = SyncedState::new();
        // Stamps deliberately disagree with the save times: the list is
        // ordered by when the person saved, not by which write is newest.
        s.apply(9, save_at(1, 1, 100, &[])).ok();
        s.apply(1, save_at(2, 2, 300, &[])).ok();
        s.apply(5, save_at(1, 3, 200, &[])).ok();
        // Equal save times fall back to the stamp, then to the key.
        s.apply(7, save_at(3, 4, 50, &[])).ok();
        s.apply(8, save_at(3, 5, 50, &[])).ok();
        s.apply(8, save_at(2, 6, 50, &[])).ok();
        s.apply(2, unsave(1, 99)).ok();
        let ids: Vec<u64> = s.saved().iter().map(|v| v.message_id).collect();
        assert_eq!(ids, vec![2, 3, 1, 6, 5, 4]);
        assert_eq!(s.saved()[0].conversation, cref(2));
        assert_eq!(s.saved()[0].snapshot.excerpt, "message 2");
    }

    #[test]
    fn saved_blob_ids_are_named_once_and_only_for_saved_messages() {
        let mut s = SyncedState::new();
        s.apply(1, save_at(1, 1, 10, &[3, 1])).ok();
        s.apply(2, save_at(1, 2, 20, &[1, 2])).ok();
        s.apply(3, save_at(2, 1, 30, &[9])).ok();
        assert_eq!(s.saved_blob_ids(), vec![blob(1), blob(2), blob(3), blob(9)]);
        s.apply(4, unsave(2, 1)).ok();
        assert_eq!(s.saved_blob_ids(), vec![blob(1), blob(2), blob(3)]);
        s.apply(5, unsave(1, 1)).ok();
        // blob 1 is still named by the other saved message.
        assert_eq!(s.saved_blob_ids(), vec![blob(1), blob(2)]);
    }

    #[test]
    fn an_out_of_bounds_save_is_refused_by_the_fold() {
        let mut s = SyncedState::new();
        let dup = save_at(1, 1, 10, &[4, 4]);
        assert_eq!(
            s.apply(1, dup),
            Err(super::super::entry::BoundsError::DuplicateBlobId)
        );
        assert!(s.is_empty());
    }

    #[test]
    fn deleting_a_folder_leaves_saved_messages_alone() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(2, save_at(1, 7, 100, &[1])).ok();
        s.apply(3, SyncEntry::FolderRemoved(fid(1))).ok();
        assert!(s.folders().is_empty());
        assert!(s.is_saved(&cref(1), 7));
        assert_eq!(
            s.apply(4, save_at(1, 8, 200, &[])),
            Ok(ApplyOutcome::Applied)
        );
    }

    #[test]
    fn saved_messages_survive_persistence_a_snapshot_and_a_merge() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(2, save_at(1, 7, 100, &[1, 2])).ok();
        s.apply(3, unsave(2, 9)).ok();
        s.apply(4, save_at(3, 1, 50, &[])).ok();

        // Persistence (what the backup payload carries).
        let back = SyncedState::from_bytes(&s.to_bytes());
        assert_eq!(back.as_ref(), Ok(&s));
        assert_eq!(back.map(|b| b.saved()), Ok(s.saved()));

        // The snapshot hands a late device the saved list, tombstone
        // included, every stamp as written.
        let snap = s.snapshot();
        assert_eq!(snap.folders.saved.len(), 3);
        let mut late = SyncedState::new();
        late.apply_all(snap.stamped_entries());
        assert_eq!(late, s);
        assert_eq!(
            late.apply(2, save_at(2, 9, 1, &[])),
            Ok(ApplyOutcome::Superseded)
        );

        // A restore merges: the newer write of each register survives.
        let mut restored = SyncedState::new();
        restored.apply(9, unsave(1, 7)).ok();
        restored.apply(1, save_at(2, 9, 5, &[])).ok();
        let mut ab = restored.clone();
        ab.merge_from(&s);
        let mut ba = s.clone();
        ba.merge_from(&restored);
        assert_eq!(ab, ba);
        assert!(!ab.is_saved(&cref(1), 7));
        assert!(!ab.is_saved(&cref(2), 9));
        assert!(ab.is_saved(&cref(3), 1));
        assert_eq!(ab.max_hlc(), 9);
    }

    #[test]
    fn the_previous_persistence_version_is_refused() {
        // Version 1 was the layout before saved messages. A well-formed v1
        // state (here: empty, and one holding a folder tombstone) is
        // refused on its version byte, not read.
        assert_eq!(
            SyncedState::from_bytes(&[1, 0, 0, 0, 0]),
            Err(CodecError::UnknownVersion)
        );
        let mut s = SyncedState::new();
        s.apply(3, SyncEntry::FolderRemoved(fid(2))).ok();
        let mut v1 = s.to_bytes();
        assert_eq!(v1[0], 2);
        v1[0] = 1;
        assert_eq!(
            SyncedState::from_bytes(&v1),
            Err(CodecError::UnknownVersion)
        );
        assert_eq!(SyncedState::new().to_bytes(), vec![2, 0, 0, 0, 0]);
    }

    #[test]
    fn a_strictly_newer_stamp_wins_and_an_older_one_does_not() {
        let mut s = SyncedState::new();
        assert_eq!(s.apply(5, meta(1, "A")), Ok(ApplyOutcome::Applied));
        assert_eq!(s.apply(4, meta(1, "B")), Ok(ApplyOutcome::Superseded));
        assert_eq!(s.apply(6, meta(1, "C")), Ok(ApplyOutcome::Applied));
        assert_eq!(s.folders()[0].meta.value.name.as_str(), "C");
    }

    #[test]
    fn an_equal_stamp_is_decided_by_the_larger_bytes_in_either_order() {
        let mut a = SyncedState::new();
        a.apply(5, meta(1, "Alpha")).ok();
        a.apply(5, meta(1, "Beta")).ok();
        let mut b = SyncedState::new();
        b.apply(5, meta(1, "Beta")).ok();
        b.apply(5, meta(1, "Alpha")).ok();
        assert_eq!(a, b);
        assert_eq!(a.apply(5, meta(1, "Beta")), Ok(ApplyOutcome::Superseded));
    }

    #[test]
    fn a_deletion_is_final_whatever_arrives_after_it() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(2, member(1, 1, MemberState::Included)).ok();
        s.apply(3, SyncEntry::FolderRemoved(fid(1))).ok();
        assert!(s.folders().is_empty());
        // An edit made elsewhere before it learned of the deletion, stamped
        // later: the deletion still wins.
        assert_eq!(
            s.apply(4, member(1, 2, MemberState::Included)),
            Ok(ApplyOutcome::Hidden)
        );
        assert_eq!(s.apply(9, meta(1, "Work again")), Ok(ApplyOutcome::Hidden));
        assert!(s.folders().is_empty());
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn a_folder_without_its_name_is_not_shown_until_the_name_arrives() {
        let mut s = SyncedState::new();
        s.apply(2, member(1, 1, MemberState::Included)).ok();
        assert!(s.folders().is_empty());
        s.apply(1, meta(1, "Work")).ok();
        assert_eq!(s.folders()[0].included, vec![cref(1)]);
    }

    #[test]
    fn a_none_member_is_kept_so_an_older_include_cannot_bring_the_chat_back() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(5, member(1, 1, MemberState::None)).ok();
        assert_eq!(
            s.apply(3, member(1, 1, MemberState::Included)),
            Ok(ApplyOutcome::Superseded)
        );
        assert!(s.folders()[0].included.is_empty());
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn order_skips_dead_ids_and_appends_unnamed_live_folders_by_stamp() {
        let mut s = SyncedState::new();
        s.apply(10, meta(1, "One")).ok();
        s.apply(5, meta(2, "Two")).ok();
        s.apply(7, meta(3, "Three")).ok();
        s.apply(
            20,
            SyncEntry::FolderOrder(FolderOrder {
                folder_ids: vec![fid(9), fid(1), fid(1)],
            }),
        )
        .ok();
        let ids: Vec<FolderId> = s.folders().into_iter().map(|v| v.folder_id).collect();
        assert_eq!(ids, vec![fid(1), fid(2), fid(3)]);
    }

    #[test]
    fn pins_are_one_value() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(
            2,
            SyncEntry::FolderPins(FolderPins {
                folder_id: fid(1),
                pinned: vec![cref(2), cref(1)],
            }),
        )
        .ok();
        assert_eq!(s.folders()[0].pinned, vec![cref(2), cref(1)]);
    }

    #[test]
    fn persistence_round_trips() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(2, member(1, 1, MemberState::Excluded)).ok();
        s.apply(3, SyncEntry::FolderRemoved(fid(2))).ok();
        s.apply(
            4,
            SyncEntry::Draft(DraftEntry {
                group_id: vec![1; 32],
                text: String::new(),
                reply_to_id: vec![],
            }),
        )
        .ok();
        let bytes = s.to_bytes();
        assert_eq!(SyncedState::from_bytes(&bytes), Ok(s));
        assert_eq!(SyncedState::from_bytes(&[]), Err(CodecError::Truncated));
        assert_eq!(
            SyncedState::from_bytes(&[3, 0, 0, 0, 0]),
            Err(CodecError::UnknownVersion)
        );
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            SyncedState::from_bytes(&trailing),
            Err(CodecError::Trailing)
        );
    }

    #[test]
    fn a_snapshot_carries_every_entry_with_its_original_stamp() {
        let mut s = SyncedState::new();
        s.apply(1, meta(1, "Work")).ok();
        s.apply(9, SyncEntry::FolderRemoved(fid(2))).ok();
        s.apply(3, member(1, 1, MemberState::None)).ok();
        s.apply(
            4,
            SyncEntry::ConversationOrder(ConversationOrderEntry {
                group_ids: vec![vec![1]],
            }),
        )
        .ok();
        let snap = s.snapshot();
        let mut fresh = SyncedState::new();
        fresh.apply_all(snap.stamped_entries());
        assert_eq!(fresh, s);
    }

    #[test]
    fn merging_two_states_keeps_the_newer_register_from_each_and_a_deletion() {
        let mut a = SyncedState::new();
        a.apply(5, meta(1, "Work")).ok();
        a.apply(6, member(1, 1, MemberState::Included)).ok();
        a.apply(2, meta(2, "Old")).ok();
        let mut b = SyncedState::new();
        b.apply(3, meta(1, "Older name")).ok();
        b.apply(7, member(1, 2, MemberState::Excluded)).ok();
        b.apply(4, SyncEntry::FolderRemoved(fid(2))).ok();
        let mut ab = a.clone();
        assert!(ab.merge_from(&b));
        let mut ba = b.clone();
        ba.merge_from(&a);
        assert_eq!(ab, ba);
        let f = ab.folders();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].meta.value.name, "Work");
        assert_eq!(f[0].included, vec![cref(1)]);
        assert_eq!(f[0].excluded, vec![cref(2)]);
        assert!(!ab.clone().merge_from(&b));
    }

    #[test]
    fn only_the_lowest_prior_leaf_sends() {
        assert!(should_send_snapshot(0, &[3, 0, 5]));
        assert!(!should_send_snapshot(3, &[3, 0, 5]));
        assert!(!should_send_snapshot(7, &[3, 0, 5]));
        assert!(!should_send_snapshot(0, &[]));
    }

    // -----------------------------------------------------------------
    // The key property: order does not matter.
    // -----------------------------------------------------------------

    fn arb_entry() -> impl Strategy<Value = (u64, SyncEntry)> {
        // A small space of folders, conversations and stamps so ties,
        // tombstones and overwrites collide often.
        let hlc = 0u64..8;
        let f = 0u8..3;
        let c = 0u8..3;
        let name = prop::sample::select(vec!["A", "B", "Work", "Home"]);
        let state = prop::sample::select(vec![
            MemberState::None,
            MemberState::Included,
            MemberState::Excluded,
        ]);
        prop_oneof![
            (hlc.clone(), f.clone(), name).prop_map(|(h, f, n)| (h, meta(f, n))),
            (hlc.clone(), f.clone(), c.clone(), state)
                .prop_map(|(h, f, c, s)| (h, member(f, c, s))),
            (
                hlc.clone(),
                f.clone(),
                prop::collection::vec(c.clone(), 0..3)
            )
                .prop_map(|(h, f, cs)| (
                    h,
                    SyncEntry::FolderPins(FolderPins {
                        folder_id: fid(f),
                        pinned: cs.into_iter().map(cref).collect(),
                    })
                )),
            (hlc.clone(), f.clone()).prop_map(|(h, f)| (h, SyncEntry::FolderRemoved(fid(f)))),
            (hlc.clone(), prop::collection::vec(f, 0..4)).prop_map(|(h, fs)| (
                h,
                SyncEntry::FolderOrder(FolderOrder {
                    folder_ids: fs.into_iter().map(fid).collect(),
                })
            )),
            (hlc.clone(), c.clone(), any::<bool>(), any::<bool>()).prop_map(|(h, c, p, m)| (
                h,
                SyncEntry::Flags(FlagsEntry {
                    group_id: vec![c],
                    pinned: p,
                    muted: m,
                    archived: false,
                    unread_override: false,
                })
            )),
            (hlc.clone(), c.clone(), 0u8..3).prop_map(|(h, c, t)| (
                h,
                SyncEntry::Draft(DraftEntry {
                    group_id: vec![c],
                    text: "x".repeat(usize::from(t)),
                    reply_to_id: vec![],
                })
            )),
            (hlc.clone(), prop::collection::vec(c.clone(), 0..3)).prop_map(|(h, cs)| (
                h,
                SyncEntry::ConversationOrder(ConversationOrderEntry {
                    group_ids: cs.into_iter().map(|c| vec![c]).collect(),
                })
            )),
            // Saved messages: two conversations x two message ids, so a
            // save, a different save and a removal of one register meet on
            // one stamp often.
            (
                hlc.clone(),
                c.clone(),
                0u64..2,
                0u64..3,
                prop::collection::btree_set(0u8..4, 0..3)
            )
                .prop_map(|(h, c, m, at, blobs)| (
                    h,
                    save_at(c, m, at, &blobs.into_iter().collect::<Vec<u8>>())
                )),
            (hlc, c, 0u64..2).prop_map(|(h, c, m)| (h, unsave(c, m))),
        ]
    }

    fn fold(entries: &[(u64, SyncEntry)]) -> SyncedState {
        let mut s = SyncedState::new();
        s.apply_all(entries.iter().cloned());
        s
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Commutative: any permutation of the same entries gives the same
        /// state and the same folder view.
        #[test]
        fn any_arrival_order_converges(
            entries in prop::collection::vec(arb_entry(), 0..24),
            seed in any::<u64>(),
        ) {
            let a = fold(&entries);
            let mut shuffled = entries.clone();
            // Deterministic Fisher-Yates from the seed.
            let mut x = seed | 1;
            for i in (1..shuffled.len()).rev() {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                let j = usize::try_from(x % (i as u64 + 1)).unwrap_or(0);
                shuffled.swap(i, j);
            }
            let b = fold(&shuffled);
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(a.folders(), b.folders());
            prop_assert_eq!(a.saved(), b.saved());
            prop_assert_eq!(a.saved_blob_ids(), b.saved_blob_ids());
        }

        /// The tie-break, stated on its own: for any two writes of ONE
        /// saved register on ONE stamp, both arrival orders hold the same
        /// value, and it is the one with the larger canonical bytes.
        #[test]
        fn an_equal_stamp_tie_on_a_saved_register_has_one_winner(
            h in 0u64..4,
            first in (0u64..3, prop::collection::btree_set(0u8..4, 0..3), any::<bool>()),
            second in (0u64..3, prop::collection::btree_set(0u8..4, 0..3), any::<bool>()),
        ) {
            let entry = |(at, blobs, removed): (u64, std::collections::BTreeSet<u8>, bool)| {
                if removed {
                    unsave(1, 7)
                } else {
                    save_at(1, 7, at, &blobs.into_iter().collect::<Vec<u8>>())
                }
            };
            let (x, y) = (entry(first), entry(second));
            let xy = fold(&[(h, x.clone()), (h, y.clone())]);
            let yx = fold(&[(h, y.clone()), (h, x.clone())]);
            prop_assert_eq!(&xy, &yx);
            let winner = if x.canonical_bytes() >= y.canonical_bytes() { x } else { y };
            prop_assert_eq!(xy.entries(), vec![(h, winner)]);
        }

        /// The list reads agree with the registers: a message is in the
        /// saved list exactly when `is_saved` says so, the list is ordered
        /// most recently saved first, and the blob ids are exactly the
        /// distinct ids the listed messages name.
        #[test]
        fn the_saved_reads_agree_with_each_other(entries in prop::collection::vec(arb_entry(), 0..24)) {
            let s = fold(&entries);
            let list = s.saved();
            for v in &list {
                prop_assert!(s.is_saved(&v.conversation, v.message_id));
            }
            for (_, e) in s.entries() {
                if let SyncEntry::Saved(m) = e {
                    let listed = list
                        .iter()
                        .any(|v| v.conversation == m.conversation && v.message_id == m.message_id);
                    prop_assert_eq!(listed, s.is_saved(&m.conversation, m.message_id));
                    prop_assert_eq!(listed, matches!(m.value, SavedValue::Present { .. }));
                }
            }
            prop_assert!(list.windows(2).all(|w| w[0].saved_at_ms >= w[1].saved_at_ms));
            let mut want: Vec<BlobId> = list.iter().flat_map(|v| v.blob_ids.clone()).collect();
            want.sort_unstable();
            want.dedup();
            prop_assert_eq!(s.saved_blob_ids(), want);
        }

        /// Persisted bytes that were damaged never panic the decoder, and
        /// whatever it still accepts is a state the fold holds: every
        /// entry within its bounds, and stable under a further round trip.
        #[test]
        fn damaged_persisted_bytes_are_refused_or_decode_to_a_valid_state(
            entries in prop::collection::vec(arb_entry(), 0..12),
            at in any::<prop::sample::Index>(),
            flip in 1u8..=255,
            cut in any::<prop::sample::Index>(),
        ) {
            let bytes = fold(&entries).to_bytes();
            let mut flipped = bytes.clone();
            let i = at.index(flipped.len());
            flipped[i] ^= flip;
            let truncated = &bytes[..cut.index(bytes.len())];
            for candidate in [flipped.as_slice(), truncated] {
                if let Ok(state) = SyncedState::from_bytes(candidate) {
                    for (_, e) in state.entries() {
                        prop_assert!(e.validate().is_ok());
                    }
                    prop_assert_eq!(SyncedState::from_bytes(&state.to_bytes()), Ok(state));
                }
            }
        }

        /// Idempotent: folding everything twice changes nothing.
        #[test]
        fn folding_twice_is_folding_once(entries in prop::collection::vec(arb_entry(), 0..24)) {
            let a = fold(&entries);
            let mut twice = entries.clone();
            twice.extend(entries.iter().cloned());
            prop_assert_eq!(a, fold(&twice));
        }

        /// Associative: folding two devices' states into each other (via
        /// their snapshots) equals folding the union.
        #[test]
        fn merging_states_equals_folding_the_union(
            left in prop::collection::vec(arb_entry(), 0..16),
            right in prop::collection::vec(arb_entry(), 0..16),
        ) {
            let mut l = fold(&left);
            let r = fold(&right);
            l.apply_all(r.snapshot().stamped_entries());
            let mut all = left.clone();
            all.extend(right.iter().cloned());
            prop_assert_eq!(l, fold(&all));
        }

        /// A snapshot rebuilds the same state on a fresh device, and the
        /// persistence codec round-trips it.
        #[test]
        fn snapshot_and_persistence_rebuild_the_state(entries in prop::collection::vec(arb_entry(), 0..24)) {
            let a = fold(&entries);
            let mut fresh = SyncedState::new();
            fresh.apply_all(a.snapshot().stamped_entries());
            prop_assert_eq!(&fresh, &a);
            prop_assert_eq!(SyncedState::from_bytes(&a.to_bytes()), Ok(a));
        }
    }
}
