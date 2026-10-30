// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - A member's walk up its path (ADR-0086). The properties asserted are in
//! `lakko_circle::fuzz_api::path_walk`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_circle::fuzz_api::path_walk(data);
});
