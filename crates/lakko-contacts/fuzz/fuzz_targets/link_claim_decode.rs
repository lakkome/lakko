// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_link_claim` (ADR-0079, reworked 2026-10-17).
//!
//! A link-request claim arrives as a DeviceGroup `ServiceEnvelope`, from
//! another device of the account or from the zero-trust DS replaying a
//! corrupt row, and reaches this bounded, fail-closed decoder before any
//! state is consulted: a domain, a version byte checked before the length,
//! and a fixed-length payload.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder or the dispatcher;
//! * a decoded claim is exactly the advertised length;
//! * round-trip stability: whatever decodes re-composes to exactly the input
//!   (the encoding is canonical), and the dispatcher classifies the same
//!   bytes as the same claim;
//! * the pure outcome rule names at most one winner among the devices, the
//!   earliest roster claim, and never a device outside the roster.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::link_claim::{
    link_claim_outcome, link_claim_winner, LinkClaimOutcome, ObservedLinkClaim,
    LINK_CLAIM_ENVELOPE_LEN,
};

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::link_claim_decode(data) {
        assert_eq!(data.len(), LINK_CLAIM_ENVELOPE_LEN);
        assert!(ok, "decode, compose and dispatch must agree on a claim");
    }
    // The same bytes read as claims: 9 bytes each, a message id and one of
    // four device keys; device 3 is not in the roster (removed).
    let devices: [[u8; 32]; 4] = [[1; 32], [2; 32], [3; 32], [4; 32]];
    let roster = &devices[..3];
    let claims: Vec<ObservedLinkClaim> = data
        .chunks_exact(9)
        .map(|c| ObservedLinkClaim {
            message_id: u64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]),
            author_key: devices[usize::from(c[8] % 4)],
        })
        .collect();
    let winner = link_claim_winner(&claims, roster);
    let mut winners = 0;
    for key in &devices {
        match link_claim_outcome(key, &claims, roster) {
            LinkClaimOutcome::Won { message_id } => {
                winners += 1;
                assert!(roster.contains(key));
                assert_eq!(winner.map(|w| w.message_id), Some(message_id));
            }
            LinkClaimOutcome::Lost { winner_message_id } => {
                assert_eq!(winner.map(|w| w.message_id), Some(winner_message_id));
            }
            LinkClaimOutcome::NoWinner => assert!(winner.is_none()),
        }
    }
    assert!(winners <= 1);
    if let Some(w) = winner {
        assert!(roster.contains(&w.author_key));
        assert!(claims
            .iter()
            .filter(|c| roster.contains(&c.author_key))
            .all(|c| c.message_id >= w.message_id));
    }
});
