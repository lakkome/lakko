// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `pairing::open_rejoin_knock` (G-55, ADR-0081 amendment): a
//! sealed DeviceGroup rejoin knock as a sibling reads it from the knock queue,
//! which accepts rows from anyone holding a `ds-knock` token. Covers the magic
//! and version checks, the AEAD refusal under a fixed lineage seed, the
//! padding parser fed directly, and a seal-open round trip of the input as a
//! KeyPackage (`fuzz_api::open_rejoin_knock`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::open_rejoin_knock(data);
});
