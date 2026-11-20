// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ReentryMarker::decode` (the ADR-0091 `0xFF04` KeyPackage
//! extension body). The bytes are attacker-touchable: every peer reads the
//! marker from every `Add` it gates (receipt, sweep, commit gate), and any
//! member can propose an `Add` carrying a KeyPackage it minted. Asserts no
//! panic, and (inside `fuzz_api::decode_reentry_marker`) that anything that
//! decodes re-encodes to the identical bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_reentry_marker(data);
});
