// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `GroupKind::decode` (the ADR-0062 `0xFF03` GroupContext
//! conversation-kind body). The bytes are attacker-touchable: the kind travels
//! inside every Welcome a group founder can craft, and `join_from_welcome`
//! decodes it before the group is kept. Asserts no panic, and (inside
//! `fuzz_api::decode_group_kind`) that anything that decodes re-encodes to the
//! identical two bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_group_kind(data);
});
