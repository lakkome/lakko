// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the owned-channels list decoder (ADR-0101 section 7, via
//! `fuzz_api::owned_channels_decode`). The input is padded or cut to the
//! list's fixed length so the fuzzer explores the content rather than the
//! length gate (which a separate short input still reaches). Version 2
//! (65 532 bytes, entries with a handle seed). Whatever decodes must
//! re-encode to the identical bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_channels::fuzz_api::owned_channels_decode(data);
    let mut wire = data.to_vec();
    wire.resize(lakko_channels::OWNED_CHANNELS_LIST_LEN, 0);
    if let Some(same) = lakko_channels::fuzz_api::owned_channels_decode(&wire) {
        assert!(same, "a decoded list did not re-encode to its own bytes");
    }
});
