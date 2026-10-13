// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_group_closed` (ADR-0073 phase 2,
//! `docs/ux/identity-recovery.md` §16.5).
//!
//! A group-closed envelope arrives as an MLS application message on a group
//! journal — from any member (the role gate that decides whether the sender
//! may close the group runs AFTER this decode, on the receiver), or from the
//! zero-trust DS replaying a corrupt row — so the bytes reach this bounded,
//! fail-closed decoder before any state is consulted. The payload has a
//! one-byte length prefix that admits exactly two values, a version gate
//! judged before the remaining length, and a trailing-bytes refusal.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * **round-trip stability** — anything that decodes re-composes to bytes
//!   that decode to the identical notice, and those bytes are exactly the
//!   input: the encoding is canonical, so an accepted envelope has one
//!   spelling;
//! * a decoded envelope is exactly one of the two advertised lengths;
//! * the order is total and panic-free, and a notice is never newer than
//!   itself — the property that makes a growth re-send of the stored notice
//!   a no-op rather than an update.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::group_closed::{
    compose_group_closed, decode_group_closed, group_closed_is_newer,
    GROUP_CLOSED_ENVELOPE_MAX_LEN, GROUP_CLOSED_ENVELOPE_MIN_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(closed) = decode_group_closed(data) else {
        return;
    };

    assert!(
        data.len() == GROUP_CLOSED_ENVELOPE_MIN_LEN || data.len() == GROUP_CLOSED_ENVELOPE_MAX_LEN
    );
    assert_eq!(
        closed.successor_group_id.is_some(),
        data.len() == GROUP_CLOSED_ENVELOPE_MAX_LEN
    );

    let recomposed = compose_group_closed(&closed);
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    let again = decode_group_closed(&recomposed).expect("a composed notice must decode");
    assert_eq!(again, closed);

    assert!(!group_closed_is_newer(closed.closed_at_ms, closed.closed_at_ms));
    let _ = group_closed_is_newer(closed.closed_at_ms, u64::MAX);
    let _ = group_closed_is_newer(u64::MAX, closed.closed_at_ms);
});
