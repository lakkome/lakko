// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `Signed::<Targets>::from_json` raw-JSON
//! decoder. The `Targets` body is the largest of the four
//! TUF envelopes (carries a `BTreeMap` of target metadata),
//! so it has the widest deserialiser attack surface.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_tuf::Signed::<lakko_tuf::Targets>::from_json(data);
});
