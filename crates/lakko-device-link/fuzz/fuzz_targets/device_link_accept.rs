// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - the old device's accept path of the ADR-0049 section 2
//! device link, under a fixed transcript. `pake_a`, `confirm_a` and the
//! sealed payload are relay-controlled; accept must never panic and must
//! never accept (the fuzzer cannot know the code or the SPAKE2 key).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(!lakko_device_link::fuzz_api::accept_arbitrary(data));
});
