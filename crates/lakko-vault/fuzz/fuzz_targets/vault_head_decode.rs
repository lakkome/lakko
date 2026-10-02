// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `ConversationHead::decode` and `DevicesHead::decode`
//! (ADR-0064 sections 2 and 5).
//!
//! A head is what a vault object opens to: bytes written by some device of
//! the account, an older or a compromised one included. Asserted, not merely
//! exercised:
//!
//! 1. Neither decoder panics.
//! 2. A decoded head is within the size bound and validates.
//! 3. It re-encodes to the identical bytes (the encoding is canonical).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_vault::fuzz_api::head_decode(data);
});
