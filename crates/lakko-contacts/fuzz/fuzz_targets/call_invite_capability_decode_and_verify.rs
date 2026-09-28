// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `call_invite::decode_and_verify`
//! (Phase 3c3-fuzz-envelopes).
//!
//! The plaintext capability parser. Since the 2026-09-28
//! security pass a `/c/` link carries the capability SEALED
//! under a secret in its fragment, so the joiner reaches this
//! parser on the bytes a sealed link opens to (`open_link`; the
//! open itself is fuzzed by `invite_link_open`). The discovery
//! service no longer parses capabilities at all. The bytes are
//! still attacker-controlled — whoever mints a link chooses what
//! it opens to — so any panic is a DoS vector on the joiner's
//! landing page.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::call_invite::decode_and_verify(data);
});
