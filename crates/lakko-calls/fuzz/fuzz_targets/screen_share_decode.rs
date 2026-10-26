// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the screen-share state (ADR-0083).
//!
//! A share state reaches a receiver inside a sealed SFU relay payload any
//! participant can fill, or inside a one-to-one call signal any member can
//! send, and is decoded before its signature is checked. Every byte is
//! attacker-chosen.
//!
//! The harness asserts, on arbitrary input:
//!   1. `decode_screen_share` never panics;
//!   2. a state that decodes has a non-zero seq, a peer id within bounds,
//!      and exactly the length its layout gives;
//!   3. folding it into a `ScreenShareState` never panics, and the fold's
//!      next start seq is above every seq it has seen.

#![no_main]

use lakko_calls::relay::MAX_PEER_ID_LEN;
use lakko_calls::screen_share::{
    decode_screen_share, screen_share_len, ScreenShareState, VerifiedShare,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(decoded) = decode_screen_share(data) else {
        return;
    };
    assert_ne!(decoded.seq, 0);
    assert!(!decoded.peer_id.is_empty() && decoded.peer_id.len() <= MAX_PEER_ID_LEN);
    assert_eq!(data.len(), screen_share_len(decoded.peer_id.len()));

    let mut fold = ScreenShareState::new([0; 32]);
    fold.observe(&VerifiedShare {
        id: decoded.id(),
        peer_id: decoded.peer_id.clone(),
        status: decoded.status,
        with_sound: decoded.with_sound,
        account_key: decoded.signer_key.to_vec(),
    });
    assert!(fold.next_start_seq() > decoded.seq || decoded.seq == u64::MAX);
    let _ = fold.current();
    let _ = fold.own_superseded();
});
