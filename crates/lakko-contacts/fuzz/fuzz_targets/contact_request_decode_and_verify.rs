// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ContactRequest::decode_and_verify` (Phase 3c3-fuzz-envelopes).
//!
//! The outermost untrusted-input boundary of the stranger-inbox
//! path: anyone can deliver arbitrary bytes to anyone's inbox
//! pseudonym (PoW + Privacy Pass gate the rate but not the
//! payload shape). The decoder runs **before** any HPKE-open,
//! before signature verification — a malicious sender controls
//! every byte. Any panic here is a DoS vector that disables the
//! recipient's stranger-inbox polling loop until restart.
//!
//! The same bytes also drive the SEALED-plaintext splitter (profile, reply
//! grant, intro; ADR-0048 §2, ADR-0060 §6) through
//! `fuzz_api::contact_request_plaintext`, which the AEAD otherwise hides from
//! a fuzzer: anything it accepts must re-frame to the same bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::decode_and_verify(data);
    assert_ne!(
        lakko_contacts::fuzz_api::contact_request_plaintext(data),
        Some(false),
        "an accepted sealed plaintext must re-frame to the same bytes"
    );
});
