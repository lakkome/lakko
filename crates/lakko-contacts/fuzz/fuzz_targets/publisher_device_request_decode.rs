// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_publisher_device_request` (ADR-0090 section 2.2).
//!
//! A co-publisher's device request rides a PublisherGroup journal row any
//! publisher (or the zero-trust DS) can craft, and reaches the decoder before
//! any state is consulted: the domain, the version byte (checked before any
//! length), the action byte, a two-byte KeyPackage length bounded before the
//! copy, the mailbox binding layout, a delegation count bounded before the
//! copy, and the exact remaining length.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! within the advertised bounds and dispatches as the same request.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::publisher_device_request_decode(data) {
        assert!(
            ok,
            "a decoded device request must re-compose and dispatch the same"
        );
    }
});
