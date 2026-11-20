// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target `moderation_decode` (ADR-0089). See the `[[bin]]` comment in
//! `../Cargo.toml` for what it drives and asserts.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_moderation::fuzz_api::decode_all(data);
});
