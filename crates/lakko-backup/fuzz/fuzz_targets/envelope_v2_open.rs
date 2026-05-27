// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_v2` (full v2 envelope AEAD-open
//! path).
//!
//! Wider surface than `header_of` — covers the schema-
//! version gate, the salt + nonce slicing, the chain-
//! header decode, the ct_len length-prefix arithmetic,
//! and (when the input happens to be valid enough to
//! reach there) the Argon2id KEK derivation + AEAD-open
//! itself. Fixed all-zero PSK-equivalent (the "phrase"
//! bytes) so the AEAD path is reproducible across runs;
//! 99% of fuzzer inputs will fail at the AEAD tag check,
//! the interesting cases are the pre-AEAD parser
//! branches (truncation, length-prefix overflow,
//! schema-byte rejection, trailing junk) where panic /
//! overflow bugs typically hide.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_backup::open_v2;

const FIXED_PHRASE: &[u8] = b"abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon about";

fuzz_target!(|data: &[u8]| {
    let _ = open_v2(FIXED_PHRASE, data);
});
