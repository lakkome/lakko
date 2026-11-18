// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `HistoryChunk::decode` (ADR-0064 section 2).
//!
//! A chunk carries up to 512 saved messages, each with nested attachment,
//! reply, forward and reaction lists. Asserted, not merely exercised:
//!
//! 1. The decoder never panics.
//! 2. A decoded chunk is within the size bound and validates.
//! 3. It re-encodes to the identical bytes (the encoding is canonical).
//! 4. Merging its messages with themselves is sorted and holds each id once.
//! 5. Record version 2 (ADR-0090): every content block (kind, payload,
//!    state) and every audio title and artist is within its bound.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_vault::fuzz_api::chunk_decode(data);
});
