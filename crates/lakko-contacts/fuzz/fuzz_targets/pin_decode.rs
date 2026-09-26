// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `pin::decode_pin` (ADR-0052 decision 3): the shared-pin envelope rides an
//! MLS application message on the conversation's journal, so these bytes
//! reach the parser from the zero-trust DS before any state or role is
//! consulted. Asserts no panic, the fixed length, round-trip stability and
//! that a stamp is never newer than itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::pin::{compose_pin, decode_pin, pin_is_newer, PIN_ENVELOPE_LEN};

fuzz_target!(|data: &[u8]| {
    let Ok(op) = decode_pin(data) else {
        return;
    };
    assert_eq!(data.len(), PIN_ENVELOPE_LEN);
    let recomposed = compose_pin(&op);
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    assert_eq!(decode_pin(&recomposed).expect("a composed pin must decode"), op);
    assert!(!pin_is_newer(op.updated_at_ms, op.updated_at_ms));
    let _ = pin_is_newer(op.updated_at_ms, u64::MAX);
});
