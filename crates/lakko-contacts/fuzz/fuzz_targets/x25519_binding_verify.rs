// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `verify_x25519_binding` (security review 2026-11-21).
//!
//! The binding of an account's X25519 key to its identity key is served by
//! the directory, which a hostile operator controls, and every client
//! verifies it before sealing to the X25519 key: the length check, the key
//! parse and the signature check see attacker-chosen bytes.
//!
//! Asserted: no panic; nothing verifies under a fixed identity key the
//! fuzzer does not hold; a binding signed by that key verifies.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_contacts::fuzz_api::x25519_binding_verify(data),
        "an x25519 binding verified without its key, or a signed one did not"
    );
});
