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
//! ## The lineage register (ADR-0065 R.5)
//!
//! `successor group id -> root conversation id`, one register per successor,
//! folded by the same rule as every other register, so convergence needs no
//! second argument. It is **write-once at the writer**: [`SyncedState::set_lineage`]
//! refuses to write a successor that already holds a register, so a device
//! never mints a second stamp for a conversation that has already moved. Two
//! devices that write the same successor concurrently converge on one value
//! by the ordinary rule. It does not travel in [`StateSnapshot`] (whose wire
//! codec lives in `lakko-mls`); [`SyncedState::lineage_envelopes`] and
//! [`SyncedState::apply_lineage_envelope`] carry it instead, and
//! [`SyncedState::to_bytes`] persists it with the rest.

use std::collections::BTreeMap;

use super::entry::{
    take_bytes32, take_u32, take_u64, take_u8, CodecError, ConversationEntry,
    ConversationOrderEntry, ConversationRef, DraftEntry, FlagsEntry, FolderChanges, FolderId,
    FolderMeta, LineageEntry, MemberState, ReadCursorEntry, RegisterKey, Stamped, StateSnapshot,
    SyncEntry,
};
use super::lineage::{
    compose_lineage_envelope, decode_lineage_envelope, LineageEnvelopeError,
    LINEAGE_ENVELOPE_MAX_ENTRIES,
};

/// The persistence format's version byte.
const STATE_VERSION: u8 = 1;

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
    /// (`lakko_mls::device_group::split_state_snapshot`), because only it
    /// knows the wire encoding. `folders` may exceed the per-part register
    /// cap here; the split respects it.
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
                // Lineage registers travel in their own envelope
                // (`lineage_envelopes`), never in the snapshot.
                SyncEntry::Lineage(_) => {},
                other => {
                    let _ = snap.folders.push(h.hlc, other.clone());
                },
            }
        }
        snap
    }

    // -----------------------------------------------------------------
    // Conversation lineage (ADR-0065 R.5).
    // -----------------------------------------------------------------

    /// Record that `successor_group_id` continues the conversation
    /// `root_conversation_id`, stamped `hlc`. **Write-once**: when a register
    /// is already held for this successor nothing is written and `Ok(false)`
    /// is returned, whatever root it names; read it with
    /// [`Self::lineage_root`]. `Ok(true)` means the register was written and
    /// the caller should send it ([`Self::lineage_envelope_for`]).
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::LineageIdLength`] /
    /// [`super::entry::BoundsError::LineageSelfReference`].
    pub fn set_lineage(
        &mut self,
        hlc: u64,
        successor_group_id: &[u8],
        root_conversation_id: &[u8],
    ) -> Result<bool, super::entry::BoundsError> {
        let entry = LineageEntry {
            successor_group_id: successor_group_id.to_vec(),
            root_conversation_id: root_conversation_id.to_vec(),
        };
        entry.validate()?;
        if self
            .registers
            .contains_key(&RegisterKey::Lineage(successor_group_id.to_vec()))
        {
            return Ok(false);
        }
        Ok(matches!(
            self.apply(hlc, SyncEntry::Lineage(entry))?,
            ApplyOutcome::Applied
        ))
    }

    /// The lineage register held for a successor group: which conversation
    /// it continues. `None` for a group that is itself a root, or unknown.
    #[must_use]
    pub fn lineage_root(&self, successor_group_id: &[u8]) -> Option<Stamped<LineageEntry>> {
        match self
            .registers
            .get(&RegisterKey::Lineage(successor_group_id.to_vec()))
        {
            Some(Held {
                hlc,
                entry: SyncEntry::Lineage(l),
            }) => Some(Stamped {
                hlc: *hlc,
                value: l.clone(),
            }),
            _ => None,
        }
    }

    /// The conversation id a group belongs to: the root its lineage register
    /// names, or the group id itself when it holds none (a root, or a group
    /// this device knows no lineage for).
    #[must_use]
    pub fn conversation_id_of(&self, group_id: &[u8]) -> Vec<u8> {
        self.lineage_root(group_id)
            .map_or_else(|| group_id.to_vec(), |s| s.value.root_conversation_id)
    }

    /// Every successor recorded for one conversation, ordered by `(stamp,
    /// successor id)`: the order the registers were written in, as far as
    /// the stamps tell. The root itself is not listed.
    #[must_use]
    pub fn lineage_successors(&self, root_conversation_id: &[u8]) -> Vec<Stamped<LineageEntry>> {
        let mut out: Vec<Stamped<LineageEntry>> = self
            .all_lineage()
            .into_iter()
            .filter(|s| s.value.root_conversation_id == root_conversation_id)
            .collect();
        out.sort_by(|a, b| {
            a.hlc
                .cmp(&b.hlc)
                .then_with(|| a.value.successor_group_id.cmp(&b.value.successor_group_id))
        });
        out
    }

    /// Every lineage register, ascending by successor id.
    #[must_use]
    pub fn all_lineage(&self) -> Vec<Stamped<LineageEntry>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::Lineage(l) => Some(Stamped {
                    hlc: h.hlc,
                    value: l.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// The envelope carrying the ONE register held for `successor_group_id`
    /// with its original stamp: what a device sends to its other devices
    /// right after [`Self::set_lineage`] returned `true`. `None` when no
    /// register is held.
    #[must_use]
    pub fn lineage_envelope_for(&self, successor_group_id: &[u8]) -> Option<Vec<u8>> {
        let held = self.lineage_root(successor_group_id)?;
        compose_lineage_envelope(&[held]).ok()
    }

    /// Every held lineage register with its original stamp, as envelopes of
    /// at most [`LINEAGE_ENVELOPE_MAX_ENTRIES`] each: the catch-up re-send
    /// beside [`Self::snapshot`]. Empty when none is held.
    #[must_use]
    pub fn lineage_envelopes(&self) -> Vec<Vec<u8>> {
        self.all_lineage()
            .chunks(LINEAGE_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_lineage_envelope(chunk).ok())
            .collect()
    }

    /// Decode a lineage envelope and fold every register in with its
    /// original stamp. Fail closed: a malformed envelope changes nothing.
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`LineageEnvelopeError`].
    pub fn apply_lineage_envelope(&mut self, bytes: &[u8]) -> Result<bool, LineageEnvelopeError> {
        let entries = decode_lineage_envelope(bytes)?;
        Ok(self.apply_all(
            entries
                .into_iter()
                .map(|s| (s.hlc, SyncEntry::Lineage(s.value))),
        ))
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
    /// Layout: `version(1) = 1 ‖ count(u32 BE) ‖ count × { hlc(u64 BE) ‖
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
    /// any malformed or out-of-bounds entry refuses the whole state.
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
        ConversationRefKind, FolderMember, FolderOrder, FolderPins, EXCLUDE_ARCHIVED,
        INCLUDE_GROUPS,
    };
    use super::super::lineage::LINEAGE_ENVELOPE_DOMAIN_V1;
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
    fn lineage(successor: u8, root: u8) -> SyncEntry {
        SyncEntry::Lineage(LineageEntry {
            successor_group_id: vec![successor],
            root_conversation_id: vec![root],
        })
    }
    fn member(f: u8, c: u8, state: MemberState) -> SyncEntry {
        SyncEntry::FolderMember(FolderMember {
            folder_id: fid(f),
            conversation: cref(c),
            state,
        })
    }

    #[test]
    fn set_lineage_is_write_once_and_reads_back() {
        let mut s = SyncedState::new();
        assert_eq!(s.set_lineage(5, b"succ-1", b"root"), Ok(true));
        // A second local write for the same successor mints nothing, with
        // the same root or another, at any stamp.
        assert_eq!(s.set_lineage(9, b"succ-1", b"root"), Ok(false));
        assert_eq!(s.set_lineage(9, b"succ-1", b"other"), Ok(false));
        let held = s.lineage_root(b"succ-1").unwrap();
        assert_eq!(held.hlc, 5);
        assert_eq!(held.value.root_conversation_id, b"root");
        assert_eq!(s.conversation_id_of(b"succ-1"), b"root");
        assert_eq!(s.conversation_id_of(b"root"), b"root");
        assert_eq!(s.conversation_id_of(b"unknown"), b"unknown");
        assert!(s.lineage_root(b"root").is_none());

        assert_eq!(
            s.set_lineage(1, b"x", b"x"),
            Err(super::super::entry::BoundsError::LineageSelfReference)
        );
        assert_eq!(
            s.set_lineage(1, b"", b"x"),
            Err(super::super::entry::BoundsError::LineageIdLength)
        );
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn a_conversation_lists_its_successors_in_stamp_order() {
        let mut s = SyncedState::new();
        s.set_lineage(7, b"s2", b"root").unwrap();
        s.set_lineage(3, b"s9", b"root").unwrap();
        s.set_lineage(3, b"s1", b"root").unwrap();
        s.set_lineage(1, b"t1", b"elsewhere").unwrap();
        let ids: Vec<Vec<u8>> = s
            .lineage_successors(b"root")
            .into_iter()
            .map(|l| l.value.successor_group_id)
            .collect();
        assert_eq!(ids, vec![b"s1".to_vec(), b"s9".to_vec(), b"s2".to_vec()]);
        assert!(s.lineage_successors(b"nobody").is_empty());
        assert_eq!(s.all_lineage().len(), 4);
    }

    #[test]
    fn a_received_lineage_register_follows_the_fold_rule() {
        // Two devices wrote the same successor concurrently: the greater
        // stamp wins, an equal stamp is decided by the bytes, in any order.
        let mut a = SyncedState::new();
        a.apply(5, lineage(1, 10)).ok();
        assert_eq!(a.apply(4, lineage(1, 11)), Ok(ApplyOutcome::Superseded));
        assert_eq!(a.apply(6, lineage(1, 12)), Ok(ApplyOutcome::Applied));
        let mut x = SyncedState::new();
        x.apply(5, lineage(1, 10)).ok();
        x.apply(5, lineage(1, 11)).ok();
        let mut y = SyncedState::new();
        y.apply(5, lineage(1, 11)).ok();
        y.apply(5, lineage(1, 10)).ok();
        assert_eq!(x, y);
        assert_eq!(
            x.lineage_root(&[1]).unwrap().value.root_conversation_id,
            [11]
        );
    }

    #[test]
    fn lineage_travels_by_envelope_and_persistence_never_by_snapshot() {
        let mut a = SyncedState::new();
        a.apply(1, meta(1, "Work")).ok();
        a.set_lineage(4, b"succ", b"root").unwrap();
        assert!(a
            .snapshot()
            .stamped_entries()
            .iter()
            .all(|(_, e)| !matches!(e, SyncEntry::Lineage(_))));

        let live = a.lineage_envelope_for(b"succ").unwrap();
        assert!(a.lineage_envelope_for(b"root").is_none());
        let mut b = SyncedState::new();
        assert_eq!(b.apply_lineage_envelope(&live), Ok(true));
        assert_eq!(b.apply_lineage_envelope(&live), Ok(false));
        assert_eq!(b.lineage_root(b"succ"), a.lineage_root(b"succ"));
        // The original stamp travelled, so a re-send never beats a later write.
        assert_eq!(b.lineage_root(b"succ").unwrap().hlc, 4);

        assert!(SyncedState::new().lineage_envelopes().is_empty());
        assert_eq!(a.lineage_envelopes(), vec![live]);
        let restored = SyncedState::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(restored.lineage_root(b"succ"), a.lineage_root(b"succ"));
        let mut merged = SyncedState::new();
        assert!(merged.merge_from(&restored));
        assert_eq!(merged, a);

        // A malformed envelope changes nothing.
        let mut bad = a.lineage_envelope_for(b"succ").unwrap();
        bad.push(0);
        let before = b.clone();
        assert_eq!(
            b.apply_lineage_envelope(&bad),
            Err(LineageEnvelopeError::Trailing)
        );
        assert_eq!(b, before);
    }

    #[test]
    fn more_registers_than_one_envelope_holds_are_split() {
        let mut s = SyncedState::new();
        let n = u32::try_from(LINEAGE_ENVELOPE_MAX_ENTRIES + 3).unwrap();
        for i in 0..n {
            s.set_lineage(u64::from(i), &i.to_be_bytes(), b"root")
                .unwrap();
        }
        let envelopes = s.lineage_envelopes();
        assert_eq!(envelopes.len(), 2);
        let mut fresh = SyncedState::new();
        for e in &envelopes {
            fresh.apply_lineage_envelope(e).unwrap();
        }
        assert_eq!(fresh, s);
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
            SyncedState::from_bytes(&[2, 0, 0, 0, 0]),
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
            // Successors 0..3 and roots 10..13: several writers naming
            // different roots for one successor collide often.
            (hlc, c, 10u8..13).prop_map(|(h, s, r)| (h, lineage(s, r))),
        ]
    }

    /// Everything a fresh device is sent to catch up: the snapshot's entries
    /// and the lineage envelopes.
    fn catch_up(from: &SyncedState, into: &mut SyncedState) {
        into.apply_all(from.snapshot().stamped_entries());
        for envelope in from.lineage_envelopes() {
            into.apply_lineage_envelope(&envelope).unwrap();
        }
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
            catch_up(&r, &mut l);
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
            catch_up(&a, &mut fresh);
            prop_assert_eq!(&fresh, &a);
            prop_assert_eq!(SyncedState::from_bytes(&a.to_bytes()), Ok(a));
        }

        /// `merge_from` (the backup restore) is commutative and idempotent
        /// with lineage registers in play, and agrees with the union fold.
        #[test]
        fn merge_from_is_commutative_and_idempotent(
            left in prop::collection::vec(arb_entry(), 0..16),
            right in prop::collection::vec(arb_entry(), 0..16),
        ) {
            let (l, r) = (fold(&left), fold(&right));
            let mut lr = l.clone();
            lr.merge_from(&r);
            let mut rl = r.clone();
            rl.merge_from(&l);
            prop_assert_eq!(&lr, &rl);
            prop_assert_eq!(lr.all_lineage(), rl.all_lineage());
            let mut again = lr.clone();
            prop_assert!(!again.merge_from(&r));
            prop_assert!(!again.merge_from(&l));
            prop_assert_eq!(&again, &lr);
            let mut all = left.clone();
            all.extend(right.iter().cloned());
            prop_assert_eq!(lr, fold(&all));
        }

        /// Lineage registers alone: any arrival order of any writers'
        /// envelopes converges, every held register is the greatest
        /// `(stamp, bytes)` written for its successor, and the envelopes
        /// rebuild exactly the lineage part of the state.
        #[test]
        fn lineage_envelopes_converge_in_any_order(
            writes in prop::collection::vec((0u64..6, 0u8..4, 10u8..13), 1..20),
            seed in any::<u64>(),
        ) {
            let envelopes: Vec<Vec<u8>> = writes
                .iter()
                .map(|(h, s, r)| {
                    compose_lineage_envelope(&[Stamped {
                        hlc: *h,
                        value: LineageEntry {
                            successor_group_id: vec![*s; 4],
                            root_conversation_id: vec![*r; 4],
                        },
                    }])
                    .unwrap()
                })
                .collect();
            let mut a = SyncedState::new();
            for e in &envelopes {
                a.apply_lineage_envelope(e).unwrap();
            }
            let mut shuffled = envelopes.clone();
            let mut x = seed | 1;
            for i in (1..shuffled.len()).rev() {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                let j = usize::try_from(x % (i as u64 + 1)).unwrap_or(0);
                shuffled.swap(i, j);
            }
            let mut b = SyncedState::new();
            for e in shuffled.iter().chain(envelopes.iter()) {
                b.apply_lineage_envelope(e).unwrap();
            }
            prop_assert_eq!(&a, &b);
            for held in a.all_lineage() {
                let s = held.value.successor_group_id[0];
                let best = writes
                    .iter()
                    .filter(|(_, ws, _)| *ws == s)
                    .map(|(h, _, r)| (*h, *r))
                    .max()
                    .unwrap();
                prop_assert_eq!((held.hlc, held.value.root_conversation_id[0]), best);
            }
            let mut rebuilt = SyncedState::new();
            for e in a.lineage_envelopes() {
                prop_assert!(rebuilt.apply_lineage_envelope(&e).unwrap());
            }
            prop_assert_eq!(rebuilt, a);
        }

        /// The persistence decoder is total on arbitrary bytes and on a
        /// damaged valid state, and whatever it accepts re-encodes to a
        /// state that reads back identically.
        #[test]
        fn from_bytes_is_fail_closed_on_hostile_input(
            entries in prop::collection::vec(arb_entry(), 0..12),
            noise in prop::collection::vec(any::<u8>(), 0..64),
            at in any::<usize>(),
            flip in 1u8..=255,
        ) {
            let _ = SyncedState::from_bytes(&noise);
            let mut bytes = fold(&entries).to_bytes();
            let i = at % bytes.len();
            // Every strict prefix of a valid state is refused.
            prop_assert!(SyncedState::from_bytes(&bytes[..i]).is_err());
            bytes[i] ^= flip;
            if let Ok(state) = SyncedState::from_bytes(&bytes) {
                prop_assert_eq!(SyncedState::from_bytes(&state.to_bytes()), Ok(state));
            }
        }

        /// The lineage envelope decoder is total, and whatever it accepts
        /// is canonical: it re-encodes to the identical bytes.
        #[test]
        fn lineage_envelope_decode_is_total_and_canonical(
            noise in prop::collection::vec(any::<u8>(), 0..96),
            writes in prop::collection::vec((any::<u64>(), 0u8..8, 10u8..13), 1..8),
            at in any::<usize>(),
            flip in 1u8..=255,
        ) {
            let mut prefixed = LINEAGE_ENVELOPE_DOMAIN_V1.to_vec();
            prefixed.extend_from_slice(&noise);
            for candidate in [noise.clone(), prefixed] {
                if let Ok(decoded) = decode_lineage_envelope(&candidate) {
                    prop_assert_eq!(compose_lineage_envelope(&decoded).unwrap(), candidate);
                }
            }
            let mut dedup = std::collections::BTreeMap::new();
            for (h, s, r) in writes {
                dedup.insert(s, (h, r));
            }
            let entries: Vec<Stamped<LineageEntry>> = dedup
                .into_iter()
                .map(|(s, (h, r))| Stamped {
                    hlc: h,
                    value: LineageEntry {
                        successor_group_id: vec![s],
                        root_conversation_id: vec![r],
                    },
                })
                .collect();
            let mut wire = compose_lineage_envelope(&entries).unwrap();
            prop_assert_eq!(decode_lineage_envelope(&wire).unwrap(), entries);
            let i = at % wire.len();
            wire[i] ^= flip;
            if let Ok(decoded) = decode_lineage_envelope(&wire) {
                prop_assert_eq!(compose_lineage_envelope(&decoded).unwrap(), wire);
            }
        }
    }
}
