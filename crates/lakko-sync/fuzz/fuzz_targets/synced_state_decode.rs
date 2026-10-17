// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the byte decoders of the synced state (`lakko-sync`).
//!
//! Five decoders run on bytes this device did not write:
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
//! * `decode_link_record_envelope` (ADR-0079 section 4): the account's own
//!   invite links, sent the same way. Domain, version, a bounded count,
//!   every length checked against its bound before any copy, strictly
//!   ascending link ids, no trailing bytes; a revoked record it carries must
//!   be held revoked after the fold.
//! * `SyncedState::from_bytes` (ADR-0058): the persisted fold, which is also
//!   what the backup payload carries, so a hostile storage tier chooses it.
//!
//! The harness asserts none panics, that an accepted envelope is canonical
//! (re-encodes to the identical bytes) and folds to exactly its own
//! registers, that an accepted recovery-email or own-username envelope holds a
//! valid value or the empty one and does not apply twice, and that an accepted state
//! persists to bytes that read back as the same state.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_sync::fuzz_api::decode_synced_and_check(data),
        "lakko-sync synced-state decode property violated",
    );
});
