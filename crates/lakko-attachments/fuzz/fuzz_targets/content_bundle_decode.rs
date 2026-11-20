// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - the ADR-0092 content payload and state snapshot a history
//! bundle or a vault record carries (`Content::from_payload`,
//! `ContentState::decode`). The bytes were written by one of the account's
//! own devices, possibly a compromised one, and reach these decoders on a
//! new device before anything is rendered. Asserts
//! `fuzz_api::content_bundle_round_trip`: nothing panics, and what decodes
//! re-encodes to what decodes the same.

#![no_main]

use lakko_attachments::fuzz_api::content_bundle_round_trip;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(content_bundle_round_trip(data));
});
