// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_media_state` (camera-off-cleanup).
//!
//! The camera on/off media-state signal rides the opaque SFU
//! `signal-relay` payload, so the bytes reach this bounded,
//! fail-closed decoder after arriving from an untrusted relay
//! peer (the SFU forwards the payload without parsing it).
//! Any arbitrary `data` is a valid input — a plaintext that is
//! not a media-state envelope must surface as a graceful `Err`
//! (wrong domain / bad length / unknown version / bad flag),
//! never a panic. `decode_media_state` wraps the same parse
//! path the composite `dispatch_incoming_envelope` uses, so
//! this target pins the standalone strict decoder surface.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::media_state::decode_media_state(data);
});
