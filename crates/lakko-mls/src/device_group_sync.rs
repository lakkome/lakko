// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Proto conversion for the two ADR-0058 DeviceGroup payload kinds
//! (`FolderChanges`, `StateSnapshot`), and the snapshot split.
//!
//! The typed values are `lakko-sync`'s plain types; this module is the only
//! place they meet `devicegroup_v1.proto`. Decoding is **fail closed**: a
//! payload that breaks one ADR-0058 section 4 bound is refused whole with
//! [`Error::PayloadBounds`], never truncated. The bounds themselves are
//! `lakko-sync`'s ([`FolderChanges::validate`], [`StateSnapshot::validate`]),
//! so they are written once.
//!
//! No MLS state-machine change: these are two more bodies for the existing
//! `create_devicegroup_message` / `parse_devicegroup_payload`.

use lakko_proto::mls_app::devicegroup::v1 as pb;
use lakko_sync::synced::{
    folder_id_from_slice, BoundsError, ConversationEntry, ConversationOrderEntry, ConversationRef,
    ConversationRefKind, DraftEntry, FlagsEntry, FolderChanges, FolderMember, FolderMeta,
    FolderOrder, FolderPins, MemberState, ReadCursorEntry, Stamped, StateSnapshot,
    FOLDER_CHANGES_MAX_REGISTERS, SNAPSHOT_MAX_PARTS,
};
use prost::Message as _;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Encode (typed -> proto).
// ---------------------------------------------------------------------------

fn ref_to_proto(r: &ConversationRef) -> pb::ConversationRef {
    pb::ConversationRef {
        kind: i32::from(r.kind.to_wire()),
        id: r.id.clone(),
    }
}

pub(crate) fn folder_changes_to_proto(c: &FolderChanges) -> pb::FolderChanges {
    pb::FolderChanges {
        metas: c
            .metas
            .iter()
            .map(|s| pb::FolderMeta {
                hlc: s.hlc,
                folder_id: s.value.folder_id.to_vec(),
                name: s.value.name.clone(),
                include_kinds: s.value.include_kinds,
                exclude_flags: s.value.exclude_flags,
            })
            .collect(),
        members: c
            .members
            .iter()
            .map(|s| pb::FolderMember {
                hlc: s.hlc,
                folder_id: s.value.folder_id.to_vec(),
                conversation: Some(ref_to_proto(&s.value.conversation)),
                state: i32::from(s.value.state.to_wire()),
            })
            .collect(),
        pins: c
            .pins
            .iter()
            .map(|s| pb::FolderPins {
                hlc: s.hlc,
                folder_id: s.value.folder_id.to_vec(),
                pinned: s.value.pinned.iter().map(ref_to_proto).collect(),
            })
            .collect(),
        removed: c
            .removed
            .iter()
            .map(|s| pb::FolderRemoved {
                hlc: s.hlc,
                folder_id: s.value.to_vec(),
            })
            .collect(),
        order: c.order.as_ref().map(|s| pb::FolderOrder {
            hlc: s.hlc,
            folder_ids: s.value.folder_ids.iter().map(|f| f.to_vec()).collect(),
        }),
    }
}

fn conversation_entry_to_proto(s: &Stamped<ConversationEntry>) -> pb::state_snapshot::Stamped {
    use pb::state_snapshot::stamped::Entry;
    let entry = match &s.value {
        ConversationEntry::Flags(f) => Entry::Flags(pb::ThreadFlags {
            group_id: f.group_id.clone(),
            pinned: f.pinned,
            muted: f.muted,
            archived: f.archived,
            unread_override: f.unread_override,
        }),
        ConversationEntry::ReadCursor(c) => Entry::Cursor(pb::ReadCursor {
            group_id: c.group_id.clone(),
            up_to_message_id: c.up_to_message_id.clone(),
            read_at_ms: c.read_at_ms,
        }),
        ConversationEntry::Draft(d) => Entry::Draft(pb::DraftUpdate {
            group_id: d.group_id.clone(),
            text: d.text.clone(),
            reply_to_id: d.reply_to_id.clone(),
        }),
    };
    pb::state_snapshot::Stamped {
        hlc: s.hlc,
        entry: Some(entry),
    }
}

pub(crate) fn state_snapshot_to_proto(s: &StateSnapshot) -> pb::StateSnapshot {
    pb::StateSnapshot {
        entries: s.entries.iter().map(conversation_entry_to_proto).collect(),
        conversation_order: s.conversation_order.as_ref().map(|o| {
            pb::state_snapshot::StampedOrder {
                hlc: o.hlc,
                order: Some(pb::ConversationOrder {
                    group_ids: o.value.group_ids.clone(),
                }),
            }
        }),
        folders: if s.folders.is_empty() {
            None
        } else {
            Some(folder_changes_to_proto(&s.folders))
        },
        part: s.part,
        parts: s.parts,
    }
}

// ---------------------------------------------------------------------------
// Decode (proto -> typed), fail closed.
// ---------------------------------------------------------------------------

fn bounds(e: BoundsError) -> Error {
    Error::PayloadBounds(e)
}

fn ref_from_proto(r: Option<pb::ConversationRef>) -> Result<ConversationRef> {
    let r = r.ok_or_else(|| bounds(BoundsError::Missing))?;
    let kind = ConversationRefKind::from_wire(i64::from(r.kind)).map_err(bounds)?;
    let r = ConversationRef { kind, id: r.id };
    r.validate().map_err(bounds)?;
    Ok(r)
}

pub(crate) fn folder_changes_from_proto(c: pb::FolderChanges) -> Result<FolderChanges> {
    // Count before converting anything, so an oversized payload costs no
    // per-register work.
    let count = c.metas.len()
        + c.members.len()
        + c.pins.len()
        + c.removed.len()
        + usize::from(c.order.is_some());
    if count > FOLDER_CHANGES_MAX_REGISTERS {
        return Err(bounds(BoundsError::TooManyRegisters));
    }
    let mut out = FolderChanges::default();
    for m in c.metas {
        out.metas.push(Stamped {
            hlc: m.hlc,
            value: FolderMeta {
                folder_id: folder_id_from_slice(&m.folder_id).map_err(bounds)?,
                name: m.name,
                include_kinds: m.include_kinds,
                exclude_flags: m.exclude_flags,
            },
        });
    }
    for m in c.members {
        out.members.push(Stamped {
            hlc: m.hlc,
            value: FolderMember {
                folder_id: folder_id_from_slice(&m.folder_id).map_err(bounds)?,
                conversation: ref_from_proto(m.conversation)?,
                state: MemberState::from_wire(i64::from(m.state)).map_err(bounds)?,
            },
        });
    }
    for p in c.pins {
        let folder_id = folder_id_from_slice(&p.folder_id).map_err(bounds)?;
        if p.pinned.len() > lakko_sync::synced::FOLDER_PINS_MAX {
            return Err(bounds(BoundsError::TooManyPins));
        }
        let pinned = p
            .pinned
            .into_iter()
            .map(|r| ref_from_proto(Some(r)))
            .collect::<Result<Vec<_>>>()?;
        out.pins.push(Stamped {
            hlc: p.hlc,
            value: FolderPins { folder_id, pinned },
        });
    }
    for r in c.removed {
        out.removed.push(Stamped {
            hlc: r.hlc,
            value: folder_id_from_slice(&r.folder_id).map_err(bounds)?,
        });
    }
    if let Some(o) = c.order {
        if o.folder_ids.len() > lakko_sync::synced::FOLDER_ORDER_MAX {
            return Err(bounds(BoundsError::OrderTooLong));
        }
        let folder_ids = o
            .folder_ids
            .iter()
            .map(|f| folder_id_from_slice(f).map_err(bounds))
            .collect::<Result<Vec<_>>>()?;
        out.order = Some(Stamped {
            hlc: o.hlc,
            value: FolderOrder { folder_ids },
        });
    }
    out.validate().map_err(bounds)?;
    Ok(out)
}

pub(crate) fn state_snapshot_from_proto(s: pb::StateSnapshot) -> Result<StateSnapshot> {
    use pb::state_snapshot::stamped::Entry;
    if s.parts == 0 || s.parts > SNAPSHOT_MAX_PARTS || s.part >= s.parts {
        return Err(bounds(BoundsError::SnapshotPart));
    }
    let mut entries = Vec::with_capacity(s.entries.len());
    for e in s.entries {
        let value = match e.entry.ok_or_else(|| bounds(BoundsError::Missing))? {
            Entry::Flags(f) => ConversationEntry::Flags(FlagsEntry {
                group_id: f.group_id,
                pinned: f.pinned,
                muted: f.muted,
                archived: f.archived,
                unread_override: f.unread_override,
            }),
            Entry::Cursor(c) => ConversationEntry::ReadCursor(ReadCursorEntry {
                group_id: c.group_id,
                up_to_message_id: c.up_to_message_id,
                read_at_ms: c.read_at_ms,
            }),
            Entry::Draft(d) => ConversationEntry::Draft(DraftEntry {
                group_id: d.group_id,
                text: d.text,
                reply_to_id: d.reply_to_id,
            }),
        };
        entries.push(Stamped { hlc: e.hlc, value });
    }
    let conversation_order = match s.conversation_order {
        None => None,
        Some(o) => Some(Stamped {
            hlc: o.hlc,
            value: ConversationOrderEntry {
                group_ids: o
                    .order
                    .ok_or_else(|| bounds(BoundsError::Missing))?
                    .group_ids,
            },
        }),
    };
    let folders = match s.folders {
        None => FolderChanges::default(),
        Some(f) => folder_changes_from_proto(f)?,
    };
    let snap = StateSnapshot {
        entries,
        conversation_order,
        folders,
        part: s.part,
        parts: s.parts,
    };
    snap.validate().map_err(bounds)?;
    Ok(snap)
}

// ---------------------------------------------------------------------------
// The split (ADR-0058 section 5.1).
// ---------------------------------------------------------------------------

/// Bytes reserved per part for everything but the items: the
/// `DeviceGroupPayload` envelope (hlc, sender leaf, oneof key and length),
/// the `folders` sub-message header, and `part` / `parts`. Generous on
/// purpose; the split's own test checks real encoded sizes.
const PART_OVERHEAD: usize = 64;

/// Length of a protobuf varint.
const fn varint_len(mut v: usize) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// Cost of one length-delimited field holding `len` bytes, with a one-byte
/// key (every field number here is below 16).
const fn field_cost(len: usize) -> usize {
    1 + varint_len(len) + len
}

enum Item {
    Conversation(Stamped<ConversationEntry>),
    Order(Stamped<ConversationOrderEntry>),
    Folder(u64, lakko_sync::synced::SyncEntry),
}

/// Split one whole-state snapshot ([`lakko_sync::SyncedState::snapshot`])
/// into parts whose encoded `DeviceGroupPayload` is at most
/// `max_part_bytes` each, and whose folder registers stay within the
/// 1024-register cap. Every entry keeps its original stamp; parts are
/// independent and fold on their own.
///
/// A single entry larger than `max_part_bytes` (a very long draft) gets a
/// part of its own; nothing is truncated.
///
/// An empty state yields one empty part, so a receiver still learns the
/// sender's state is empty rather than missing.
///
/// # Errors
///
/// [`Error::PayloadBounds`] ([`BoundsError::SnapshotPart`]) when the state
/// needs more than 64 parts.
pub fn split_state_snapshot(
    snapshot: StateSnapshot,
    max_part_bytes: usize,
) -> Result<Vec<StateSnapshot>> {
    let mut items: Vec<Item> = snapshot
        .entries
        .into_iter()
        .map(Item::Conversation)
        .collect();
    if let Some(o) = snapshot.conversation_order {
        items.push(Item::Order(o));
    }
    items.extend(
        snapshot
            .folders
            .stamped_entries()
            .into_iter()
            .map(|(h, e)| Item::Folder(h, e)),
    );

    let empty = || StateSnapshot {
        entries: Vec::new(),
        conversation_order: None,
        folders: FolderChanges::default(),
        part: 0,
        parts: 1,
    };
    let mut parts: Vec<StateSnapshot> = Vec::new();
    let mut cur = empty();
    let mut cur_bytes = PART_OVERHEAD;
    let mut cur_items = 0usize;

    for item in items {
        let (cost, is_folder) = match &item {
            Item::Conversation(s) => (
                field_cost(conversation_entry_to_proto(s).encoded_len()),
                false,
            ),
            Item::Order(o) => (
                field_cost(
                    pb::state_snapshot::StampedOrder {
                        hlc: o.hlc,
                        order: Some(pb::ConversationOrder {
                            group_ids: o.value.group_ids.clone(),
                        }),
                    }
                    .encoded_len(),
                ),
                false,
            ),
            Item::Folder(h, e) => {
                let mut one = FolderChanges::default();
                let _ = one.push(*h, e.clone());
                // The one-register FolderChanges costs its field; the item
                // inside it costs its inner field, which that length is an
                // upper bound of.
                (folder_changes_to_proto(&one).encoded_len(), true)
            },
        };
        let over_bytes = cur_bytes + cost > max_part_bytes;
        let over_regs = is_folder && cur.folders.register_count() >= FOLDER_CHANGES_MAX_REGISTERS;
        if cur_items > 0 && (over_bytes || over_regs) {
            parts.push(core::mem::replace(&mut cur, empty()));
            cur_bytes = PART_OVERHEAD;
            cur_items = 0;
        }
        match item {
            Item::Conversation(s) => cur.entries.push(s),
            Item::Order(o) => cur.conversation_order = Some(o),
            Item::Folder(h, e) => {
                let _ = cur.folders.push(h, e);
            },
        }
        cur_bytes += cost;
        cur_items += 1;
    }
    parts.push(cur);

    let n = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    if n > SNAPSHOT_MAX_PARTS {
        return Err(bounds(BoundsError::SnapshotPart));
    }
    for (i, p) in parts.iter_mut().enumerate() {
        p.part = u32::try_from(i).unwrap_or(u32::MAX);
        p.parts = n;
    }
    Ok(parts)
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    clippy::semicolon_if_nothing_returned,
    clippy::map_unwrap_or
)]
mod tests {
    use super::*;
    use crate::device_group::DeviceGroupBody;
    use lakko_sync::synced::{SyncEntry, SyncedState, INCLUDE_GROUPS, SNAPSHOT_MAX_PART_BYTES};

    fn big_state(drafts: usize, text_len: usize, members: usize) -> SyncedState {
        let mut s = SyncedState::new();
        for i in 0..drafts {
            let id = u32::try_from(i).unwrap_or(0).to_be_bytes().to_vec();
            s.apply(
                1,
                SyncEntry::Draft(DraftEntry {
                    group_id: id,
                    text: "x".repeat(text_len),
                    reply_to_id: vec![],
                }),
            )
            .ok();
        }
        s.apply(
            1,
            SyncEntry::FolderMeta(FolderMeta {
                folder_id: [1; 16],
                name: "Work".into(),
                include_kinds: INCLUDE_GROUPS,
                exclude_flags: 0,
            }),
        )
        .ok();
        for i in 0..members {
            let id = u32::try_from(i).unwrap_or(0).to_be_bytes().to_vec();
            s.apply(
                2,
                SyncEntry::FolderMember(FolderMember {
                    folder_id: [1; 16],
                    conversation: ConversationRef {
                        kind: ConversationRefKind::Group,
                        id,
                    },
                    state: MemberState::Included,
                }),
            )
            .ok();
        }
        s
    }

    fn envelope_len(p: &StateSnapshot) -> usize {
        pb::DeviceGroupPayload {
            hlc: u64::MAX,
            sender_leaf: u32::MAX,
            body: Some(pb::device_group_payload::Body::StateSnapshot(
                state_snapshot_to_proto(p),
            )),
        }
        .encoded_len()
    }

    #[test]
    fn parts_stay_under_the_cap_and_rebuild_the_state() {
        let s = big_state(300, 2_000, 1_500);
        let parts = split_state_snapshot(s.snapshot(), SNAPSHOT_MAX_PART_BYTES).expect("split");
        assert!(parts.len() > 1);
        let mut rebuilt = SyncedState::new();
        for (i, p) in parts.iter().enumerate() {
            assert!(envelope_len(p) <= SNAPSHOT_MAX_PART_BYTES, "part {i}");
            assert!(p.folders.register_count() <= FOLDER_CHANGES_MAX_REGISTERS);
            assert_eq!(p.parts as usize, parts.len());
            // Through the wire and back.
            let back = state_snapshot_from_proto(state_snapshot_to_proto(p)).expect("decode");
            rebuilt.apply_all(back.stamped_entries());
        }
        assert_eq!(rebuilt, s);
    }

    #[test]
    fn an_empty_state_is_one_empty_part() {
        let parts = split_state_snapshot(SyncedState::new().snapshot(), 1024).expect("split");
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].part, parts[0].parts), (0, 1));
    }

    #[test]
    fn more_than_64_parts_is_refused() {
        let s = big_state(100, 500, 0);
        let err = split_state_snapshot(s.snapshot(), 600).expect_err("too many parts");
        assert!(matches!(
            err,
            Error::PayloadBounds(BoundsError::SnapshotPart)
        ));
    }

    // -----------------------------------------------------------------
    // Decode bounds, through the real parser.
    // -----------------------------------------------------------------

    fn wire(body: pb::device_group_payload::Body) -> Vec<u8> {
        pb::DeviceGroupPayload {
            hlc: 1,
            sender_leaf: 0,
            body: Some(body),
        }
        .encode_to_vec()
    }

    fn good_meta() -> pb::FolderMeta {
        pb::FolderMeta {
            hlc: 5,
            folder_id: vec![1; 16],
            name: "Work".into(),
            include_kinds: 0b011,
            exclude_flags: 0b100,
        }
    }

    fn good_ref() -> pb::ConversationRef {
        pb::ConversationRef {
            kind: 3,
            id: vec![9; 32],
        }
    }

    fn changes_with(f: impl FnOnce(&mut pb::FolderChanges)) -> Vec<u8> {
        let mut c = pb::FolderChanges {
            metas: vec![good_meta()],
            members: vec![pb::FolderMember {
                hlc: 6,
                folder_id: vec![1; 16],
                conversation: Some(good_ref()),
                state: 2,
            }],
            pins: vec![pb::FolderPins {
                hlc: 7,
                folder_id: vec![1; 16],
                pinned: vec![good_ref()],
            }],
            removed: vec![pb::FolderRemoved {
                hlc: 8,
                folder_id: vec![2; 16],
            }],
            order: Some(pb::FolderOrder {
                hlc: 9,
                folder_ids: vec![vec![1; 16]],
            }),
        };
        f(&mut c);
        wire(pb::device_group_payload::Body::FolderChanges(c))
    }

    fn refused(bytes: &[u8]) -> Option<BoundsError> {
        match crate::DeviceGroup::parse_devicegroup_payload(bytes) {
            Err(Error::PayloadBounds(e)) => Some(e),
            _ => None,
        }
    }

    #[test]
    fn a_well_formed_change_set_decodes_with_per_register_stamps() {
        let parsed =
            crate::DeviceGroup::parse_devicegroup_payload(&changes_with(|_| {})).expect("decodes");
        let entries = parsed.stamped_entries();
        let stamps: Vec<u64> = entries.iter().map(|(h, _)| *h).collect();
        // The envelope stamp (1) is ignored; each register keeps its own.
        assert_eq!(stamps, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    fn every_bound_refuses_the_whole_payload() {
        use BoundsError as B;
        let cases: Vec<(Vec<u8>, B)> = vec![
            (
                changes_with(|c| c.metas[0].folder_id = vec![1; 15]),
                B::FolderIdLength,
            ),
            (
                changes_with(|c| c.members[0].folder_id = vec![1; 17]),
                B::FolderIdLength,
            ),
            (
                changes_with(|c| c.removed[0].folder_id = vec![]),
                B::FolderIdLength,
            ),
            (
                changes_with(|c| {
                    c.order
                        .as_mut()
                        .map(|o| o.folder_ids.push(vec![0; 3]))
                        .unwrap_or(())
                }),
                B::FolderIdLength,
            ),
            (
                changes_with(|c| c.metas[0].name = String::new()),
                B::NameEmpty,
            ),
            (
                changes_with(|c| c.metas[0].name = "a".repeat(25)),
                B::NameTooLong,
            ),
            (
                changes_with(|c| c.metas[0].name = "a\nb".into()),
                B::NameCharacters,
            ),
            (
                changes_with(|c| c.metas[0].include_kinds = 0b1000),
                B::UnknownBits,
            ),
            (
                changes_with(|c| c.metas[0].exclude_flags = 1 << 20),
                B::UnknownBits,
            ),
            (
                changes_with(|c| {
                    c.members[0].conversation = Some(pb::ConversationRef {
                        kind: 0,
                        id: vec![1],
                    })
                }),
                B::RefKindUnspecified,
            ),
            (
                changes_with(|c| {
                    c.members[0].conversation = Some(pb::ConversationRef {
                        kind: 9,
                        id: vec![1],
                    })
                }),
                B::RefKindUnspecified,
            ),
            (
                changes_with(|c| {
                    c.members[0].conversation = Some(pb::ConversationRef {
                        kind: 1,
                        id: vec![],
                    })
                }),
                B::RefIdLength,
            ),
            (
                changes_with(|c| c.pins[0].pinned[0].id = vec![0; 65]),
                B::RefIdLength,
            ),
            (
                changes_with(|c| c.members[0].conversation = None),
                B::Missing,
            ),
            (
                changes_with(|c| c.members[0].state = 3),
                B::UnknownMemberState,
            ),
            (
                changes_with(|c| c.pins[0].pinned = vec![good_ref(); 11]),
                B::TooManyPins,
            ),
            (
                changes_with(|c| {
                    c.order = Some(pb::FolderOrder {
                        hlc: 1,
                        folder_ids: vec![vec![1; 16]; 257],
                    })
                }),
                B::OrderTooLong,
            ),
            (
                changes_with(|c| {
                    c.removed = vec![
                        pb::FolderRemoved {
                            hlc: 1,
                            folder_id: vec![2; 16]
                        };
                        1021
                    ];
                }),
                B::TooManyRegisters,
            ),
        ];
        for (i, (bytes, want)) in cases.into_iter().enumerate() {
            assert_eq!(refused(&bytes), Some(want), "case {i}");
        }
        // At the limits: accepted.
        assert!(
            crate::DeviceGroup::parse_devicegroup_payload(&changes_with(|c| {
                c.metas[0].name = "😀".repeat(24);
                c.pins[0].pinned = vec![good_ref(); 10];
                c.members[0]
                    .conversation
                    .as_mut()
                    .map(|r| r.id = vec![0; 64])
                    .unwrap_or(());
                c.order = Some(pb::FolderOrder {
                    hlc: 1,
                    folder_ids: vec![vec![1; 16]; 256],
                });
                c.removed = vec![
                    pb::FolderRemoved {
                        hlc: 1,
                        folder_id: vec![2; 16]
                    };
                    1020
                ];
            }))
            .is_ok()
        );
    }

    #[test]
    fn the_fuzz_oracle_holds_on_valid_and_refused_payloads() {
        use crate::fuzz_api::devicegroup_payload_accepted_within_bounds as oracle;
        assert!(oracle(&changes_with(|_| {})));
        assert!(oracle(&changes_with(|c| c.metas[0].name = String::new())));
        assert!(oracle(&snapshot_wire(0, 1, |_| {})));
        // A deterministic byte sweep: nothing the decoder accepts may break
        // a bound.
        let base = changes_with(|_| {});
        for i in 0..base.len() {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut b = base.clone();
                b[i] ^= flip;
                assert!(oracle(&b), "byte {i} flip {flip:#x}");
            }
        }
    }

    fn snapshot_wire(part: u32, parts: u32, f: impl FnOnce(&mut pb::StateSnapshot)) -> Vec<u8> {
        let mut s = pb::StateSnapshot {
            entries: vec![pb::state_snapshot::Stamped {
                hlc: 3,
                entry: Some(pb::state_snapshot::stamped::Entry::Flags(pb::ThreadFlags {
                    group_id: vec![1; 32],
                    pinned: true,
                    muted: false,
                    archived: false,
                    unread_override: false,
                })),
            }],
            conversation_order: Some(pb::state_snapshot::StampedOrder {
                hlc: 4,
                order: Some(pb::ConversationOrder {
                    group_ids: vec![vec![1; 32]],
                }),
            }),
            folders: Some(pb::FolderChanges {
                metas: vec![good_meta()],
                ..Default::default()
            }),
            part,
            parts,
        };
        f(&mut s);
        wire(pb::device_group_payload::Body::StateSnapshot(s))
    }

    #[test]
    fn snapshot_bounds() {
        assert!(
            crate::DeviceGroup::parse_devicegroup_payload(&snapshot_wire(0, 1, |_| {})).is_ok()
        );
        assert!(
            crate::DeviceGroup::parse_devicegroup_payload(&snapshot_wire(63, 64, |_| {})).is_ok()
        );
        assert_eq!(
            refused(&snapshot_wire(1, 1, |_| {})),
            Some(BoundsError::SnapshotPart)
        );
        assert_eq!(
            refused(&snapshot_wire(0, 0, |_| {})),
            Some(BoundsError::SnapshotPart)
        );
        assert_eq!(
            refused(&snapshot_wire(0, 65, |_| {})),
            Some(BoundsError::SnapshotPart)
        );
        assert_eq!(
            refused(&snapshot_wire(0, 1, |s| s.entries[0].entry = None)),
            Some(BoundsError::Missing)
        );
        assert_eq!(
            refused(&snapshot_wire(0, 1, |s| {
                s.conversation_order
                    .as_mut()
                    .map(|o| o.order = None)
                    .unwrap_or(());
            })),
            Some(BoundsError::Missing)
        );
        assert_eq!(
            refused(&snapshot_wire(0, 1, |s| {
                s.folders
                    .as_mut()
                    .map(|f| f.metas[0].name = String::new())
                    .unwrap_or(());
            })),
            Some(BoundsError::NameEmpty)
        );
    }

    proptest::proptest! {
        /// Whatever state a device holds, composing it as a DeviceGroup
        /// snapshot and parsing it back yields the same registers.
        #[test]
        fn a_composed_snapshot_parses_back_to_the_same_entries(
            n_drafts in 0usize..20,
            n_members in 0usize..40,
            text_len in 0usize..50,
        ) {
            let s = big_state(n_drafts, text_len, n_members);
            for part in split_state_snapshot(s.snapshot(), SNAPSHOT_MAX_PART_BYTES).expect("split") {
                let body = DeviceGroupBody::StateSnapshot(part.clone());
                let bytes = pb::DeviceGroupPayload {
                    hlc: 1,
                    sender_leaf: 0,
                    body: Some(body.into_proto()),
                }
                .encode_to_vec();
                let parsed = crate::DeviceGroup::parse_devicegroup_payload(&bytes).expect("parse");
                proptest::prop_assert_eq!(parsed.body, DeviceGroupBody::StateSnapshot(part));
            }
        }
    }
}
