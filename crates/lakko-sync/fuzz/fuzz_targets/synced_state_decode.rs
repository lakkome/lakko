// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the byte decoders of the synced state (`lakko-sync`).
//!
//! Nine decoders run on bytes this device did not write:
//!
//! * `decode_lineage_envelope` (ADR-0073 R.5): a conversation-lineage
//!   envelope another device of the same account sent over the DeviceGroup's
//!   opaque service-envelope carrier. Domain, version (judged before the
//!   count), a bounded count, one-byte length prefixes checked against their
//!   bound before any copy, strictly ascending successors, no trailing bytes.
//! * `decode_recovery_email_envelope` (ADR-0075, amended 2026-10-13): the
//!   account's recovery email register, sent the same way. Domain, version
//!   (judged before the rest), a stamp, a one-byte length checked against
//!   its bound before any copy, no trailing bytes, UTF-8, the address shape.
//! * `decode_own_username_envelope` (ADR-0076 section 3): the account's own
//!   username register, sent the same way and laid out the same; the value
//!   must be empty or a canonical username.
//! * `decode_quick_reaction_envelope` (UX `reactions-and-emoji.md` D3): the
//!   account's quick reaction, sent the same way and laid out the same; the
//!   value is 1 to 64 bytes of UTF-8.
//! * `decode_link_record_envelope` (ADR-0079 section 4): the account's own
//!   invite links, sent the same way (version `0x02` since the record carries
//!   the ledger row's revocation seed, 0 or 32 bytes, reworked 2026-10-17;
//!   the persisted state is version 4). Domain, version, a bounded count,
//!   every length checked against its bound before any copy, strictly
//!   ascending link ids, no trailing bytes; a revoked record it carries must
//!   be held revoked after the fold. Version `0x04` since 2026-11-05 (ADR-0087
//!   phase B): a group, channel or publisher link carries its target id, a
//!   private channel link's invite seed and admission version, and the sealed
//!   `/g/` blob, each length checked before the copy; a `/g/` link is keyed by
//!   the derived id of its ledger id and never accepts a contact request (the
//!   persisted state is version 12).
//! * `decode_device_ack_envelope` (identity-recovery 17.5): the devices
//!   whose added-device warning was seen, sent the same way. Domain,
//!   version, a bounded count, fixed-size entries, strictly ascending keys,
//!   no trailing bytes; an accepted envelope folds to exactly its keys.
//! * `decode_blocked_envelope` (ADR-0089 section 6): the people the account
//!   blocked or unblocked, sent the same way. Domain, version, a bounded
//!   count, fixed-size entries with a state byte, strictly ascending keys,
//!   no trailing bytes; an accepted envelope folds to exactly its keys and
//!   states.
//! * `decode_channel_reaction_envelope` (ADR-0087 section 13a): the
//!   account's own reactions under channel posts and, since phase B
//!   (version `0x02`), on their comments, sent the same way. Domain,
//!   version, a bounded count, a target byte (a post target must carry a
//!   zero comment cursor), a one-byte emoji length checked against its
//!   bound before any copy, UTF-8, a state byte, strictly ascending keys, no
//!   trailing bytes; an accepted envelope folds to exactly its registers,
//!   does not apply twice, and a prune past every post removes it all.
//! * `decode_channel_comment_envelope` (ADR-0087 section 13a, a key per
//!   write): the nonces of the account's own comments, sent the same way.
//!   Domain, version, a bounded count, fixed-size entries, a state byte,
//!   strictly ascending keys, no trailing bytes.
//! * `SyncedState::from_bytes` (ADR-0058): the persisted fold, which is also
//!   what the backup payload carries, so a hostile storage tier chooses it.
//!   Version 10 since the quick-reaction register; version 9 added the own
//!   channel-comment register (ADR-0087 section 13a); version 8 added the own channel-reaction register and version 7
//!   the conversation-end register (section 9).
//!
//! The harness asserts none panics, that an accepted envelope is canonical
//! (re-encodes to the identical bytes) and folds to exactly its own
//! registers, that an accepted recovery-email or own-username envelope holds a
//! valid value or the empty one and does not apply twice, that an accepted state
//! persists to bytes that read back as the same state, and that its
//! conversation-end registers read back, travel in the snapshot and yield
//! writes that take effect.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_sync::fuzz_api::decode_synced_and_check(data),
        "lakko-sync synced-state decode property violated",
    );
});
