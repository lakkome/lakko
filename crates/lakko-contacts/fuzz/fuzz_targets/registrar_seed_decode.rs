// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `registrar_seed::decode_registrar_seed` (ADR-0057): a journal root's
//! registrar-seed envelope rides an MLS application message on the group's
//! journal, so these bytes reach the parser from the zero-trust DS (or any
//! member) before any state or role is consulted. Asserts no panic, the
//! fixed length, that the dispatcher classifies the same bytes the same way,
//! that a decoded envelope (its clear registrar public key included)
//! re-encodes to the identical bytes, and that opening a hostile seal under
//! a fixed key never panics and never accepts a seed that does not derive
//! the header's registrar public key.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
use lakko_contacts::registrar_seed::{
    decode_registrar_seed, registrar_public_key, REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
    REGISTRAR_SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::SecretKey;

/// Offset of the clear registrar public key: separator, version, journal
/// id, account key, rank.
const PUBLIC_KEY_AT: usize = 1 + 32 + 32 + 1;

fuzz_target!(|data: &[u8]| {
    let Ok(envelope) = decode_registrar_seed(data) else {
        return;
    };
    assert_eq!(data.len(), REGISTRAR_SEED_ENVELOPE_LEN);
    match dispatch_incoming_envelope(data) {
        Ok(IncomingEnvelope::RegistrarSeed(dispatched)) => assert_eq!(dispatched, envelope),
        other => panic!("dispatcher disagrees with the decoder: {other:?}"),
    }
    // The clear public key round-trips: it is the bytes at its offset, and
    // the whole envelope re-encodes to the identical wire.
    let at = REGISTRAR_SEED_ENVELOPE_DOMAIN_V1.len() + PUBLIC_KEY_AT;
    assert_eq!(&data[at..at + 32], &envelope.registrar_public_key);
    assert_eq!(envelope.to_bytes(), data);
    // An open that succeeds must have checked the derived key.
    if let Ok(seed) = envelope.open(&SecretKey::from_bytes([0x5A; 32])) {
        assert_eq!(registrar_public_key(&seed), envelope.registrar_public_key);
    }
});
