// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `dispatch_incoming_envelope` (Phase 3c3-fuzz-envelopes).
//!
//! Composite-dispatcher fuzz: feeds arbitrary bytes through the
//! owner-side polling-loop entry point that runs the two unwrap
//! helpers in dispatch order and falls through to the intro-message
//! branch. Critically, any arbitrary `data` is a valid input —
//! intro-message fallthrough means the dispatcher MUST return
//! `Ok(IntroMessage(_))` for "anything that doesn't look like a
//! call envelope", regardless of the bytes' shape. A panic here
//! would indicate a regression in the prefix-matching dispatcher
//! itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::dispatch_incoming_envelope(data);
});
