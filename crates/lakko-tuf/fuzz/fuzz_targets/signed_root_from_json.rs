// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `Signed::<Root>::from_json` raw-JSON decoder.
//!
//! Shoves an attacker-controlled byte slice through the
//! outermost TUF parser. Goal: prove the parser never
//! panics / aborts / overflows on arbitrary bytes. The
//! signature + chain checks run AFTER `from_json`, so this
//! surface is fully attacker-controlled — the harness must
//! be safe against any input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_tuf::Signed::<lakko_tuf::Root>::from_json(data);
});
