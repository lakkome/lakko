// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `link_seal::open` + the kind's `decode_and_verify` (the
//! 2026-09-28 security pass of ADR-0028 and ADR-0009).
//!
//! Every invite link (`/i/`, `/c/`, `/g/`) now carries its capability SEALED
//! under a secret in the URL fragment. The sealed blob is what the discovery
//! ledger serves under an id, or what an inline link carries in its path, so
//! a malicious ledger or link-minter chooses every byte of it. The joiner
//! opens it (kind/version gate, length split, HKDF, AEAD) and verifies the
//! capability inside before any other step.
//!
//! The first input byte picks the kind; the rest is the sealed blob, opened
//! under a FIXED secret. The fuzzer cannot forge an AEAD tag, so the accepted
//! arm is reached only from a corpus entry a test sealed under that secret;
//! whatever is accepted must re-seal and re-open to the same plaintext
//! (`Some(false)` is a bug and aborts).

#![no_main]

use libfuzzer_sys::fuzz_target;

const SECRET: [u8; lakko_contacts::link_seal::LINK_SECRET_LEN] =
    [0x42; lakko_contacts::link_seal::LINK_SECRET_LEN];

fuzz_target!(|data: &[u8]| {
    let Some((&kind, sealed)) = data.split_first() else {
        return;
    };
    if lakko_contacts::fuzz_api::open_sealed_link(kind, &SECRET, sealed) == Some(false) {
        panic!("an accepted sealed link did not round-trip");
    }
});
