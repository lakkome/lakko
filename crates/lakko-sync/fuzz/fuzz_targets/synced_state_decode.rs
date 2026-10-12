// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the byte decoders of the synced state (`lakko-sync`).
//!
//! Two decoders run on bytes this device did not write:
//!
//! * `decode_lineage_envelope` (ADR-0065 R.5): a conversation-lineage
//!   envelope another device of the same account sent over the DeviceGroup's
//!   opaque service-envelope carrier. Domain, version (judged before the
//!   count), a bounded count, one-byte length prefixes checked against their
//!   bound before any copy, strictly ascending successors, no trailing bytes.
//! * `SyncedState::from_bytes` (ADR-0058): the persisted fold, which is also
//!   what the backup payload carries, so a hostile storage tier chooses it.
//!
//! The harness asserts neither panics, that an accepted envelope is
//! canonical (re-encodes to the identical bytes) and folds to exactly its
//! own registers, and that an accepted state persists to bytes that read
//! back as the same state.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_sync::fuzz_api::decode_synced_and_check(data),
        "lakko-sync synced-state decode property violated",
    );
});
