// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_media_state` (camera-off-cleanup; the microphone
//! since 2026-11-07).
//!
//! The camera and microphone media-state signal rides the SFU
//! `signal-relay` payload (sealed under the call's relay key, so a peer of
//! the call chooses these bytes), so the bytes reach this bounded,
//! fail-closed decoder from an untrusted relay peer. Any arbitrary `data`
//! is a valid input — a plaintext that is not a media-state envelope must
//! surface as a graceful `Err` (wrong domain / bad length / unknown version
//! / unknown flag bit), never a panic. The target also asserts what the
//! decoder may accept: only version `0x02`, and whatever decodes
//! re-composes to the identical bytes.

#![no_main]

use lakko_contacts::media_state::{
    compose_media_state, decode_media_state, MEDIA_STATE_ENVELOPE_DOMAIN_V1, MEDIA_STATE_VERSION_V2,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(state) = decode_media_state(data) else {
        return;
    };
    assert_eq!(
        data[MEDIA_STATE_ENVELOPE_DOMAIN_V1.len()],
        MEDIA_STATE_VERSION_V2,
        "only version 0x02 is accepted"
    );
    assert_eq!(
        compose_media_state(state.camera_on, state.microphone_on),
        data,
        "a decoded media state re-composes to its bytes"
    );
});
