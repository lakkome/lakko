// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_group_name` (ADR-0051 §1).
//!
//! A group-name envelope arrives as an MLS application message on a group
//! journal — from any member (the role gate that decides whether the
//! sender may set the name runs AFTER this decode, on the receiver), or
//! from the zero-trust DS replaying a corrupt row — so the bytes reach
//! this bounded, fail-closed decoder before any state is consulted. Like
//! its profile sibling the payload is variable-length: a one-byte length
//! prefix, UTF-8 validation and the Unicode format-character table, so
//! there is real parser surface here.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * **round-trip stability** — anything that decodes re-composes (the
//!   composer must accept whatever the decoder accepted, since both run
//!   one validator) to bytes that decode to the identical name, and those
//!   bytes are exactly the input: the encoding is canonical, so an
//!   accepted envelope has one spelling;
//! * a decoded envelope is within the advertised length bounds;
//! * the order is total and panic-free, and a name is never newer than
//!   itself — the property that makes a growth re-send of the stored name
//!   a no-op rather than an update.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::group_name::{
    compose_group_name, decode_group_name, group_name_is_newer, GROUP_NAME_ENVELOPE_MAX_LEN,
    GROUP_NAME_ENVELOPE_MIN_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(group_name) = decode_group_name(data) else {
        return;
    };

    assert!((GROUP_NAME_ENVELOPE_MIN_LEN..=GROUP_NAME_ENVELOPE_MAX_LEN).contains(&data.len()));

    let recomposed =
        compose_group_name(&group_name).expect("compose must accept what decode accepted");
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    let again = decode_group_name(&recomposed).expect("a composed group name must decode");
    assert_eq!(again, group_name);

    assert!(!group_name_is_newer(
        group_name.updated_at_ms,
        group_name.updated_at_ms
    ));
    let _ = group_name_is_newer(group_name.updated_at_ms, u64::MAX);
});
