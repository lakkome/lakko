// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `EpochId::from_be_bytes`.
//!
//! Tiny but kept around for completeness so the wire-side
//! 4-byte big-endian parser surface is covered. The
//! function is itself bounds-checked (rejects any slice
//! whose length is not exactly 4) but it is the entry
//! point downstream consumers use to round-trip an
//! epoch_id off the network, so a corpus that exercises
//! all 2^32 + non-4-length inputs is the right shape.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_channels::epoch::EpochId::from_be_bytes(data);
});
