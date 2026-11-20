// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target: the attested-message receive path (ADR-0098 section 1).
//! The plaintext of an MLS application message is chosen by any group
//! member; the receiver splits the wrapper and verifies its signature before
//! the chat parser runs. Asserts no panic and that nothing verifies under a
//! fixed key the fuzzer does not hold.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_attachments::fuzz_api::attested_open(data);
});
