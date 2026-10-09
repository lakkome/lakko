// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_key_rotation` and the dispatcher's view of the same
//! bytes (ADR-0066 section 3.3).
//!
//! A key-rotation envelope arrives as an MLS application message in the
//! account's DeviceGroup. A device of the account that is compromised, or the
//! zero-trust DS replaying a corrupt row, chooses the bytes, and they reach
//! this bounded, fail-closed decoder before the receiver checks the proof or
//! derives anything from the entropy. Fixed length; the version byte is
//! judged before the length.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder or the dispatcher;
//! * a decoded envelope is exactly the advertised length;
//! * **round-trip stability** — whatever decodes re-composes to exactly the
//!   input (the encoding is canonical) and decodes to the same value;
//! * the dispatcher classifies the same bytes the same way: what decodes is
//!   dispatched as a key rotation, never as an intro message, so it can
//!   never be rendered;
//! * the `Debug` form never contains the entropy bytes as text;
//! * the pending-age rule is total.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
use lakko_contacts::key_rotation::{
    compose_key_rotation, decode_key_rotation, KEY_ROTATION_ENVELOPE_LEN,
};

fuzz_target!(|data: &[u8]| {
    let dispatched = dispatch_incoming_envelope(data);
    let Ok(rotation) = decode_key_rotation(data) else {
        assert!(
            !matches!(dispatched, Ok(IncomingEnvelope::KeyRotation(_))),
            "the dispatcher accepted what the decoder refused"
        );
        return;
    };

    assert_eq!(data.len(), KEY_ROTATION_ENVELOPE_LEN);
    let recomposed = compose_key_rotation(&rotation);
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    assert_eq!(
        decode_key_rotation(&recomposed).expect("a composed envelope must decode"),
        rotation
    );
    match dispatched {
        Ok(IncomingEnvelope::KeyRotation(got)) => assert_eq!(got, rotation),
        other => panic!("a key rotation was dispatched as {other:?}"),
    }

    assert!(format!("{rotation:?}").contains("redacted"));
    let _ = rotation.pending_expired(0);
    let _ = rotation.pending_expired(u64::MAX);
});
