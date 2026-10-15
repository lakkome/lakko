// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `open_mailbox_row` (ADR-0078 section 1).
//!
//! Every row of a personal mailbox is served by the zero-trust Delivery
//! Service, and anyone holding a reach grant (or, for an open mailbox, an
//! anonymous token) chooses its bytes; the owner's client opens it before
//! anything else looks at it. The input is opened under a fixed mailbox key
//! (a fuzzer cannot forge the AEAD tag, so this exercises the version byte,
//! the bucket check and the AEAD refusal), and its tail is also fed straight
//! to the plaintext parser (kind, length field, zero padding), which a
//! fuzzer cannot reach through the AEAD. Asserted: no panic; anything that
//! opens re-seals to a row that opens to the same kind and body; an accepted
//! body fits the plaintext it came from.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::fuzz_api::{mailbox_plaintext, mailbox_row_open};

fuzz_target!(|data: &[u8]| {
    if let Some(stable) = mailbox_row_open(data) {
        assert!(stable, "an opened row must re-seal to an openable row");
    }
    if let Some(fits) = mailbox_plaintext(data) {
        assert!(fits, "an accepted body must fit its plaintext");
    }
});
