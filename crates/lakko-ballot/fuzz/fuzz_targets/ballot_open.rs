// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - see `lakko_ballot::fuzz_api::ballot_open` for the properties asserted.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_ballot::fuzz_api::ballot_open(data);
});
