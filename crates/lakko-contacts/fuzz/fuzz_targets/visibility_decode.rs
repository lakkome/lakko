// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - the "who can find me" setting envelope (ADR-0060 §7).
//!
//! It travels over the owner's DeviceGroup, so a compromised own device or
//! a corrupt journal row can craft it. Asserted: no panic, whatever decodes
//! passes the canonical-form rule, and round-trip stability.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::visibility::{compose_visibility, decode_visibility, visibility_is_newer};

fuzz_target!(|data: &[u8]| {
    let Ok(setting) = decode_visibility(data) else {
        return;
    };
    setting.validate().expect("a decoded setting is valid");
    let recomposed =
        compose_visibility(&setting).expect("compose must accept what decode accepted");
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    assert_eq!(decode_visibility(&recomposed).expect("round trip"), setting);
    assert!(!visibility_is_newer(
        setting.updated_at_ms,
        setting.updated_at_ms
    ));
});
