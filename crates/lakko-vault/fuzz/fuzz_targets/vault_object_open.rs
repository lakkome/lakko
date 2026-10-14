// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `open_object` under a fixed key, and the padding frame
//! (ADR-0064 section 1).
//!
//! The sealed bytes are what the storage service returns, so every byte is
//! the server's to choose. A fuzzer cannot forge the authentication tag, so
//! the sealed path exercises the size cap, the format byte, the bucket shape
//! and the refusal, and the padding parser that runs after authentication is
//! driven directly. Asserted, not merely exercised:
//!
//! 1. Nothing panics.
//! 2. An object that opens re-seals to the identical bytes.
//! 3. A frame that unpads re-pads to the identical bytes (one plaintext has
//!    one padded form).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_vault::fuzz_api::object_open(data);
});
