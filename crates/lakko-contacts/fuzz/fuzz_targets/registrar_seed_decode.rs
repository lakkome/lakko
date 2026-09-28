// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `registrar_seed::decode_registrar_seed` (ADR-0057): a journal root's
//! registrar-seed envelope rides an MLS application message on the group's
//! journal, so these bytes reach the parser from the zero-trust DS (or any
//! member) before any state or role is consulted. Asserts no panic, the
//! fixed length, that the dispatcher classifies the same bytes the same way,
//! and that opening a hostile seal under a fixed key never panics.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
use lakko_contacts::registrar_seed::{decode_registrar_seed, REGISTRAR_SEED_ENVELOPE_LEN};
use lakko_crypto::agreement::SecretKey;

fuzz_target!(|data: &[u8]| {
    let Ok(envelope) = decode_registrar_seed(data) else {
        return;
    };
    assert_eq!(data.len(), REGISTRAR_SEED_ENVELOPE_LEN);
    match dispatch_incoming_envelope(data) {
        Ok(IncomingEnvelope::RegistrarSeed(dispatched)) => assert_eq!(dispatched, envelope),
        other => panic!("dispatcher disagrees with the decoder: {other:?}"),
    }
    let _ = envelope.open(&SecretKey::from_bytes([0x5A; 32]));
});
