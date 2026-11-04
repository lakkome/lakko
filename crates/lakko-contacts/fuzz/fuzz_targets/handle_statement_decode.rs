// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `HandleStatement::decode` + `verify` (ADR-0087 section 3).
//!
//! Every write to a group's or channel's `@name` in discovery is one signed
//! statement with no session, so anyone can send discovery these bytes and
//! they reach the bounded decoder before any row is consulted: the domain, a
//! known kind, the one length of that kind, the visibility byte and (for a
//! rekey) a valid new key; then the window and the signature.
//!
//! Asserted: no input panics the decoder or the verifier, and whatever
//! decodes re-encodes to exactly the input (one statement, one encoding).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::handle_statement_decode(data) {
        assert!(ok, "a decoded handle statement must re-encode to its input");
    }
});
