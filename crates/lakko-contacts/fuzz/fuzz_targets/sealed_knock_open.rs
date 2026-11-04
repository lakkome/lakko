// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_knock` (ADR-0087 section 5).
//!
//! A knock row is written to a group's DS knock queue by any stranger with a
//! token, so an admin opens bytes it did not choose: the version byte, the
//! two row sizes, the AEAD refusal under a fixed seed, and the plaintext
//! parser (key package, reply grant, profile, capability, zero padding),
//! which a fuzzer cannot reach through the AEAD and so is also fed directly.
//!
//! Asserted: no panic, and whatever opens or parses re-seals and re-opens to
//! the same request.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::sealed_knock_open(data) {
        assert!(ok, "an opened knock must re-seal and re-open to the same request");
    }
    if let Some(ok) = lakko_contacts::fuzz_api::knock_plaintext(data) {
        assert!(ok, "a parsed knock plaintext must re-seal and re-open to the same request");
    }
});
