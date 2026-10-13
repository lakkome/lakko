// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the email-vault open path and the recovery-address rule
//! (ADR-0075, via `fuzz_api::open_email_vault`). The sealed blob comes back
//! from the server in the answer to a mailed code, so whoever controls
//! that answer chooses every byte. It is opened under a fixed stretched key
//! (Argon2id is skipped: its inputs are the person's, not the blob's),
//! which drives the 44-byte length gate, the HKDF step, the AEAD refusal
//! and the entropy rebuild. The fuzzer cannot forge a tag, so nothing may
//! open. The same bytes, when UTF-8, go through `normalize_recovery_email`;
//! a normalised address must normalise to itself, because the client and
//! the server each normalise once and must agree.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (opened, address_stable) = lakko_mnemonic::fuzz_api::open_email_vault(data);
    assert!(!opened, "a blob opened under the fixed key");
    assert!(address_stable, "normalising twice changed the address");
});
