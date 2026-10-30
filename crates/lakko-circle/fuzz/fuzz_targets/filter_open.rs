// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - A sealed filter and its plaintext (ADR-0086). The properties asserted are in
//! `lakko_circle::fuzz_api::filter_open`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_circle::fuzz_api::filter_open(data);
});
