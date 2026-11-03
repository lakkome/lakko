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
//! ## The lineage register (ADR-0073 R.5)
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
//!
//! ## The recovery email register (ADR-0075, amended 2026-10-13)
//!
//! One register per account holding the normalised recovery email address,
//! or an empty value for "none". It is an ordinary last-writer-wins register
//! (not write-once): [`SyncedState::set_recovery_email`] writes whenever its
//! stamp wins. A removal is a stamped empty value and is KEPT, so a device
//! that was offline across it cannot bring the address back with an older
//! stamp. Like lineage it does not travel in [`StateSnapshot`];
//! [`SyncedState::recovery_email_envelope`] and
//! [`SyncedState::apply_recovery_email_envelope`] carry it between devices,
//! and [`SyncedState::to_bytes`] persists it (and so the backup carries it).
//!
//! ## The own-username register (ADR-0076 section 3)
//!
//! One register per account holding the account's canonical username, or an
//! empty value for "none": the recovery-email register's shape exactly.
//! Last-writer-wins, the empty value kept with its stamp, carried between
//! devices by [`SyncedState::own_username_envelope`] /
//! [`SyncedState::apply_own_username_envelope`], persisted by
//! [`SyncedState::to_bytes`], never in [`StateSnapshot`].
//!
//! ## The link-record register (ADR-0079 section 4)
//!
//! One register per invite link of the account, keyed by link id, carried by
//! [`SyncedState::link_record_envelopes`] /
//! [`SyncedState::apply_link_record_envelope`] and persisted with the rest,
//! never in [`StateSnapshot`]. It follows the rule above with one addition
//! that makes a revocation final: between two link records a revoked one
//! beats a live one whatever the stamps, so the order is `(revoked, stamp,
//! canonical bytes)`. That is still a total order on the values of one
//! register, so the fold stays commutative, associative and idempotent.
//! [`SyncedState::link_answer_verdict`] is the one rule every client asks
//! when a contact request carries a link answer.
//!
//! ## The device-acknowledgement register (identity-recovery 17.5)
//!
//! One register per device signature key: the person has seen the
//! "a device was added to your account" warning for that device on some
//! device of the account, so no device warns about it any more. Write-once
//! at the writer ([`SyncedState::acknowledge_device`]), folded by the rule
//! above, carried by [`SyncedState::device_ack_envelopes`] /
//! [`SyncedState::apply_device_ack_envelope`], persisted with the rest,
//! never in [`StateSnapshot`]. Adding the register added a tag and changed
//! no existing layout, so it did not change the persistence version.
//!
//! ## Own channel reactions (ADR-0087 section 13a)
//!
//! One register per `(channel, post, emoji)` the account reacted with or took
//! off, folded by the rule above (a removal is a kept tombstone; on an exact
//! stamp tie the removal wins). Carried by
//! [`SyncedState::channel_reaction_envelopes`] /
//! [`SyncedState::apply_channel_reaction_envelope`], persisted with the rest
//! (so the backup carries it), never in [`StateSnapshot`]. The own-comments
//! register (the nonce each comment's write key comes from) is carried and
//! folded the same way, by [`SyncedState::channel_comment_envelopes`] /
//! [`SyncedState::apply_channel_comment_envelope`]. Unlike the other
//! registers both are **pruned**: [`SyncedState::prune_channel_activity`] drops
//! every register whose post has left the channel service
//! ([`super::channel_reaction::CHANNEL_REACTION_RETENTION_MS`]). That is safe
//! where pruning a conversation end is not: once the post is gone there is
//! nothing a stale register could show or take back, so a register a late
//! envelope brings back changes nothing and goes at the next prune.
//!
//! ## Saved messages (ADR-0065 section 4)
//!
//! A saved message is one more register, keyed `(conversation, message
//! id)`, and it follows the rule above with nothing added: a save and a
//! removal are two values of the same register, so the greater stamp wins,
//! a removal is held as a tombstone, and a later save with a greater stamp
//! saves the message again. It is not folder-scoped: no folder deletion
//! touches it.
//!
//! ## Conversation ends (ADR-0087 section 9)
//!
//! One register per conversation an end has reached, holding "ended" or
//! "joined again" with the MLS epoch of that event. Its order is `(epoch,
//! stamp, canonical bytes)`: the rule above with the epoch in front, the
//! link-record shape again, so the fold stays commutative, associative and
//! idempotent. The stamp alone cannot decide this register. A device that was
//! offline across a removal and a re-add applies the old removal when it comes
//! back and writes "ended" with a stamp newer than the "joined" its account
//! already holds; by stamps that would delete the conversation from every
//! device that rejoined. An MLS group keeps its id through all of it and its
//! epoch only ever rises, so the epoch says which event is the later one
//! whatever the clocks say. A channel subscription has no epoch and writes `0`,
//! so a channel's registers are ordered by stamps alone, and that is what
//! "strictly newer stamp wins" comes to there. At a complete tie "joined" has
//! the larger bytes: a tie must not delete a conversation.
//!
//! [`SyncedState::compose_conversation_ended`] and
//! [`SyncedState::compose_conversation_joined`] are the only writers a client
//! uses. Each answers with the register to send, or `None` when writing one
//! would tell nobody anything: an end older than what is held, or at
//! [`super::entry::CONVERSATION_ENDS_MAX_CLIENT`]; a join with no end held, so
//! the state does not grow with every group the person ever joined.
//!
//! **A register is never dropped.** A tombstone could go only if no device can
//! still need it, and no age shows that: the 30-day retention bounds what a
//! device can read from a journal, not how long it can stay offline, and a
//! device behind that horizon catches up from the snapshot, which is a re-send
//! of this state. Dropping a register at 30 days, or at any other age, is
//! exactly what leaves that device holding the conversation. The client-side
//! limit bounds the state instead.

use std::collections::BTreeMap;

use super::channel_comment::{
    compose_channel_comment_envelope, decode_channel_comment_envelope, ChannelCommentEntry,
    ChannelCommentEnvelopeError, CHANNEL_COMMENTS_MAX_CLIENT, CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES,
    CHANNEL_COMMENT_NONCE_LEN,
};
use super::channel_reaction::{
    compose_channel_reaction_envelope, decode_channel_reaction_envelope, ChannelReactionEntry,
    ChannelReactionEnvelopeError, CHANNEL_REACTIONS_MAX_CLIENT, CHANNEL_REACTIONS_PER_POST_MAX,
    CHANNEL_REACTION_CHANNEL_ID_LEN, CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES,
};
use super::device_ack::{
    compose_device_ack_envelope, decode_device_ack_envelope, DeviceAckEntry,
    DeviceAckEnvelopeError, DEVICE_ACK_ENVELOPE_MAX_ENTRIES, DEVICE_ACK_KEY_LEN,
};
use super::entry::{
    take_bytes32, take_u32, take_u64, take_u8, BlobId, CodecError, ConversationEnd,
    ConversationEndState, ConversationEntry, ConversationOrderEntry, ConversationRef, DraftEntry,
    FlagsEntry, FolderChanges, FolderId, FolderMeta, LineageEntry, MemberState, OwnUsernameEntry,
    ReadCursorEntry, RecoveryEmailEntry, RegisterKey, SavedSnapshot, SavedValue, Stamped,
    StateSnapshot, SyncEntry, CONVERSATION_ENDS_MAX_CLIENT,
};
use super::lineage::{
    compose_lineage_envelope, decode_lineage_envelope, LineageEnvelopeError,
    LINEAGE_ENVELOPE_MAX_ENTRIES,
};
use super::link_record::{
    compose_link_record_envelope, decode_link_record_envelope, LinkAnswerVerdict, LinkRecordEntry,
    LinkRecordEnvelopeError, LINK_RECORD_ENVELOPE_MAX_ENTRIES, LINK_RECORD_ID_LEN,
    LINK_RECORD_REQUESTER_KEY_LEN, LINK_RECORD_SECRET_LEN,
};
use super::own_username::{
    compose_own_username_envelope, decode_own_username_envelope, OwnUsernameEnvelopeError,
};

use super::recovery_email::{
    compose_recovery_email_envelope, decode_recovery_email_envelope, RecoveryEmailEnvelopeError,
};

/// The persistence format's version byte.
///
/// `2` since the saved-message register (ADR-0065 section 4) joined the
/// state, `3` since the link-record register (ADR-0079 section 4), `4` since
/// a link record carries its ledger row's revocation seed (ADR-0079 as
/// reworked 2026-10-17), `5` since a link record carries the `shared` flag
/// (2026-10-26), `6` since a saved-message snapshot carries the message's
/// own time (`SavedSnapshot::sent_at_ms`; the two branches that made the
/// last two changes had each called theirs `5`), `7` since the
/// conversation-end register (ADR-0087 section 9), `8` since the
/// own channel-reaction register (ADR-0087 section 13a), `9` since the
/// own channel-comment register (the same section, a key per write). Older
/// versions are refused rather than read: nothing is deployed, and a build
/// that silently accepted an older layout would be a second decoder nobody
/// tests.
const STATE_VERSION: u8 = 9;

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

/// `true` when `(a_hlc, a_bytes)` beats `(b_hlc, b_bytes)`. Between two link
/// records a revoked one beats a live one first (ADR-0079: a revocation is
/// final), then a shared one beats an unshared one (a copied link never
/// becomes editable again, 2026-10-26). Between two conversation-end registers
/// the greater MLS epoch wins first (ADR-0087 section 9: the epoch, not a
/// clock, says which event came later).
fn beats(a_hlc: u64, a: &SyncEntry, b_hlc: u64, b: &SyncEntry) -> bool {
    if let (SyncEntry::LinkRecord(x), SyncEntry::LinkRecord(y)) = (a, b) {
        if x.revoked != y.revoked {
            return x.revoked;
        }
        if x.shared != y.shared {
            return x.shared;
        }
    }
    if let (SyncEntry::ConversationEnd(x), SyncEntry::ConversationEnd(y)) = (a, b) {
        if x.epoch != y.epoch {
            return x.epoch > y.epoch;
        }
    }
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
                // Lineage registers, the recovery email and the own
                // username travel in their own envelopes
                // (`lineage_envelopes`, `recovery_email_envelope`,
                // `own_username_envelope`), never in the snapshot.
                SyncEntry::Lineage(_)
                | SyncEntry::RecoveryEmail(_)
                | SyncEntry::OwnUsername(_)
                | SyncEntry::LinkRecord(_)
                | SyncEntry::DeviceAck(_)
                | SyncEntry::ChannelReaction(_)
                | SyncEntry::ChannelComment(_) => {},
                other => {
                    let _ = snap.folders.push(h.hlc, other.clone());
                },
            }
        }
        snap
    }

    // -----------------------------------------------------------------
    // Conversation lineage (ADR-0073 R.5).
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
    // Recovery email (ADR-0075, amended 2026-10-13).
    // -----------------------------------------------------------------

    /// Write the account's recovery email register, stamped `hlc`:
    /// `Some(address)` sets the address, `None` (or an empty address)
    /// records that the account has none.
    ///
    /// `address` must already be normalised
    /// (`lakko_mnemonic::recovery_email::normalize_recovery_email`; the FFI
    /// surfaces do it); it is stored as given and never re-spelled.
    ///
    /// An ordinary last-writer-wins write: `Ok(true)` when the register now
    /// holds this value (send [`Self::recovery_email_envelope`] to the
    /// account's other devices), `Ok(false)` when what is held has a greater
    /// stamp, or an equal stamp and larger-or-identical bytes, and nothing
    /// changed.
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::RecoveryEmailLength`] /
    /// [`super::entry::BoundsError::RecoveryEmailShape`].
    pub fn set_recovery_email(
        &mut self,
        hlc: u64,
        address: Option<&str>,
    ) -> Result<bool, super::entry::BoundsError> {
        let entry = RecoveryEmailEntry {
            address: address.unwrap_or_default().to_owned(),
        };
        Ok(matches!(
            self.apply(hlc, SyncEntry::RecoveryEmail(entry))?,
            ApplyOutcome::Applied
        ))
    }

    /// The recovery email register as held, a stamped removal included, so a
    /// caller can tell "removed at T" from "never written" (`None`).
    #[must_use]
    pub fn recovery_email_register(&self) -> Option<Stamped<RecoveryEmailEntry>> {
        match self.registers.get(&RegisterKey::RecoveryEmail) {
            Some(Held {
                hlc,
                entry: SyncEntry::RecoveryEmail(r),
            }) => Some(Stamped {
                hlc: *hlc,
                value: r.clone(),
            }),
            _ => None,
        }
    }

    /// The account's recovery email address, or `None` when the register was
    /// never written or holds a removal.
    #[must_use]
    pub fn recovery_email(&self) -> Option<String> {
        self.recovery_email_register()
            .map(|s| s.value.address)
            .filter(|a| !a.is_empty())
    }

    /// The envelope carrying the held register with its original stamp, a
    /// removal included: what a device sends to its other devices after
    /// [`Self::set_recovery_email`] returned `true`, and again as the
    /// catch-up re-send beside [`Self::snapshot`] after the DeviceGroup
    /// grew. `None` when the register was never written.
    #[must_use]
    pub fn recovery_email_envelope(&self) -> Option<Vec<u8>> {
        compose_recovery_email_envelope(&self.recovery_email_register()?).ok()
    }

    /// Decode a recovery-email envelope and fold the register in with its
    /// original stamp. Fail closed: a malformed envelope changes nothing.
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`RecoveryEmailEnvelopeError`].
    pub fn apply_recovery_email_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, RecoveryEmailEnvelopeError> {
        let register = decode_recovery_email_envelope(bytes)?;
        Ok(matches!(
            self.apply(register.hlc, SyncEntry::RecoveryEmail(register.value))?,
            ApplyOutcome::Applied
        ))
    }

    // -----------------------------------------------------------------
    // Own username (ADR-0076 section 3).
    // -----------------------------------------------------------------

    /// Write the account's own-username register, stamped `hlc`:
    /// `Some(username)` sets it, `None` (or an empty string) records that
    /// the account has none.
    ///
    /// `username` must already be canonical
    /// (`lakko_contacts::username::check_canonical_username`; the FFI
    /// surfaces normalise first); it is stored as given.
    ///
    /// An ordinary last-writer-wins write: `Ok(true)` when the register now
    /// holds this value (send [`Self::own_username_envelope`] to the
    /// account's other devices), `Ok(false)` when what is held has a greater
    /// stamp, or an equal stamp and larger-or-identical bytes, and nothing
    /// changed.
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::OwnUsername`].
    pub fn set_own_username(
        &mut self,
        hlc: u64,
        username: Option<&str>,
    ) -> Result<bool, super::entry::BoundsError> {
        let entry = OwnUsernameEntry {
            username: username.unwrap_or_default().to_owned(),
        };
        Ok(matches!(
            self.apply(hlc, SyncEntry::OwnUsername(entry))?,
            ApplyOutcome::Applied
        ))
    }

    /// The own-username register as held, a stamped "none" included, so a
    /// caller can tell "cleared at T" from "never written" (`None`).
    #[must_use]
    pub fn own_username_register(&self) -> Option<Stamped<OwnUsernameEntry>> {
        match self.registers.get(&RegisterKey::OwnUsername) {
            Some(Held {
                hlc,
                entry: SyncEntry::OwnUsername(u),
            }) => Some(Stamped {
                hlc: *hlc,
                value: u.clone(),
            }),
            _ => None,
        }
    }

    /// The account's own username, or `None` when the register was never
    /// written or holds "none".
    #[must_use]
    pub fn own_username(&self) -> Option<String> {
        self.own_username_register()
            .map(|s| s.value.username)
            .filter(|u| !u.is_empty())
    }

    /// The envelope carrying the held register with its original stamp, a
    /// "none" included: what a device sends to its other devices after
    /// [`Self::set_own_username`] returned `true`, and again as the catch-up
    /// re-send beside [`Self::snapshot`] after the DeviceGroup grew. `None`
    /// when the register was never written.
    #[must_use]
    pub fn own_username_envelope(&self) -> Option<Vec<u8>> {
        compose_own_username_envelope(&self.own_username_register()?).ok()
    }

    /// Decode an own-username envelope and fold the register in with its
    /// original stamp. Fail closed: a malformed envelope changes nothing.
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`OwnUsernameEnvelopeError`].
    pub fn apply_own_username_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, OwnUsernameEnvelopeError> {
        let register = decode_own_username_envelope(bytes)?;
        Ok(matches!(
            self.apply(register.hlc, SyncEntry::OwnUsername(register.value))?,
            ApplyOutcome::Applied
        ))
    }

    // -----------------------------------------------------------------
    // Device acknowledgements (identity-recovery 17.5).
    // -----------------------------------------------------------------

    /// Record that the person has seen the added-device warning for each of
    /// `device_keys`, stamped `hlc`, at wall-clock `now_ms`. **Write-once**: a
    /// key already acknowledged is not re-stamped. Returns the envelopes to
    /// send to the account's other devices, carrying exactly the registers
    /// this call wrote (empty when nothing was new).
    pub fn acknowledge_devices(
        &mut self,
        hlc: u64,
        device_keys: &[[u8; DEVICE_ACK_KEY_LEN]],
        now_ms: u64,
    ) -> Vec<Vec<u8>> {
        let mut written: Vec<Stamped<DeviceAckEntry>> = Vec::new();
        for key in device_keys {
            if self.acknowledge_device(hlc, *key, now_ms) {
                written.push(Stamped {
                    hlc,
                    value: DeviceAckEntry {
                        device_key: *key,
                        acknowledged_at_ms: now_ms,
                    },
                });
            }
        }
        written.sort_by_key(|e| e.value.device_key);
        written.dedup_by_key(|e| e.value.device_key);
        written
            .chunks(DEVICE_ACK_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_device_ack_envelope(chunk).ok())
            .collect()
    }

    /// One key of [`Self::acknowledge_devices`]: `true` when the register was
    /// written now, `false` when it was already held.
    pub fn acknowledge_device(
        &mut self,
        hlc: u64,
        device_key: [u8; DEVICE_ACK_KEY_LEN],
        now_ms: u64,
    ) -> bool {
        if self.is_device_acknowledged(&device_key) {
            return false;
        }
        matches!(
            self.apply(
                hlc,
                SyncEntry::DeviceAck(DeviceAckEntry {
                    device_key,
                    acknowledged_at_ms: now_ms,
                }),
            ),
            Ok(ApplyOutcome::Applied)
        )
    }

    /// Whether the warning for `device_key` was seen on some device.
    #[must_use]
    pub fn is_device_acknowledged(&self, device_key: &[u8; DEVICE_ACK_KEY_LEN]) -> bool {
        self.registers
            .contains_key(&RegisterKey::DeviceAck(*device_key))
    }

    /// Every acknowledged device key, ascending.
    #[must_use]
    pub fn acknowledged_devices(&self) -> Vec<[u8; DEVICE_ACK_KEY_LEN]> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::DeviceAck(a) => Some(a.device_key),
                _ => None,
            })
            .collect()
    }

    /// Every held acknowledgement with its original stamp, as envelopes of
    /// at most [`DEVICE_ACK_ENVELOPE_MAX_ENTRIES`] entries: the re-send a
    /// device owes when a live send did not go out.
    #[must_use]
    pub fn device_ack_envelopes(&self) -> Vec<Vec<u8>> {
        let held: Vec<Stamped<DeviceAckEntry>> = self
            .registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::DeviceAck(a) => Some(Stamped {
                    hlc: h.hlc,
                    value: a.clone(),
                }),
                _ => None,
            })
            .collect();
        held.chunks(DEVICE_ACK_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_device_ack_envelope(chunk).ok())
            .collect()
    }

    /// Decode a device-acknowledgement envelope and fold every register in
    /// with its original stamp. Fail closed. Returns whether anything
    /// changed.
    ///
    /// # Errors
    ///
    /// [`DeviceAckEnvelopeError`].
    pub fn apply_device_ack_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, DeviceAckEnvelopeError> {
        let entries = decode_device_ack_envelope(bytes)?;
        Ok(self.apply_all(
            entries
                .into_iter()
                .map(|s| (s.hlc, SyncEntry::DeviceAck(s.value))),
        ))
    }

    // -----------------------------------------------------------------
    // Own channel reactions (ADR-0087 section 13a).
    // -----------------------------------------------------------------

    /// Record that the account reacted to (`reacted`) or took its reaction
    /// off (`!reacted`) the post at `post_cursor` in `channel_id` with
    /// `emoji`, stamped `hlc`. `post_at_ms` is the post's server time.
    ///
    /// Returns the envelope to send to the account's other devices, or
    /// `None` when nothing was written: the register already says so, or a
    /// new reaction would pass [`CHANNEL_REACTIONS_MAX_CLIENT`] (a removal of
    /// a held reaction is always written).
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::ChannelReactionEmoji`], or
    /// [`super::entry::BoundsError::TooManyChannelReactions`] when the
    /// account already stands with [`CHANNEL_REACTIONS_PER_POST_MAX`] other
    /// reactions on the post (the client shows it and sends nothing).
    pub fn set_channel_reaction(
        &mut self,
        hlc: u64,
        channel_id: [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        emoji: &str,
        reacted: bool,
        post_at_ms: u64,
    ) -> Result<Option<Vec<u8>>, super::entry::BoundsError> {
        let value = ChannelReactionEntry {
            channel_id,
            post_cursor,
            emoji: emoji.to_string(),
            reacted,
            post_at_ms,
        };
        value.validate()?;
        let held = self.channel_reaction_register(&channel_id, post_cursor, emoji);
        if reacted
            && !held.as_ref().is_some_and(|h| h.value.reacted)
            && self.channel_reactions_on(&channel_id, post_cursor).len()
                >= CHANNEL_REACTIONS_PER_POST_MAX
        {
            return Err(super::entry::BoundsError::TooManyChannelReactions);
        }
        match &held {
            Some(h) if h.value.reacted == reacted => return Ok(None),
            None if !reacted => return Ok(None),
            None if self.channel_reaction_count() >= CHANNEL_REACTIONS_MAX_CLIENT => {
                return Ok(None);
            },
            _ => {},
        }
        if self.apply(hlc, SyncEntry::ChannelReaction(value.clone()))? != ApplyOutcome::Applied {
            return Ok(None);
        }
        Ok(compose_channel_reaction_envelope(&[Stamped { hlc, value }]).ok())
    }

    fn channel_reaction_register(
        &self,
        channel_id: &[u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        emoji: &str,
    ) -> Option<Stamped<ChannelReactionEntry>> {
        match self.registers.get(&RegisterKey::ChannelReaction(
            *channel_id,
            post_cursor,
            emoji.to_string(),
        )) {
            Some(Held {
                hlc,
                entry: SyncEntry::ChannelReaction(r),
            }) => Some(Stamped {
                hlc: *hlc,
                value: r.clone(),
            }),
            _ => None,
        }
    }

    fn channel_reaction_count(&self) -> usize {
        self.registers
            .keys()
            .filter(|k| matches!(k, RegisterKey::ChannelReaction(..)))
            .count()
    }

    /// Whether the account's reaction `emoji` stands on the post.
    #[must_use]
    pub fn has_channel_reaction(
        &self,
        channel_id: &[u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        emoji: &str,
    ) -> bool {
        self.channel_reaction_register(channel_id, post_cursor, emoji)
            .is_some_and(|r| r.value.reacted)
    }

    /// The emoji the account's reactions on the post stand with, in byte
    /// order of the emoji.
    #[must_use]
    pub fn channel_reactions_on(
        &self,
        channel_id: &[u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
    ) -> Vec<String> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ChannelReaction(r)
                    if r.reacted && r.channel_id == *channel_id && r.post_cursor == post_cursor =>
                {
                    Some(r.emoji.clone())
                },
                _ => None,
            })
            .collect()
    }

    /// Every held own-reaction register (tombstones included) with its
    /// original stamp, as envelopes of at most
    /// [`CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES`] entries: the catch-up a
    /// device sends on DeviceGroup growth, and the re-send it owes when a
    /// live send did not go out.
    #[must_use]
    pub fn channel_reaction_envelopes(&self) -> Vec<Vec<u8>> {
        let held: Vec<Stamped<ChannelReactionEntry>> = self
            .registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ChannelReaction(r) => Some(Stamped {
                    hlc: h.hlc,
                    value: r.clone(),
                }),
                _ => None,
            })
            .collect();
        held.chunks(CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_channel_reaction_envelope(chunk).ok())
            .collect()
    }

    /// Decode a channel-reaction envelope and fold every register in with
    /// its original stamp. Fail closed. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`ChannelReactionEnvelopeError`].
    pub fn apply_channel_reaction_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, ChannelReactionEnvelopeError> {
        let entries = decode_channel_reaction_envelope(bytes)?;
        Ok(self.apply_all(
            entries
                .into_iter()
                .map(|s| (s.hlc, SyncEntry::ChannelReaction(s.value))),
        ))
    }

    /// Drop every own-reaction and own-comment register whose post has left
    /// the channel service at `now_ms` (its time plus
    /// [`super::channel_reaction::CHANNEL_REACTION_RETENTION_MS`] is in the
    /// past). Returns how many went.
    pub fn prune_channel_activity(&mut self, now_ms: u64) -> usize {
        let before = self.registers.len();
        self.registers.retain(|_, h| match &h.entry {
            SyncEntry::ChannelReaction(r) => !r.expired_at(now_ms),
            SyncEntry::ChannelComment(c) => !c.expired_at(now_ms),
            _ => true,
        });
        before - self.registers.len()
    }

    // -----------------------------------------------------------------
    // Own channel comments (ADR-0087 section 13a).
    // -----------------------------------------------------------------

    fn channel_comment_register(
        &self,
        channel_id: &[u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        comment_nonce: &[u8; CHANNEL_COMMENT_NONCE_LEN],
    ) -> Option<Stamped<ChannelCommentEntry>> {
        match self.registers.get(&RegisterKey::ChannelComment(
            *channel_id,
            post_cursor,
            *comment_nonce,
        )) {
            Some(Held {
                hlc,
                entry: SyncEntry::ChannelComment(c),
            }) => Some(Stamped {
                hlc: *hlc,
                value: c.clone(),
            }),
            _ => None,
        }
    }

    fn write_channel_comment(&mut self, hlc: u64, value: ChannelCommentEntry) -> Option<Vec<u8>> {
        match self.apply(hlc, SyncEntry::ChannelComment(value.clone())) {
            Ok(ApplyOutcome::Applied) => {
                compose_channel_comment_envelope(&[Stamped { hlc, value }]).ok()
            },
            _ => None,
        }
    }

    /// Record a comment the account just wrote under the post at
    /// `post_cursor` in `channel_id`, with the nonce its write key came from,
    /// stamped `hlc`. `post_at_ms` is the post's server time. **Write-once**:
    /// a nonce already held writes nothing. Returns the envelope to send, or
    /// `None` when nothing was written (already held, or at
    /// [`CHANNEL_COMMENTS_MAX_CLIENT`]).
    pub fn record_channel_comment(
        &mut self,
        hlc: u64,
        channel_id: [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        comment_nonce: [u8; CHANNEL_COMMENT_NONCE_LEN],
        post_at_ms: u64,
    ) -> Option<Vec<u8>> {
        if self
            .channel_comment_register(&channel_id, post_cursor, &comment_nonce)
            .is_some()
            || self.channel_comment_count() >= CHANNEL_COMMENTS_MAX_CLIENT
        {
            return None;
        }
        self.write_channel_comment(
            hlc,
            ChannelCommentEntry {
                channel_id,
                post_cursor,
                comment_nonce,
                deleted: false,
                post_at_ms,
            },
        )
    }

    /// Record that the account deleted its comment `comment_nonce`, stamped
    /// `hlc`. Returns the envelope to send, or `None` when the register
    /// already says deleted. A deletion of a comment this device never held
    /// is still written, so the other devices drop it too.
    pub fn delete_channel_comment(
        &mut self,
        hlc: u64,
        channel_id: [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
        comment_nonce: [u8; CHANNEL_COMMENT_NONCE_LEN],
        post_at_ms: u64,
    ) -> Option<Vec<u8>> {
        if self
            .channel_comment_register(&channel_id, post_cursor, &comment_nonce)
            .is_some_and(|c| c.value.deleted)
        {
            return None;
        }
        self.write_channel_comment(
            hlc,
            ChannelCommentEntry {
                channel_id,
                post_cursor,
                comment_nonce,
                deleted: true,
                post_at_ms,
            },
        )
    }

    fn channel_comment_count(&self) -> usize {
        self.registers
            .keys()
            .filter(|k| matches!(k, RegisterKey::ChannelComment(..)))
            .count()
    }

    /// The nonces of the account's comments on the post that are not deleted,
    /// ascending: derive each one's write key to find the rows that are the
    /// account's own.
    #[must_use]
    pub fn channel_comments_on(
        &self,
        channel_id: &[u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        post_cursor: u64,
    ) -> Vec<[u8; CHANNEL_COMMENT_NONCE_LEN]> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ChannelComment(c)
                    if !c.deleted
                        && c.channel_id == *channel_id
                        && c.post_cursor == post_cursor =>
                {
                    Some(c.comment_nonce)
                },
                _ => None,
            })
            .collect()
    }

    /// Every held own-comment register (tombstones included) with its
    /// original stamp, as envelopes of at most
    /// [`CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES`] entries.
    #[must_use]
    pub fn channel_comment_envelopes(&self) -> Vec<Vec<u8>> {
        let held: Vec<Stamped<ChannelCommentEntry>> = self
            .registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ChannelComment(c) => Some(Stamped {
                    hlc: h.hlc,
                    value: c.clone(),
                }),
                _ => None,
            })
            .collect();
        held.chunks(CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_channel_comment_envelope(chunk).ok())
            .collect()
    }

    /// Decode an own-comments envelope and fold every register in with its
    /// original stamp. Fail closed. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`ChannelCommentEnvelopeError`].
    pub fn apply_channel_comment_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, ChannelCommentEnvelopeError> {
        let entries = decode_channel_comment_envelope(bytes)?;
        Ok(self.apply_all(
            entries
                .into_iter()
                .map(|s| (s.hlc, SyncEntry::ChannelComment(s.value))),
        ))
    }

    // -----------------------------------------------------------------
    // Queries.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Link records (ADR-0079 section 4).
    // -----------------------------------------------------------------

    /// Write a link record (a link just minted, or any change to one).
    /// `Ok(true)` when it was written and should be sent
    /// ([`Self::link_record_envelope_for`]).
    ///
    /// # Errors
    ///
    /// The [`super::entry::BoundsError`] the record breaks.
    pub fn set_link_record(
        &mut self,
        hlc: u64,
        record: LinkRecordEntry,
    ) -> Result<bool, super::entry::BoundsError> {
        Ok(matches!(
            self.apply(hlc, SyncEntry::LinkRecord(record))?,
            ApplyOutcome::Applied
        ))
    }

    /// The record held for a link id.
    #[must_use]
    pub fn link_record(
        &self,
        link_id: &[u8; LINK_RECORD_ID_LEN],
    ) -> Option<Stamped<LinkRecordEntry>> {
        match self.registers.get(&RegisterKey::LinkRecord(*link_id)) {
            Some(Held {
                hlc,
                entry: SyncEntry::LinkRecord(r),
            }) => Some(Stamped {
                hlc: *hlc,
                value: r.clone(),
            }),
            _ => None,
        }
    }

    /// Every link record, ascending by link id.
    #[must_use]
    pub fn all_link_records(&self) -> Vec<Stamped<LinkRecordEntry>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::LinkRecord(r) => Some(Stamped {
                    hlc: h.hlc,
                    value: r.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Mark a link revoked. `Ok(true)` when the register changed (send it);
    /// `Ok(false)` when no record is held or it was already revoked.
    ///
    /// # Errors
    ///
    /// The [`super::entry::BoundsError`] the held record breaks (never, for
    /// a record the fold accepted).
    pub fn revoke_link_record(
        &mut self,
        hlc: u64,
        link_id: &[u8; LINK_RECORD_ID_LEN],
    ) -> Result<bool, super::entry::BoundsError> {
        let Some(held) = self.link_record(link_id) else {
            return Ok(false);
        };
        if held.value.revoked {
            return Ok(false);
        }
        let mut next = held.value;
        next.revoked = true;
        self.set_link_record(hlc.max(held.hlc.saturating_add(1)), next)
    }

    /// Mark a link copied (2026-10-26): someone may hold it now, so it is no
    /// longer editable. `Ok(true)` when the register changed (send it);
    /// `Ok(false)` when no record is held, it is revoked, or it was already
    /// shared.
    ///
    /// # Errors
    ///
    /// The [`super::entry::BoundsError`] the held record breaks (never, for
    /// a record the fold accepted).
    pub fn mark_link_shared(
        &mut self,
        hlc: u64,
        link_id: &[u8; LINK_RECORD_ID_LEN],
    ) -> Result<bool, super::entry::BoundsError> {
        let Some(held) = self.link_record(link_id) else {
            return Ok(false);
        };
        if held.value.revoked || held.value.shared {
            return Ok(false);
        }
        let mut next = held.value;
        next.shared = true;
        self.set_link_record(hlc.max(held.hlc.saturating_add(1)), next)
    }

    /// Count one accepted request through a link, remembering the
    /// requester. `Ok(true)` when the register changed (send it); `Ok(false)`
    /// when no live record is held for the link.
    ///
    /// # Errors
    ///
    /// The [`super::entry::BoundsError`] the held record breaks (never, for
    /// a record the fold accepted).
    pub fn record_link_use(
        &mut self,
        hlc: u64,
        link_id: &[u8; LINK_RECORD_ID_LEN],
        requester_key: &[u8; LINK_RECORD_REQUESTER_KEY_LEN],
    ) -> Result<bool, super::entry::BoundsError> {
        let Some(held) = self.link_record(link_id) else {
            return Ok(false);
        };
        if held.value.revoked {
            return Ok(false);
        }
        let next = held.value.with_use(requester_key);
        self.set_link_record(hlc.max(held.hlc.saturating_add(1)), next)
    }

    /// The one rule for a contact request that carries a link answer
    /// (ADR-0079 section 3 as amended 2026-10-19, UX section 5):
    /// [`LinkAnswerVerdict::Accept`] only for a contact link of this account
    /// whose secret matches (compared in constant time) and that counts for
    /// a request written at `pressed_at_ms` (the server's stamp on the
    /// request row: not revoked now, not expired then, uses left);
    /// [`LinkAnswerVerdict::Answered`] when the requester was already
    /// accepted through it; [`LinkAnswerVerdict::Unknown`] when this device
    /// holds no record of the link; otherwise [`LinkAnswerVerdict::Dead`].
    #[must_use]
    pub fn link_answer_verdict(
        &self,
        link_id: &[u8; LINK_RECORD_ID_LEN],
        link_secret: &[u8; LINK_RECORD_SECRET_LEN],
        requester_key: &[u8; LINK_RECORD_REQUESTER_KEY_LEN],
        pressed_at_ms: u64,
    ) -> LinkAnswerVerdict {
        self.link_record(link_id)
            .map_or(LinkAnswerVerdict::Unknown, |r| {
                r.value.verdict(link_secret, requester_key, pressed_at_ms)
            })
    }

    /// The envelope carrying one held link record with its original stamp.
    #[must_use]
    pub fn link_record_envelope_for(&self, link_id: &[u8; LINK_RECORD_ID_LEN]) -> Option<Vec<u8>> {
        let held = self.link_record(link_id)?;
        compose_link_record_envelope(&[held]).ok()
    }

    /// Every held link record with its original stamp, as envelopes of at
    /// most [`LINK_RECORD_ENVELOPE_MAX_ENTRIES`] entries: the catch-up
    /// re-send after the DeviceGroup grew.
    #[must_use]
    pub fn link_record_envelopes(&self) -> Vec<Vec<u8>> {
        self.all_link_records()
            .chunks(LINK_RECORD_ENVELOPE_MAX_ENTRIES)
            .filter_map(|chunk| compose_link_record_envelope(chunk).ok())
            .collect()
    }

    /// Decode a link-record envelope and fold every register in with its
    /// original stamp. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`LinkRecordEnvelopeError`].
    pub fn apply_link_record_envelope(
        &mut self,
        bytes: &[u8],
    ) -> Result<bool, LinkRecordEnvelopeError> {
        let entries = decode_link_record_envelope(bytes)?;
        Ok(self.apply_all(
            entries
                .into_iter()
                .map(|s| (s.hlc, SyncEntry::LinkRecord(s.value))),
        ))
    }

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

    // -----------------------------------------------------------------
    // Conversation ends (ADR-0087 section 9).
    // -----------------------------------------------------------------

    /// The register to write now that this account's membership of
    /// `conversation` has ended, or `None` when there is nothing to tell the
    /// other devices. `epoch` is the MLS epoch the group froze at; `0` for a
    /// channel. Put the register in a change set, fold it with
    /// [`Self::apply_all`] (or the FFI's `applyFolderChanges`), send the same
    /// change set.
    ///
    /// `None` means: the register already says ended at this epoch or later;
    /// or it says joined at a later epoch, so this device is applying an old
    /// end and its account has rejoined since; or this conversation is new to
    /// the register and [`CONVERSATION_ENDS_MAX_CLIENT`] are held. In every
    /// case the device still ends the conversation on itself.
    ///
    /// The stamp is raised above a held one, so a write at an equal epoch,
    /// which is the channel case, always takes effect.
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::RefIdLength`].
    pub fn compose_conversation_ended(
        &self,
        hlc: u64,
        conversation: &ConversationRef,
        epoch: u64,
    ) -> Result<Option<Stamped<ConversationEnd>>, super::entry::BoundsError> {
        conversation.validate()?;
        let held = self.conversation_end(conversation);
        let write = match &held {
            None => self.conversation_end_count() < CONVERSATION_ENDS_MAX_CLIENT,
            Some(h) => match h.value.state {
                ConversationEndState::Ended => epoch > h.value.epoch,
                ConversationEndState::Joined => epoch >= h.value.epoch,
            },
        };
        Ok(write.then(|| {
            Self::stamped_end(
                hlc,
                held.as_ref(),
                conversation,
                ConversationEndState::Ended,
                epoch,
            )
        }))
    }

    /// The register to write now that this account has joined `conversation`,
    /// or `None` when there is nothing to clear. `epoch` is the MLS epoch the
    /// Welcome joined at; `0` for a channel.
    ///
    /// `None` means no end is held at or below this epoch: no register at
    /// all, so a first join writes nothing and the state does not grow with
    /// every group the person ever joined; or a join is already held; or the
    /// held end is later than this join and stands.
    ///
    /// # Errors
    ///
    /// [`super::entry::BoundsError::RefIdLength`].
    pub fn compose_conversation_joined(
        &self,
        hlc: u64,
        conversation: &ConversationRef,
        epoch: u64,
    ) -> Result<Option<Stamped<ConversationEnd>>, super::entry::BoundsError> {
        conversation.validate()?;
        let held = self.conversation_end(conversation);
        let write = held.as_ref().is_some_and(|h| match h.value.state {
            ConversationEndState::Ended => epoch >= h.value.epoch,
            ConversationEndState::Joined => epoch > h.value.epoch,
        });
        Ok(write.then(|| {
            Self::stamped_end(
                hlc,
                held.as_ref(),
                conversation,
                ConversationEndState::Joined,
                epoch,
            )
        }))
    }

    fn stamped_end(
        hlc: u64,
        held: Option<&Stamped<ConversationEnd>>,
        conversation: &ConversationRef,
        state: ConversationEndState,
        epoch: u64,
    ) -> Stamped<ConversationEnd> {
        Stamped {
            hlc: held.map_or(hlc, |h| hlc.max(h.hlc.saturating_add(1))),
            value: ConversationEnd {
                conversation: conversation.clone(),
                state,
                epoch,
            },
        }
    }

    /// The register held for a conversation, a "joined" included.
    #[must_use]
    pub fn conversation_end(
        &self,
        conversation: &ConversationRef,
    ) -> Option<Stamped<ConversationEnd>> {
        match self
            .registers
            .get(&RegisterKey::ConversationEnd(conversation.clone()))
        {
            Some(Held {
                hlc,
                entry: SyncEntry::ConversationEnd(e),
            }) => Some(Stamped {
                hlc: *hlc,
                value: e.clone(),
            }),
            _ => None,
        }
    }

    /// Whether this account's membership of a conversation has ended. `false`
    /// for one never ended and for one joined again since.
    #[must_use]
    pub fn is_conversation_ended(&self, conversation: &ConversationRef) -> bool {
        self.conversation_end(conversation)
            .is_some_and(|e| e.value.state == ConversationEndState::Ended)
    }

    /// Every conversation whose register says ended, by `(kind, id)`.
    #[must_use]
    pub fn ended_conversations(&self) -> Vec<Stamped<ConversationEnd>> {
        self.registers
            .values()
            .filter_map(|h| match &h.entry {
                SyncEntry::ConversationEnd(e) if e.state == ConversationEndState::Ended => {
                    Some(Stamped {
                        hlc: h.hlc,
                        value: e.clone(),
                    })
                },
                _ => None,
            })
            .collect()
    }

    /// How many conversation-end registers are held, "joined" ones included.
    fn conversation_end_count(&self) -> usize {
        self.registers
            .keys()
            .filter(|k| matches!(k, RegisterKey::ConversationEnd(_)))
            .count()
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
    /// Layout: `version(1) ‖ count(u32 BE) ‖ count × { hlc(u64 BE) ‖
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
    use super::super::lineage::LINEAGE_ENVELOPE_DOMAIN_V1;
    use super::super::own_username::OWN_USERNAME_ENVELOPE_DOMAIN_V1;
    use super::super::recovery_email::RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1;
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
    const ADDRESSES: [&str; 4] = ["", "a@b", "b@a", "änna@büro.example"];
    fn email(address: &str) -> SyncEntry {
        SyncEntry::RecoveryEmail(RecoveryEmailEntry {
            address: address.to_owned(),
        })
    }
    const USERNAMES: [&str; 4] = ["", "bob", "alice", "bob_42"];
    fn uname(username: &str) -> SyncEntry {
        SyncEntry::OwnUsername(OwnUsernameEntry {
            username: username.to_owned(),
        })
    }
    fn link(id: u8, uses: u32, revoked: bool, label: u8) -> SyncEntry {
        SyncEntry::LinkRecord(crate::synced::LinkRecordEntry {
            link_id: [id; 16],
            link_secret: [0x5C; 12],
            kind: crate::synced::LinkRecordKind::Contact,
            ledger_id: vec![7; 6],
            revocation_seed: vec![0x11; 32],
            expiry_ms: 0,
            max_uses: 3,
            uses,
            revoked,
            // Odd labels are shared, so the fold proptests cover that order.
            shared: label % 2 == 1,
            minted_at_ms: 0,
            label: "x".repeat(usize::from(label)),
            answered: vec![],
        })
    }

    /// ADR-0079 amendment of 2026-10-19: the moment the request was written
    /// decides expiry; a revocation decides until the account has answered.
    #[test]
    fn a_link_counts_by_the_request_stamp_and_a_revocation_wins_until_answered() {
        use crate::synced::LinkAnswerVerdict;
        let mut a = SyncedState::new();
        let SyncEntry::LinkRecord(mut record) = link(1, 0, false, 1) else {
            unreachable!()
        };
        record.expiry_ms = 1_000;
        assert_eq!(a.set_link_record(5, record), Ok(true));
        let id = [1u8; 16];
        let secret = [0x5C; 12];
        let bob = [0xB0; 32];
        // Written before the expiry, processed long after it: accepted.
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &bob, 999),
            LinkAnswerVerdict::Accept
        );
        // Written at or after the expiry: dead.
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &bob, 1_000),
            LinkAnswerVerdict::Dead
        );
        // Revoked before this account answered: dead even for a request
        // written while the link was live.
        assert_eq!(a.revoke_link_record(6, &id), Ok(true));
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &bob, 1),
            LinkAnswerVerdict::Dead
        );
        // Answered before the revocation: stays answered.
        let mut c = SyncedState::new();
        let SyncEntry::LinkRecord(record) = link(1, 0, false, 1) else {
            unreachable!()
        };
        assert_eq!(c.set_link_record(5, record), Ok(true));
        assert_eq!(c.record_link_use(6, &id, &bob), Ok(true));
        assert_eq!(c.revoke_link_record(7, &id), Ok(true));
        assert_eq!(
            c.link_answer_verdict(&id, &secret, &bob, 1),
            LinkAnswerVerdict::Answered
        );
    }

    #[test]
    fn a_copy_on_one_device_ends_editing_on_every_device() {
        let SyncEntry::LinkRecord(record) = link(3, 0, false, 2) else {
            unreachable!()
        };
        assert!(record.editable());
        let id = record.link_id;
        let mut a = SyncedState::new();
        let mut b = SyncedState::new();
        a.set_link_record(5, record.clone()).unwrap();
        b.set_link_record(5, record).unwrap();
        // Copied on a; b meanwhile writes a later, unshared change.
        assert_eq!(a.mark_link_shared(6, &id), Ok(true));
        assert_eq!(a.mark_link_shared(7, &id), Ok(false));
        let mut later = b.link_record(&id).unwrap().value;
        later.label = "renamed".into();
        b.set_link_record(50, later).unwrap();
        let shared = a.link_record_envelope_for(&id).unwrap();
        let unshared = b.link_record_envelope_for(&id).unwrap();
        assert_eq!(b.apply_link_record_envelope(&shared), Ok(true));
        assert_eq!(a.apply_link_record_envelope(&unshared), Ok(false));
        assert_eq!(a.all_link_records(), b.all_link_records());
        assert!(!a.link_record(&id).unwrap().value.editable());
        // A revoked link is never marked.
        assert_eq!(a.revoke_link_record(60, &id), Ok(true));
        assert_eq!(a.mark_link_shared(61, &id), Ok(false));
    }

    #[test]
    fn link_records_are_written_used_revoked_and_carried() {
        use crate::synced::{LinkAnswerVerdict, LinkRecordKind};
        let mut a = SyncedState::new();
        let SyncEntry::LinkRecord(record) = link(1, 0, false, 1) else {
            unreachable!()
        };
        assert_eq!(a.set_link_record(5, record.clone()), Ok(true));
        assert_eq!(a.set_link_record(5, record), Ok(false));
        let id = [1u8; 16];
        let secret = [0x5C; 12];
        let bob = [0xB0; 32];
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &bob, 0),
            LinkAnswerVerdict::Accept
        );
        assert_eq!(
            a.link_answer_verdict(&[2; 16], &secret, &bob, 0),
            LinkAnswerVerdict::Unknown
        );
        // A use, written on a stamp lower than the held one, still lands.
        assert_eq!(a.record_link_use(1, &id, &bob), Ok(true));
        let held = a.link_record(&id).unwrap();
        assert_eq!(held.value.uses, 1);
        assert_eq!(held.hlc, 6);
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &bob, 0),
            LinkAnswerVerdict::Answered
        );
        // Another device learns it from the envelope.
        let mut b = SyncedState::new();
        for e in a.link_record_envelopes() {
            assert_eq!(b.apply_link_record_envelope(&e), Ok(true));
        }
        assert_eq!(b.all_link_records(), a.all_link_records());
        // Revoke on b; a later live write on a cannot bring it back.
        assert_eq!(b.revoke_link_record(7, &id), Ok(true));
        assert_eq!(b.revoke_link_record(8, &id), Ok(false));
        assert_eq!(a.record_link_use(100, &id, &[0xB1; 32]), Ok(true));
        let live_later = a.link_record_envelope_for(&id).unwrap();
        assert_eq!(b.apply_link_record_envelope(&live_later), Ok(false));
        assert!(b.link_record(&id).unwrap().value.revoked);
        let revoked = b.link_record_envelope_for(&id).unwrap();
        assert_eq!(a.apply_link_record_envelope(&revoked), Ok(true));
        assert!(a.link_record(&id).unwrap().value.revoked);
        assert_eq!(a.record_link_use(200, &id, &[0xB2; 32]), Ok(false));
        assert_eq!(
            a.link_answer_verdict(&id, &secret, &[0xB3; 32], 0),
            LinkAnswerVerdict::Dead
        );
        // Persisted, and never in the snapshot.
        assert_eq!(SyncedState::from_bytes(&a.to_bytes()).unwrap(), a);
        assert!(a.snapshot().stamped_entries().is_empty());
        assert_eq!(held.value.kind, LinkRecordKind::Contact);
    }

    /// ADR-0079 as reworked 2026-10-17: the ledger row's revocation seed
    /// rides the record, so a device that did not mint the link holds it,
    /// keeps it through a use and through its own revocation, and can delete
    /// the ledger row itself.
    #[test]
    fn the_revocation_seed_reaches_every_device_and_survives_a_revocation() {
        let SyncEntry::LinkRecord(mut record) = link(1, 0, false, 1) else {
            unreachable!()
        };
        record.ledger_id = vec![6; 6];
        record.revocation_seed = vec![0xA7; 32];
        let id = record.link_id;
        let mut minting = SyncedState::new();
        assert_eq!(minting.set_link_record(5, record), Ok(true));
        let mut other = SyncedState::new();
        for e in minting.link_record_envelopes() {
            assert_eq!(other.apply_link_record_envelope(&e), Ok(true));
        }
        assert_eq!(
            other.link_record(&id).unwrap().value.revocation_seed,
            vec![0xA7; 32]
        );
        assert_eq!(other.record_link_use(6, &id, &[1; 32]), Ok(true));
        assert_eq!(other.revoke_link_record(7, &id), Ok(true));
        let revoked = other.link_record(&id).unwrap().value;
        assert!(revoked.revoked);
        assert_eq!(revoked.revocation_seed, vec![0xA7; 32]);
        let back = SyncedState::from_bytes(&other.to_bytes()).unwrap();
        assert_eq!(
            back.link_record(&id).unwrap().value.revocation_seed,
            vec![0xA7; 32]
        );
        // The revocation reaches the minting device with the seed intact.
        let env = other.link_record_envelope_for(&id).unwrap();
        assert_eq!(minting.apply_link_record_envelope(&env), Ok(true));
        assert_eq!(
            minting.link_record(&id).unwrap().value.revocation_seed,
            vec![0xA7; 32]
        );
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
                    sent_at_ms: 0,
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
        let mut old = s.to_bytes();
        assert_eq!(old[0], 9);
        for version in [1u8, 2, 3, 4, 5, 6, 7, 8] {
            old[0] = version;
            assert_eq!(
                SyncedState::from_bytes(&old),
                Err(CodecError::UnknownVersion)
            );
        }
        assert_eq!(SyncedState::new().to_bytes(), vec![9, 0, 0, 0, 0]);
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
    fn the_recovery_email_is_a_last_writer_wins_register() {
        use super::super::entry::BoundsError;
        let mut s = SyncedState::new();
        assert_eq!(s.recovery_email(), None);
        assert_eq!(s.recovery_email_register(), None);
        assert_eq!(s.recovery_email_envelope(), None);

        assert_eq!(s.set_recovery_email(5, Some("a@b")), Ok(true));
        assert_eq!(s.recovery_email().as_deref(), Some("a@b"));
        // The same write again, and an older one, change nothing.
        assert_eq!(s.set_recovery_email(5, Some("a@b")), Ok(false));
        assert_eq!(s.set_recovery_email(4, Some("old@b")), Ok(false));
        assert_eq!(s.recovery_email().as_deref(), Some("a@b"));
        // Not write-once: a newer address replaces it.
        assert_eq!(s.set_recovery_email(6, Some("new@b")), Ok(true));
        assert_eq!(s.recovery_email().as_deref(), Some("new@b"));
        assert_eq!(s.recovery_email_register().unwrap().hlc, 6);
        assert_eq!(s.len(), 1);

        // An invalid address is refused and changes nothing.
        assert_eq!(
            s.set_recovery_email(9, Some("ab")),
            Err(BoundsError::RecoveryEmailLength)
        );
        assert_eq!(
            s.set_recovery_email(9, Some("no-at-sign")),
            Err(BoundsError::RecoveryEmailShape)
        );
        assert_eq!(s.recovery_email().as_deref(), Some("new@b"));
    }

    #[test]
    fn a_removal_beats_an_older_address_and_a_newer_address_beats_an_older_removal() {
        let mut s = SyncedState::new();
        s.set_recovery_email(5, Some("a@b")).unwrap();
        assert_eq!(s.set_recovery_email(7, None), Ok(true));
        assert_eq!(s.recovery_email(), None);
        // The removal is kept, with its stamp.
        let held = s.recovery_email_register().unwrap();
        assert_eq!((held.hlc, held.value.is_removed()), (7, true));
        assert_eq!(s.len(), 1);
        // A stale device re-sends the address it still holds: refused, in
        // every form it can arrive in.
        assert_eq!(s.set_recovery_email(6, Some("a@b")), Ok(false));
        assert_eq!(s.apply(6, email("a@b")), Ok(ApplyOutcome::Superseded));
        let mut stale = SyncedState::new();
        stale.set_recovery_email(5, Some("a@b")).unwrap();
        assert_eq!(
            s.apply_recovery_email_envelope(&stale.recovery_email_envelope().unwrap()),
            Ok(false)
        );
        assert!(!s.merge_from(&stale));
        assert_eq!(s.recovery_email(), None);
        // And the stale device learns of the removal from any of them.
        assert!(stale.merge_from(&s));
        assert_eq!(stale.recovery_email(), None);

        // An empty address is the removal too.
        let mut t = SyncedState::new();
        t.set_recovery_email(1, Some("")).unwrap();
        assert!(t.recovery_email_register().unwrap().value.is_removed());

        // A newer address beats an older removal.
        assert_eq!(s.set_recovery_email(8, Some("again@b")), Ok(true));
        assert_eq!(s.recovery_email().as_deref(), Some("again@b"));
        // An older removal arriving late does not remove it.
        assert_eq!(s.apply(7, email("")), Ok(ApplyOutcome::Superseded));
        assert_eq!(s.recovery_email().as_deref(), Some("again@b"));
    }

    #[test]
    fn an_equal_stamp_on_the_recovery_email_is_decided_by_the_bytes_in_either_order() {
        let fold_of = |order: [&str; 3]| {
            let mut s = SyncedState::new();
            for a in order {
                s.apply(5, email(a)).ok();
            }
            s
        };
        let x = fold_of(["a@b", "", "b@a"]);
        assert_eq!(x, fold_of(["b@a", "a@b", ""]));
        assert_eq!(x, fold_of(["", "b@a", "a@b"]));
        // The larger canonical bytes win: the longer length prefix first.
        assert_eq!(x.recovery_email().as_deref(), Some("b@a"));
    }

    #[test]
    fn the_recovery_email_travels_by_envelope_and_persistence_never_by_snapshot() {
        let mut a = SyncedState::new();
        a.apply(1, meta(1, "Work")).ok();
        a.set_recovery_email(4, Some("änna@büro.example")).unwrap();
        assert!(a
            .snapshot()
            .stamped_entries()
            .iter()
            .all(|(_, e)| !matches!(e, SyncEntry::RecoveryEmail(_))));

        let live = a.recovery_email_envelope().unwrap();
        let mut b = SyncedState::new();
        assert_eq!(b.apply_recovery_email_envelope(&live), Ok(true));
        assert_eq!(b.apply_recovery_email_envelope(&live), Ok(false));
        assert_eq!(b.recovery_email_register(), a.recovery_email_register());
        // The original stamp travelled, so a re-send never beats a later write.
        assert_eq!(b.recovery_email_register().unwrap().hlc, 4);
        assert_eq!(b.recovery_email_envelope().unwrap(), live);

        // Through bytes (local persistence and the backup payload).
        let restored = SyncedState::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(restored, a);
        assert_eq!(
            restored.recovery_email().as_deref(),
            Some("änna@büro.example")
        );
        let mut merged = SyncedState::new();
        assert!(merged.merge_from(&restored));
        assert_eq!(merged, a);
        assert_eq!(a.max_hlc(), 4);

        // A removal round-trips through all three as a removal.
        a.set_recovery_email(9, None).unwrap();
        let removal = a.recovery_email_envelope().unwrap();
        assert_eq!(b.apply_recovery_email_envelope(&removal), Ok(true));
        assert_eq!(b.recovery_email(), None);
        let restored = SyncedState::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(
            restored.recovery_email_register(),
            a.recovery_email_register()
        );
        assert!(restored
            .recovery_email_register()
            .unwrap()
            .value
            .is_removed());

        // A malformed envelope changes nothing.
        let mut bad = removal;
        bad.push(0);
        let before = b.clone();
        assert_eq!(
            b.apply_recovery_email_envelope(&bad),
            Err(RecoveryEmailEnvelopeError::Trailing)
        );
        assert_eq!(b, before);
        // Nor does a state whose register breaks a bound: the whole state
        // is refused.
        let mut bytes = vec![STATE_VERSION, 0, 0, 0, 1];
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        bytes.extend_from_slice(&[0, 0, 0, 7, 12, 0, 0, 0, 2, b'a', b'@']);
        assert_eq!(
            SyncedState::from_bytes(&bytes),
            Err(CodecError::Bounds(
                super::super::entry::BoundsError::RecoveryEmailLength
            ))
        );
    }

    fn channel_reaction(c: u8, cursor: u64, emoji: &str, reacted: bool) -> SyncEntry {
        SyncEntry::ChannelReaction(ChannelReactionEntry {
            channel_id: [c; 16],
            post_cursor: cursor,
            emoji: emoji.to_string(),
            reacted,
            post_at_ms: 1_000_u64.saturating_add(cursor),
        })
    }

    fn channel_comment(c: u8, cursor: u64, nonce: u8, deleted: bool) -> SyncEntry {
        SyncEntry::ChannelComment(ChannelCommentEntry {
            channel_id: [c; 16],
            post_cursor: cursor,
            comment_nonce: [nonce; 16],
            deleted,
            post_at_ms: 1_000_u64.saturating_add(cursor),
        })
    }

    #[test]
    fn an_own_comment_is_recorded_deleted_carried_and_pruned() {
        use super::super::channel_comment::CHANNEL_COMMENT_RETENTION_MS;
        let ch = [4u8; 16];
        let mut a = SyncedState::new();
        let sent = a.record_channel_comment(1, ch, 7, [1; 16], 500).unwrap();
        // Write-once.
        assert!(a.record_channel_comment(2, ch, 7, [1; 16], 500).is_none());
        a.record_channel_comment(3, ch, 7, [2; 16], 500).unwrap();
        assert_eq!(a.channel_comments_on(&ch, 7), vec![[1; 16], [2; 16]]);
        assert!(a.channel_comments_on(&ch, 8).is_empty());
        // Another device learns the nonce and can delete the comment.
        let mut b = SyncedState::new();
        assert_eq!(b.apply_channel_comment_envelope(&sent), Ok(true));
        assert_eq!(b.apply_channel_comment_envelope(&sent), Ok(false));
        let gone = b.delete_channel_comment(4, ch, 7, [1; 16], 500).unwrap();
        assert!(b.delete_channel_comment(5, ch, 7, [1; 16], 500).is_none());
        assert_eq!(a.apply_channel_comment_envelope(&gone), Ok(true));
        assert_eq!(a.channel_comments_on(&ch, 7), vec![[2; 16]]);
        // The older record arriving late does not bring it back.
        assert_eq!(a.apply_channel_comment_envelope(&sent), Ok(false));
        assert_eq!(a.channel_comments_on(&ch, 7), vec![[2; 16]]);
        // Persisted, never in the snapshot, re-sent whole.
        assert!(a.snapshot().folders.is_empty());
        let back = SyncedState::from_bytes(&a.to_bytes()).unwrap_or_default();
        assert_eq!(back.entries(), a.entries());
        assert_eq!(a.channel_comment_envelopes().len(), 1);
        // Pruned with the post.
        a.set_channel_reaction(6, ch, 7, "a", true, 500).ok();
        assert_eq!(
            a.prune_channel_activity(500 + CHANNEL_COMMENT_RETENTION_MS),
            0
        );
        assert_eq!(
            a.prune_channel_activity(501 + CHANNEL_COMMENT_RETENTION_MS),
            3
        );
        assert!(a.is_empty());
    }

    #[test]
    fn on_an_equal_stamp_a_comment_deletion_wins_in_either_order() {
        let live = channel_comment(1, 7, 3, false);
        let gone = channel_comment(1, 7, 3, true);
        let mut x = SyncedState::new();
        x.apply(5, live.clone()).ok();
        x.apply(5, gone.clone()).ok();
        let mut y = SyncedState::new();
        y.apply(5, gone).ok();
        y.apply(5, live).ok();
        assert_eq!(x, y);
        assert!(x.channel_comments_on(&[1; 16], 7).is_empty());
    }

    #[test]
    fn an_own_reaction_is_set_taken_off_carried_and_persisted() {
        let ch = [4u8; 16];
        let thumbs = "\u{1F44D}";
        let mut a = SyncedState::new();
        assert!(!a.has_channel_reaction(&ch, 7, thumbs));
        // Taking off a reaction never given writes nothing.
        assert_eq!(
            a.set_channel_reaction(1, ch, 7, thumbs, false, 500),
            Ok(None)
        );
        let sent = a
            .set_channel_reaction(2, ch, 7, thumbs, true, 500)
            .unwrap()
            .unwrap();
        assert!(a.has_channel_reaction(&ch, 7, thumbs));
        assert_eq!(a.channel_reactions_on(&ch, 7), vec![thumbs.to_string()]);
        assert!(a.channel_reactions_on(&ch, 8).is_empty());
        // The same state again writes nothing.
        assert_eq!(
            a.set_channel_reaction(3, ch, 7, thumbs, true, 500),
            Ok(None)
        );
        // Another device folds it in, idempotently.
        let mut b = SyncedState::new();
        assert_eq!(b.apply_channel_reaction_envelope(&sent), Ok(true));
        assert_eq!(b.apply_channel_reaction_envelope(&sent), Ok(false));
        assert!(b.has_channel_reaction(&ch, 7, thumbs));
        // Taken off on b; the tombstone reaches a and beats the older add.
        let off = b
            .set_channel_reaction(4, ch, 7, thumbs, false, 500)
            .unwrap()
            .unwrap();
        assert_eq!(a.apply_channel_reaction_envelope(&off), Ok(true));
        assert!(!a.has_channel_reaction(&ch, 7, thumbs));
        // The older add, arriving late, does not bring it back.
        assert_eq!(a.apply_channel_reaction_envelope(&sent), Ok(false));
        assert!(!a.has_channel_reaction(&ch, 7, thumbs));
        // Never in the snapshot, but persisted and re-sent.
        assert!(a.snapshot().folders.is_empty());
        let back = SyncedState::from_bytes(&a.to_bytes()).unwrap_or_default();
        assert_eq!(back.entries(), a.entries());
        assert_eq!(a.channel_reaction_envelopes(), vec![off]);
        assert_eq!(
            a.set_channel_reaction(5, ch, 7, "", true, 500),
            Err(super::super::entry::BoundsError::ChannelReactionEmoji)
        );
    }

    #[test]
    fn a_fourth_reaction_on_one_post_is_refused_and_a_removal_makes_room() {
        use super::super::entry::BoundsError;
        let ch = [4u8; 16];
        let mut s = SyncedState::new();
        for (i, e) in ["a", "b", "c"].iter().enumerate() {
            assert!(s
                .set_channel_reaction(i as u64 + 1, ch, 7, e, true, 500)
                .unwrap()
                .is_some());
        }
        assert_eq!(
            s.set_channel_reaction(9, ch, 7, "d", true, 500),
            Err(BoundsError::TooManyChannelReactions)
        );
        // Re-stating a held one is not a fourth, and another post is free.
        assert_eq!(s.set_channel_reaction(10, ch, 7, "a", true, 500), Ok(None));
        assert!(s
            .set_channel_reaction(11, ch, 8, "d", true, 500)
            .unwrap()
            .is_some());
        // Taking one off makes room.
        assert!(s
            .set_channel_reaction(12, ch, 7, "b", false, 500)
            .unwrap()
            .is_some());
        assert!(s
            .set_channel_reaction(13, ch, 7, "d", true, 500)
            .unwrap()
            .is_some());
        // The fold itself never refuses a fourth (a concurrent device).
        assert_eq!(
            s.apply(14, channel_reaction(4, 7, "e", true)),
            Ok(ApplyOutcome::Applied)
        );
    }

    #[test]
    fn on_an_equal_stamp_taking_a_reaction_off_wins_in_either_order() {
        let add = channel_reaction(1, 7, "a", true);
        let off = channel_reaction(1, 7, "a", false);
        let mut x = SyncedState::new();
        x.apply(5, add.clone()).ok();
        x.apply(5, off.clone()).ok();
        let mut y = SyncedState::new();
        y.apply(5, off).ok();
        y.apply(5, add).ok();
        assert_eq!(x, y);
        assert!(!x.has_channel_reaction(&[1; 16], 7, "a"));
    }

    #[test]
    fn a_post_past_its_retention_takes_its_reactions_with_it() {
        use super::super::channel_reaction::CHANNEL_REACTION_RETENTION_MS;
        let mut s = SyncedState::new();
        s.set_channel_reaction(1, [1; 16], 7, "a", true, 1_000).ok();
        s.set_channel_reaction(2, [1; 16], 8, "a", true, 9_000).ok();
        s.set_channel_reaction(3, [1; 16], 8, "b", false, 9_000)
            .ok();
        s.acknowledge_device(4, [9; 32], 0);
        assert_eq!(
            s.prune_channel_activity(1_000 + CHANNEL_REACTION_RETENTION_MS),
            0
        );
        assert_eq!(
            s.prune_channel_activity(1_001 + CHANNEL_REACTION_RETENTION_MS),
            1
        );
        assert!(!s.has_channel_reaction(&[1; 16], 7, "a"));
        assert!(s.has_channel_reaction(&[1; 16], 8, "a"));
        assert_eq!(s.prune_channel_activity(u64::MAX), 1);
        // Other registers are untouched.
        assert!(s.is_device_acknowledged(&[9; 32]));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn the_client_limit_stops_a_new_reaction_and_never_a_removal() {
        use super::super::channel_reaction::CHANNEL_REACTIONS_MAX_CLIENT;
        let mut s = SyncedState::new();
        for cursor in 0..CHANNEL_REACTIONS_MAX_CLIENT as u64 {
            s.apply(1, channel_reaction(1, cursor, "a", true)).ok();
        }
        assert_eq!(
            s.set_channel_reaction(2, [1; 16], u64::MAX, "a", true, 0),
            Ok(None)
        );
        assert!(s
            .set_channel_reaction(2, [1; 16], 0, "a", false, 0)
            .unwrap()
            .is_some());
        // The fold itself never refuses.
        assert_eq!(
            s.apply(2, channel_reaction(1, u64::MAX, "a", true)),
            Ok(ApplyOutcome::Applied)
        );
    }

    #[test]
    fn a_device_acknowledgement_is_write_once_carried_and_persisted() {
        let mut a = SyncedState::new();
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        assert!(!a.is_device_acknowledged(&k1));
        let sent = a.acknowledge_devices(10, &[k2, k1, k1], 1_000);
        assert_eq!(sent.len(), 1);
        assert!(a.is_device_acknowledged(&k1) && a.is_device_acknowledged(&k2));
        // Write-once: a second acknowledgement writes and sends nothing.
        assert!(a.acknowledge_devices(20, &[k1], 2_000).is_empty());
        // Carried: another device folds it in, idempotently.
        let mut b = SyncedState::new();
        assert_eq!(b.apply_device_ack_envelope(&sent[0]), Ok(true));
        assert_eq!(b.apply_device_ack_envelope(&sent[0]), Ok(false));
        assert_eq!(b.acknowledged_devices(), vec![k1, k2]);
        assert_eq!(b.device_ack_envelopes(), sent);
        // Never in the snapshot, but persisted.
        assert!(a.snapshot().folders.is_empty());
        let back = SyncedState::from_bytes(&a.to_bytes()).unwrap_or_default();
        assert_eq!(back.acknowledged_devices(), vec![k1, k2]);
        // Two devices acknowledging concurrently converge.
        let mut c = SyncedState::new();
        let other = c.acknowledge_devices(10, &[k1], 5_000);
        let mut ab = a.clone();
        assert!(ab.apply_device_ack_envelope(&other[0]).is_ok());
        assert!(c.apply_device_ack_envelope(&sent[0]).is_ok());
        assert_eq!(ab.entries(), c.entries());
        assert!(b
            .apply_device_ack_envelope(b"lakko/sync/device-ack/v1\0\x02")
            .is_err());
    }

    #[test]
    fn the_own_username_is_a_last_writer_wins_register() {
        use super::super::entry::BoundsError;
        let mut s = SyncedState::new();
        assert_eq!(s.own_username(), None);
        assert_eq!(s.own_username_register(), None);
        assert_eq!(s.own_username_envelope(), None);

        assert_eq!(s.set_own_username(5, Some("bob")), Ok(true));
        assert_eq!(s.own_username().as_deref(), Some("bob"));
        assert_eq!(s.set_own_username(5, Some("bob")), Ok(false));
        assert_eq!(s.set_own_username(4, Some("old_name")), Ok(false));
        // Not write-once: a rename replaces it.
        assert_eq!(s.set_own_username(6, Some("robert")), Ok(true));
        assert_eq!(s.own_username().as_deref(), Some("robert"));
        assert_eq!(s.own_username_register().unwrap().hlc, 6);
        assert_eq!(s.len(), 1);

        for bad in ["ab", "Bob", "bo b", "@bob"] {
            assert_eq!(
                s.set_own_username(9, Some(bad)),
                Err(BoundsError::OwnUsername)
            );
        }
        assert_eq!(s.own_username().as_deref(), Some("robert"));
        // It shares nothing with the recovery email register.
        s.set_recovery_email(7, Some("a@b")).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.own_username().as_deref(), Some("robert"));
    }

    #[test]
    fn no_username_beats_an_older_name_and_a_newer_name_beats_an_older_none() {
        let mut s = SyncedState::new();
        s.set_own_username(5, Some("bob")).unwrap();
        assert_eq!(s.set_own_username(7, None), Ok(true));
        assert_eq!(s.own_username(), None);
        let held = s.own_username_register().unwrap();
        assert_eq!((held.hlc, held.value.is_none()), (7, true));
        assert_eq!(s.len(), 1);
        // A stale device re-sends the name it still holds: refused, in
        // every form it can arrive in.
        assert_eq!(s.set_own_username(6, Some("bob")), Ok(false));
        assert_eq!(s.apply(6, uname("bob")), Ok(ApplyOutcome::Superseded));
        let mut stale = SyncedState::new();
        stale.set_own_username(5, Some("bob")).unwrap();
        assert_eq!(
            s.apply_own_username_envelope(&stale.own_username_envelope().unwrap()),
            Ok(false)
        );
        assert!(!s.merge_from(&stale));
        assert_eq!(s.own_username(), None);
        assert!(stale.merge_from(&s));
        assert_eq!(stale.own_username(), None);

        let mut t = SyncedState::new();
        t.set_own_username(1, Some("")).unwrap();
        assert!(t.own_username_register().unwrap().value.is_none());

        // A newer name beats an older "none", and a late older "none" does
        // not clear it. The same holds for a rename: the old name arriving
        // late does not come back.
        assert_eq!(s.set_own_username(8, Some("robert")), Ok(true));
        assert_eq!(s.apply(7, uname("")), Ok(ApplyOutcome::Superseded));
        assert_eq!(s.apply(5, uname("bob")), Ok(ApplyOutcome::Superseded));
        assert_eq!(s.own_username().as_deref(), Some("robert"));
    }

    #[test]
    fn an_equal_stamp_on_the_own_username_is_decided_by_the_bytes_in_either_order() {
        let fold_of = |order: [&str; 3]| {
            let mut s = SyncedState::new();
            for u in order {
                s.apply(5, uname(u)).ok();
            }
            s
        };
        let x = fold_of(["bob", "", "alice"]);
        assert_eq!(x, fold_of(["alice", "bob", ""]));
        assert_eq!(x, fold_of(["", "alice", "bob"]));
        // The larger canonical bytes win: the longer length prefix first.
        assert_eq!(x.own_username().as_deref(), Some("alice"));
    }

    #[test]
    fn the_own_username_travels_by_envelope_and_persistence_never_by_snapshot() {
        let mut a = SyncedState::new();
        a.apply(1, meta(1, "Work")).ok();
        a.set_own_username(4, Some("bob_42")).unwrap();
        assert!(a
            .snapshot()
            .stamped_entries()
            .iter()
            .all(|(_, e)| !matches!(e, SyncEntry::OwnUsername(_))));

        let live = a.own_username_envelope().unwrap();
        let mut b = SyncedState::new();
        assert_eq!(b.apply_own_username_envelope(&live), Ok(true));
        assert_eq!(b.apply_own_username_envelope(&live), Ok(false));
        assert_eq!(b.own_username_register(), a.own_username_register());
        assert_eq!(b.own_username_register().unwrap().hlc, 4);
        assert_eq!(b.own_username_envelope().unwrap(), live);
        // The envelope kinds do not cross.
        assert_eq!(
            b.apply_recovery_email_envelope(&live),
            Err(RecoveryEmailEnvelopeError::WrongDomain)
        );

        let restored = SyncedState::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(restored, a);
        assert_eq!(restored.own_username().as_deref(), Some("bob_42"));
        let mut merged = SyncedState::new();
        assert!(merged.merge_from(&restored));
        assert_eq!(merged, a);

        a.set_own_username(9, None).unwrap();
        let none = a.own_username_envelope().unwrap();
        assert_eq!(b.apply_own_username_envelope(&none), Ok(true));
        assert_eq!(b.own_username(), None);
        let restored = SyncedState::from_bytes(&a.to_bytes()).unwrap();
        assert!(restored.own_username_register().unwrap().value.is_none());

        let mut bad = none;
        bad.push(0);
        let before = b.clone();
        assert_eq!(
            b.apply_own_username_envelope(&bad),
            Err(OwnUsernameEnvelopeError::Trailing)
        );
        assert_eq!(b, before);
        // A persisted state whose register breaks the rule is refused whole.
        let mut bytes = vec![STATE_VERSION, 0, 0, 0, 1];
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        bytes.extend_from_slice(&[0, 0, 0, 8, 13, 0, 0, 0, 3, b'B', b'o', b'b']);
        assert_eq!(
            SyncedState::from_bytes(&bytes),
            Err(CodecError::Bounds(
                super::super::entry::BoundsError::OwnUsername
            ))
        );
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
            SyncedState::from_bytes(&[0xFF, 0, 0, 0, 0]),
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
            (hlc.clone(), c.clone(), 10u8..13).prop_map(|(h, s, r)| (h, lineage(s, r))),
            // One register for the whole account: addresses and removals
            // from several writers collide on few stamps.
            (hlc.clone(), prop::sample::select(ADDRESSES.to_vec()))
                .prop_map(|(h, a)| (h, email(a))),
            (hlc.clone(), prop::sample::select(USERNAMES.to_vec()))
                .prop_map(|(h, u)| (h, uname(u))),
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
            (hlc.clone(), c.clone(), 0u64..2).prop_map(|(h, c, m)| (h, unsave(c, m))),
            // Link records: two links, uses, a revocation, so live writes
            // meet revocations on every stamp order.
            (hlc.clone(), 0u8..2, 0u32..3, any::<bool>(), c.clone())
                .prop_map(|(h, id, uses, revoked, label)| (h, link(id, uses, revoked, label))),
            // Conversation ends: few conversations and epochs, so ends and
            // joins meet on one epoch and on one stamp often.
            (hlc.clone(), c.clone(), 0u64..3, any::<bool>())
                .prop_map(|(h, c, epoch, ended)| (h, conversation_end(c, epoch, ended))),
            // Own channel reactions: two posts x two emoji, so a reaction and
            // its removal meet on one stamp often.
            (
                hlc.clone(),
                c.clone(),
                0u64..2,
                prop::sample::select(vec!["a", "b"]),
                any::<bool>()
            )
                .prop_map(|(h, c, cursor, e, reacted)| (
                    h,
                    channel_reaction(c, cursor, e, reacted)
                )),
            // Own channel comments: two posts x two nonces, a comment and its
            // deletion meeting on one stamp often.
            (hlc, c, 0u64..2, 0u8..2, any::<bool>())
                .prop_map(|(h, c, cursor, n, deleted)| (h, channel_comment(c, cursor, n, deleted))),
        ]
    }

    /// Everything a fresh device is sent to catch up: the snapshot's entries,
    /// the lineage envelopes and the recovery-email envelope.
    fn catch_up(from: &SyncedState, into: &mut SyncedState) {
        into.apply_all(from.snapshot().stamped_entries());
        for envelope in from.lineage_envelopes() {
            into.apply_lineage_envelope(&envelope).unwrap();
        }
        if let Some(envelope) = from.recovery_email_envelope() {
            into.apply_recovery_email_envelope(&envelope).unwrap();
        }
        if let Some(envelope) = from.own_username_envelope() {
            into.apply_own_username_envelope(&envelope).unwrap();
        }
        for envelope in from.link_record_envelopes() {
            into.apply_link_record_envelope(&envelope).unwrap();
        }
        for envelope in from.channel_reaction_envelopes() {
            into.apply_channel_reaction_envelope(&envelope).unwrap();
        }
        for envelope in from.channel_comment_envelopes() {
            into.apply_channel_comment_envelope(&envelope).unwrap();
        }
    }

    fn fold(entries: &[(u64, SyncEntry)]) -> SyncedState {
        let mut s = SyncedState::new();
        s.apply_all(entries.iter().cloned());
        s
    }

    fn end_value(c: u8, epoch: u64, ended: bool) -> ConversationEnd {
        ConversationEnd {
            conversation: cref(c),
            state: if ended {
                ConversationEndState::Ended
            } else {
                ConversationEndState::Joined
            },
            epoch,
        }
    }

    fn conversation_end(c: u8, epoch: u64, ended: bool) -> SyncEntry {
        SyncEntry::ConversationEnd(end_value(c, epoch, ended))
    }

    /// `n` distinct conversations, each ended at epoch 1.
    fn with_ended_conversations(n: u32) -> SyncedState {
        fold(
            &(0..n)
                .map(|i| {
                    (
                        1,
                        SyncEntry::ConversationEnd(ConversationEnd {
                            conversation: ConversationRef {
                                kind: ConversationRefKind::Group,
                                id: i.to_be_bytes().repeat(8),
                            },
                            state: ConversationEndState::Ended,
                            epoch: 1,
                        }),
                    )
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The case that put the epoch in the register. A person is removed at
    /// epoch 5 and added again at epoch 9, all while one device is offline.
    /// That device comes back, applies the old removal, and only then writes
    /// "ended", with the newest stamp of all. By stamps it would end the
    /// conversation on every device that has rejoined.
    #[test]
    fn a_device_applying_an_old_removal_does_not_end_a_rejoined_conversation() {
        let mut rejoined = fold(&[(10, conversation_end(1, 5, true))]);
        let join = rejoined
            .compose_conversation_joined(20, &cref(1), 9)
            .unwrap()
            .expect("an end is held, so the join writes");
        assert_eq!(join.value.state, ConversationEndState::Joined);
        rejoined.apply_all(vec![(
            join.hlc,
            SyncEntry::ConversationEnd(join.value.clone()),
        )]);
        assert!(!rejoined.is_conversation_ended(&cref(1)));

        // The returning device has heard the join: it has nothing to tell.
        assert_eq!(
            rejoined.compose_conversation_ended(30, &cref(1), 5),
            Ok(None)
        );
        // It has not heard it: its write arrives anyway, with a newer stamp.
        let late = fold(&[(30, conversation_end(1, 5, true))]);
        assert!(late.is_conversation_ended(&cref(1)));
        let mut merged = rejoined.clone();
        assert!(!merged.merge_from(&late));
        assert_eq!(merged, rejoined);
        assert!(!merged.is_conversation_ended(&cref(1)));
        // And on the returning device the join wins too.
        let mut there = late;
        assert!(there.merge_from(&rejoined));
        assert!(!there.is_conversation_ended(&cref(1)));
    }

    /// The other direction: a second removal is a real end whatever the
    /// clocks say, and a device whose clock runs slow does not lose it.
    #[test]
    fn a_later_end_stands_against_a_newer_stamped_join() {
        let s = fold(&[
            (50, conversation_end(1, 9, false)),
            (40, conversation_end(1, 12, true)),
        ]);
        assert!(s.is_conversation_ended(&cref(1)));
        assert_eq!(
            s.conversation_end(&cref(1)).map(|e| (e.hlc, e.value.epoch)),
            Some((40, 12))
        );
        let written = s
            .compose_conversation_joined(60, &cref(1), 14)
            .unwrap()
            .expect("a join after the end clears it");
        assert_eq!(written.value.epoch, 14);
        // A Welcome older than the end clears nothing.
        assert_eq!(s.compose_conversation_joined(60, &cref(1), 11), Ok(None));
    }

    #[test]
    fn writers_say_when_there_is_nothing_to_tell() {
        let empty = SyncedState::new();
        // A first join leaves no trace, so the state does not grow with
        // every group the person ever joined.
        assert_eq!(empty.compose_conversation_joined(1, &cref(1), 3), Ok(None));
        let written = empty
            .compose_conversation_ended(1, &cref(1), 3)
            .unwrap()
            .expect("the first end writes");
        assert_eq!((written.hlc, written.value.epoch), (1, 3));

        let s = fold(&[(5, conversation_end(1, 3, true))]);
        // The same end from a second device of the account.
        assert_eq!(s.compose_conversation_ended(9, &cref(1), 3), Ok(None));
        assert_eq!(s.compose_conversation_ended(9, &cref(1), 2), Ok(None));
        assert!(s
            .compose_conversation_ended(9, &cref(1), 4)
            .unwrap()
            .is_some());

        let s = fold(&[(5, conversation_end(1, 3, false))]);
        assert_eq!(s.compose_conversation_joined(9, &cref(1), 3), Ok(None));

        let no_id = ConversationRef {
            kind: ConversationRefKind::Group,
            id: vec![],
        };
        assert!(empty.compose_conversation_ended(1, &no_id, 0).is_err());
        assert!(empty.compose_conversation_joined(1, &no_id, 0).is_err());
    }

    /// A channel has no epoch: its registers are ordered by stamps alone,
    /// and a writer always lands above what it holds.
    #[test]
    fn a_channel_is_ordered_by_stamps_and_a_tie_keeps_the_conversation() {
        let channel = ConversationRef {
            kind: ConversationRefKind::Channel,
            id: vec![9; 16],
        };
        let end = |ended: bool| {
            SyncEntry::ConversationEnd(ConversationEnd {
                conversation: channel.clone(),
                state: if ended {
                    ConversationEndState::Ended
                } else {
                    ConversationEndState::Joined
                },
                epoch: 0,
            })
        };
        assert!(fold(&[(5, end(true)), (6, end(false))])
            .conversation_end(&channel)
            .is_some_and(|e| e.value.state == ConversationEndState::Joined));
        assert!(fold(&[(7, end(true)), (6, end(false))]).is_conversation_ended(&channel));
        // A complete tie, in either order.
        assert!(!fold(&[(5, end(true)), (5, end(false))]).is_conversation_ended(&channel));
        assert!(!fold(&[(5, end(false)), (5, end(true))]).is_conversation_ended(&channel));

        // Re-subscribing on a device whose clock is behind still clears.
        let s = fold(&[(50, end(true))]);
        let join = s
            .compose_conversation_joined(3, &channel, 0)
            .unwrap()
            .expect("a join clears the end");
        assert_eq!(join.hlc, 51);
        // And a later unsubscribe the same way.
        let s = fold(&[(51, end(false))]);
        let again = s
            .compose_conversation_ended(3, &channel, 0)
            .unwrap()
            .expect("an end after a join writes");
        assert_eq!(again.hlc, 52);
    }

    #[test]
    fn conversation_end_reads() {
        let s = fold(&[
            (5, conversation_end(1, 3, true)),
            (6, conversation_end(2, 4, false)),
            (7, conversation_end(3, 2, true)),
        ]);
        assert!(s.is_conversation_ended(&cref(1)));
        assert!(!s.is_conversation_ended(&cref(2)));
        assert!(!s.is_conversation_ended(&cref(4)));
        assert_eq!(
            s.ended_conversations(),
            vec![
                Stamped {
                    hlc: 5,
                    value: end_value(1, 3, true),
                },
                Stamped {
                    hlc: 7,
                    value: end_value(3, 2, true),
                },
            ]
        );
        assert_eq!(s.max_hlc(), 7);
    }

    /// The register reaches every place a state travels: the snapshot a late
    /// device gets, a restored backup, and the persisted bytes.
    #[test]
    fn a_conversation_end_travels_with_the_state() {
        let s = fold(&[
            (5, conversation_end(1, 3, true)),
            (6, conversation_end(2, 4, false)),
        ]);
        let snap = s.snapshot();
        assert_eq!(snap.folders.conversation_ends.len(), 2);
        let mut late = SyncedState::new();
        catch_up(&s, &mut late);
        assert_eq!(late, s);

        let restored = SyncedState::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(restored, s);
        assert!(
            crate::fuzz_api::decode_synced_and_check(&s.to_bytes()),
            "the fuzz oracle holds on a state with ends in it"
        );
        let mut other = fold(&[(9, conversation_end(4, 1, true))]);
        assert!(other.merge_from(&restored));
        assert!(other.is_conversation_ended(&cref(1)));
        assert!(other.is_conversation_ended(&cref(4)));
    }

    /// `CONVERSATION_ENDS_MAX_CLIENT` is the writer's limit. At it a new
    /// conversation writes nothing, a held one still does, and the fold
    /// refuses nothing, so two devices over the limit together still converge.
    #[test]
    fn the_client_limit_stops_the_writer_and_never_the_fold() {
        let limit = u32::try_from(CONVERSATION_ENDS_MAX_CLIENT).unwrap();
        let below = with_ended_conversations(limit - 1);
        let fresh = cref(0xEE);
        assert!(below
            .compose_conversation_ended(9, &fresh, 1)
            .unwrap()
            .is_some());

        let at = with_ended_conversations(limit);
        assert_eq!(at.compose_conversation_ended(9, &fresh, 1), Ok(None));
        // A conversation already held is no new register.
        let held = at.ended_conversations()[0].value.conversation.clone();
        assert!(at
            .compose_conversation_ended(9, &held, 2)
            .unwrap()
            .is_some());
        assert!(at
            .compose_conversation_joined(9, &held, 2)
            .unwrap()
            .is_some());

        let mut over = at;
        assert_eq!(
            over.apply(9, conversation_end(0xEE, 1, true)),
            Ok(ApplyOutcome::Applied)
        );
        assert_eq!(over.ended_conversations().len(), limit as usize + 1);
        assert_eq!(SyncedState::from_bytes(&over.to_bytes()), Ok(over));
    }

    /// The size [`CONVERSATION_ENDS_MAX_CLIENT`]'s documentation states.
    /// Two registers landed from two branches with one tag number. They hold
    /// their own tags now; a persisted state keeps both.
    #[test]
    fn a_conversation_end_and_a_device_ack_persist_side_by_side() {
        let mut s = fold(&[(5, conversation_end(1, 3, true))]);
        let device = [7u8; DEVICE_ACK_KEY_LEN];
        assert!(s.acknowledge_device(6, device, 1_000));

        let restored = SyncedState::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(restored, s);
        assert!(restored.is_conversation_ended(&cref(1)));
        assert!(restored.is_device_acknowledged(&device));
    }

    #[test]
    fn a_conversation_end_is_56_bytes_at_rest() {
        let empty = SyncedState::new().to_bytes().len();
        assert_eq!(
            with_ended_conversations(1).to_bytes().len() - empty,
            56,
            "hlc 8, length 4, tag 1, kind 1, id length 1, id 32, state 1, epoch 8"
        );
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
            prop_assert_eq!(a.recovery_email_register(), b.recovery_email_register());
            prop_assert_eq!(a.own_username_register(), b.own_username_register());
            prop_assert_eq!(a.saved(), b.saved());
            prop_assert_eq!(a.saved_blob_ids(), b.saved_blob_ids());
            prop_assert_eq!(a.all_link_records(), b.all_link_records());
        }

        /// ADR-0079: a link revoked by any write stays revoked whatever
        /// arrives after it, in any order.
        #[test]
        fn a_revocation_is_final_in_any_order(
            entries in prop::collection::vec(arb_entry(), 0..24),
        ) {
            let s = fold(&entries);
            for id in 0u8..2 {
                let revoked_somewhere = entries.iter().any(|(_, e)| {
                    matches!(e, SyncEntry::LinkRecord(r) if r.link_id == [id; 16] && r.revoked)
                });
                let held = s.link_record(&[id; 16]).map(|r| r.value.revoked);
                if revoked_somewhere {
                    prop_assert_eq!(held, Some(true));
                }
            }
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
            prop_assert_eq!(lr.recovery_email_register(), rl.recovery_email_register());
            prop_assert_eq!(lr.own_username_register(), rl.own_username_register());
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

        /// The recovery email alone: any arrival order of any writers'
        /// envelopes (addresses and removals) converges on the greatest
        /// `(stamp, canonical bytes)` written, applying twice changes
        /// nothing, and the held envelope rebuilds the register.
        #[test]
        fn recovery_email_envelopes_converge_in_any_order(
            writes in prop::collection::vec((0u64..6, 0usize..ADDRESSES.len()), 1..20),
            seed in any::<u64>(),
        ) {
            let envelopes: Vec<Vec<u8>> = writes
                .iter()
                .map(|(h, a)| {
                    compose_recovery_email_envelope(&Stamped {
                        hlc: *h,
                        value: RecoveryEmailEntry { address: ADDRESSES[*a].to_owned() },
                    })
                    .unwrap()
                })
                .collect();
            let mut a = SyncedState::new();
            for e in &envelopes {
                a.apply_recovery_email_envelope(e).unwrap();
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
                b.apply_recovery_email_envelope(e).unwrap();
            }
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(a.len(), 1);
            let held = a.recovery_email_register().unwrap();
            let best = writes
                .iter()
                .map(|(h, i)| (*h, email(ADDRESSES[*i]).canonical_bytes()))
                .max()
                .unwrap();
            prop_assert_eq!(
                (held.hlc, SyncEntry::RecoveryEmail(held.value.clone()).canonical_bytes()),
                best
            );
            prop_assert_eq!(a.recovery_email().is_none(), held.value.is_removed());
            let mut rebuilt = SyncedState::new();
            prop_assert!(rebuilt
                .apply_recovery_email_envelope(&a.recovery_email_envelope().unwrap())
                .unwrap());
            prop_assert_eq!(rebuilt, a);
        }

        /// The recovery-email envelope decoder is total, and whatever it
        /// accepts is canonical: it re-encodes to the identical bytes.
        #[test]
        fn recovery_email_envelope_decode_is_total_and_canonical(
            noise in prop::collection::vec(any::<u8>(), 0..96),
            hlc in any::<u64>(),
            pick in 0usize..ADDRESSES.len(),
            at in any::<usize>(),
            flip in 1u8..=255,
        ) {
            let mut prefixed = RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1.to_vec();
            prefixed.push(1);
            prefixed.extend_from_slice(&noise);
            for candidate in [noise.clone(), prefixed] {
                if let Ok(decoded) = decode_recovery_email_envelope(&candidate) {
                    prop_assert_eq!(compose_recovery_email_envelope(&decoded).unwrap(), candidate);
                }
            }
            let register = Stamped {
                hlc,
                value: RecoveryEmailEntry { address: ADDRESSES[pick].to_owned() },
            };
            let mut wire = compose_recovery_email_envelope(&register).unwrap();
            prop_assert_eq!(decode_recovery_email_envelope(&wire).unwrap(), register);
            let i = at % wire.len();
            prop_assert!(decode_recovery_email_envelope(&wire[..i]).is_err());
            wire[i] ^= flip;
            if let Ok(decoded) = decode_recovery_email_envelope(&wire) {
                prop_assert_eq!(compose_recovery_email_envelope(&decoded).unwrap(), wire);
            }
        }

        /// The own username alone: any arrival order of any writers'
        /// envelopes (names and "none") converges on the greatest `(stamp,
        /// canonical bytes)` written, applying twice changes nothing, and
        /// the held envelope rebuilds the register.
        #[test]
        fn own_username_envelopes_converge_in_any_order(
            writes in prop::collection::vec((0u64..6, 0usize..USERNAMES.len()), 1..20),
            seed in any::<u64>(),
        ) {
            let envelopes: Vec<Vec<u8>> = writes
                .iter()
                .map(|(h, u)| {
                    compose_own_username_envelope(&Stamped {
                        hlc: *h,
                        value: OwnUsernameEntry { username: USERNAMES[*u].to_owned() },
                    })
                    .unwrap()
                })
                .collect();
            let mut a = SyncedState::new();
            for e in &envelopes {
                a.apply_own_username_envelope(e).unwrap();
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
                b.apply_own_username_envelope(e).unwrap();
            }
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(a.len(), 1);
            let held = a.own_username_register().unwrap();
            let best = writes
                .iter()
                .map(|(h, i)| (*h, uname(USERNAMES[*i]).canonical_bytes()))
                .max()
                .unwrap();
            prop_assert_eq!(
                (held.hlc, SyncEntry::OwnUsername(held.value.clone()).canonical_bytes()),
                best
            );
            prop_assert_eq!(a.own_username().is_none(), held.value.is_none());
            let mut rebuilt = SyncedState::new();
            prop_assert!(rebuilt
                .apply_own_username_envelope(&a.own_username_envelope().unwrap())
                .unwrap());
            prop_assert_eq!(rebuilt, a);
        }

        /// The own-username envelope decoder is total, and whatever it
        /// accepts is canonical: it re-encodes to the identical bytes.
        #[test]
        fn own_username_envelope_decode_is_total_and_canonical(
            noise in prop::collection::vec(any::<u8>(), 0..96),
            hlc in any::<u64>(),
            pick in 0usize..USERNAMES.len(),
            at in any::<usize>(),
            flip in 1u8..=255,
        ) {
            let mut prefixed = OWN_USERNAME_ENVELOPE_DOMAIN_V1.to_vec();
            prefixed.push(1);
            prefixed.extend_from_slice(&noise);
            for candidate in [noise.clone(), prefixed] {
                if let Ok(decoded) = decode_own_username_envelope(&candidate) {
                    prop_assert_eq!(compose_own_username_envelope(&decoded).unwrap(), candidate);
                }
            }
            let register = Stamped {
                hlc,
                value: OwnUsernameEntry { username: USERNAMES[pick].to_owned() },
            };
            let mut wire = compose_own_username_envelope(&register).unwrap();
            prop_assert_eq!(decode_own_username_envelope(&wire).unwrap(), register);
            let i = at % wire.len();
            prop_assert!(decode_own_username_envelope(&wire[..i]).is_err());
            wire[i] ^= flip;
            if let Ok(decoded) = decode_own_username_envelope(&wire) {
                prop_assert_eq!(compose_own_username_envelope(&decoded).unwrap(), wire);
            }
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
