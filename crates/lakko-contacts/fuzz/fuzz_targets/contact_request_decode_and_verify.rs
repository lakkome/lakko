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

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::decode_and_verify(data);
});
