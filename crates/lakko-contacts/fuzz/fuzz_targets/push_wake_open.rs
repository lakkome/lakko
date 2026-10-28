// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `open_push_wake` and the subscription token-list parser
//! (ADR-0078 section 3).
//!
//! A sealed wake arrives in the `x-lakko-wake` header of a DS publish, which
//! any writer of a journal chooses byte for byte; the DS forwards it unread
//! and push-gw opens it before anything else looks at it. The input is
//! opened under a fixed push-gw key (a fuzzer cannot forge the AEAD tag, so
//! this exercises the length, version and AEAD refusal), and is also fed
//! straight to the wake plaintext parser (kind, wake key, signature) and the
//! subscription token-list parser, which a fuzzer cannot reach through the
//! AEAD. Asserted: no panic; an opened wake has a known kind and names the
//! push_gid it carried; a parsed token list re-seals to a row that opens to
//! the same entries.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::fuzz_api::{push_subscription_plaintext, push_wake_open, push_wake_plaintext};

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = push_wake_open(data) {
        assert!(ok, "an opened wake must carry a known kind");
    }
    if let Some(ok) = push_wake_plaintext(data) {
        assert!(ok, "an accepted wake names the push_gid it carried");
    }
    if let Some(stable) = push_subscription_plaintext(data) {
        assert!(stable, "a parsed token list must re-seal to an openable row");
    }
});
