// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the ADR-0061 message syntax: parse, format in every
//! supported language, check against itself; and load the same bytes as a
//! source catalog. Must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_l10n::fuzz_api::parse_and_format_message(data);
    lakko_l10n::fuzz_api::load_arbitrary_source(data);
});
