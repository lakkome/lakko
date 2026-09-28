// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - the reach-key version state envelope (ADR-0058 §7).
//!
//! Same carrier and threat as the setting envelope. The count is two bytes
//! and each entry is length-prefixed, so the entry cap and the per-class
//! scope rule are reachable from raw bytes. Asserted: no panic, whatever
//! decodes is valid, and round-trip stability.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::visibility::{
    compose_reach_state, decode_reach_state, reach_state_is_newer, MAX_REACH_STATE_ENTRIES,
};

fuzz_target!(|data: &[u8]| {
    let Ok(state) = decode_reach_state(data) else {
        return;
    };
    assert!(state.entries.len() <= MAX_REACH_STATE_ENTRIES);
    let recomposed = compose_reach_state(&state).expect("compose must accept what decode accepted");
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    assert_eq!(decode_reach_state(&recomposed).expect("round trip"), state);
    assert!(!reach_state_is_newer(
        state.updated_at_ms,
        state.updated_at_ms
    ));
});
