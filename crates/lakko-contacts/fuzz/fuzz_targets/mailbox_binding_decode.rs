// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `MailboxBinding::decode` and `verify` (ADR-0078 section 1).
//!
//! A mailbox binding travels with every mailbox key - inside a reach grant,
//! a profile, an invite capability, an introduction or public record, a
//! committer delegation - so a stranger, a group member or a hostile
//! discovery tier chooses its bytes. Fixed length: the length gate and the
//! version byte, then the signature check against a fixed account key.
//! Asserted: no panic, and round-trip stability of whatever decodes.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::fuzz_api::mailbox_binding_decode;

fuzz_target!(|data: &[u8]| {
    if let Some(stable) = mailbox_binding_decode(data) {
        assert!(stable, "the encoding must be canonical");
    }
});
