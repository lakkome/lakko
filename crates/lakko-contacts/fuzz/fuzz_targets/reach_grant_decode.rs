// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `ReachGrant::decode` (ADR-0060 §1).
//!
//! A grant arrives inside a profile envelope any group member can craft,
//! and inside an introduction record. Fixed length, so the parser is the
//! length gate and the class byte. Asserted: no panic, round-trip
//! stability, and that deriving the public key of any decoded seed is
//! total.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::reach::{ReachGrant, REACH_GRANT_LEN};

fuzz_target!(|data: &[u8]| {
    let Ok(grant) = ReachGrant::decode(data) else {
        return;
    };
    assert_eq!(data.len(), REACH_GRANT_LEN);
    assert_eq!(
        grant.encode().as_slice(),
        data,
        "the encoding must be canonical"
    );
    let _ = grant.verifying_key();
});
