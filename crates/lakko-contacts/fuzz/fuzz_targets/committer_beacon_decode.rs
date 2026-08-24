// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_committer_beacon` (ADR-0045 decision 2,
//! variant 1) + the `acting_committer_verdict` fold.
//!
//! The committer liveness beacon rides an MLS application message on
//! the group journal, so the bytes reach this bounded, fail-closed
//! decoder after arriving from the ZERO-TRUST, opaque Delivery
//! Service — a malicious member or a corrupt journal entry could have
//! crafted them, and the receive-loop dispatch reaches this parser
//! **before any state is consulted** (the same threat class as
//! `media_state_decode` / `push_routing_secret_decode`). Any arbitrary
//! `data` is a valid input: a plaintext that is not a beacon must
//! surface as a graceful `Err` (wrong domain / bad length / unknown
//! version), never a panic.
//!
//! ## What is small here, and what is not
//!
//! The parser surface is deliberately tiny — the payload is a single
//! version byte, because the beacon carries no fields at all. What is
//! **not** small is the consequence of getting the two gates wrong:
//! the exact-length gate is what stops a v2 field being read by a v1
//! decoder, and the domain gate is what stops a beacon being rendered
//! as a chat bubble. Both are reachable from raw bytes here, including
//! the degenerate bare-separator case (domain matched, zero-byte
//! payload) that must be an `Err` rather than an `Ok(None)`
//! fall-through.
//!
//! ## Why the fold is driven from the same target
//!
//! A decoded beacon is never used for anything except feeding the
//! acting-committer decision, so the target carries it through: the
//! fuzzer's own bytes are re-used as an adversarial observation set
//! (ranks, ages and a freshness window all attacker-chosen), and the
//! fold is asserted **total** — it must never panic and never overflow
//! on any of them. Two invariants that hold for every input are
//! checked rather than merely exercised:
//!
//! * a `None` own-rank is always `NotACandidate`, whatever the field
//!   looks like — a plain member can never be talked into sweeping;
//! * a `Defer` never names a rank at or above this device's own,
//!   which is the strict-inequality rule that keeps a device's own
//!   echoed-back beacon from deadlocking it.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::committer_beacon::{
    acting_committer_verdict, decode_committer_beacon, ActingCommitter, BeaconObservation,
};

fuzz_target!(|data: &[u8]| {
    // 1. The bounded, fail-closed decode: domain prefix, exact length,
    //    version gate. Must never panic on arbitrary bytes.
    let _ = decode_committer_beacon(data);

    // 2. The fold, on an observation set built from the same
    //    attacker-controlled bytes. Five bytes per observation:
    //    a candidate/non-candidate discriminator, a rank, and an age.
    let mut observed = Vec::new();
    for chunk in data.chunks_exact(5) {
        let rank = if chunk[0] & 1 == 0 {
            None
        } else {
            Some(u32::from(chunk[1]))
        };
        // Spread ages across the whole u64 range so the freshness
        // comparison is exercised at the extremes, not just near zero.
        let age_ms = u64::from(u32::from_be_bytes([chunk[1], chunk[2], chunk[3], chunk[4]]))
            .wrapping_mul(u64::from(chunk[0]).wrapping_add(1));
        observed.push(BeaconObservation { rank, age_ms });
    }
    let window = data
        .first()
        .map_or(0, |b| u64::from(*b) * 10_000);
    let own_rank = match data.get(1) {
        Some(b) if b & 1 == 0 => None,
        Some(b) => Some(u32::from(*b)),
        None => None,
    };

    let verdict = acting_committer_verdict(own_rank, &observed, window);

    match own_rank {
        // 3. A non-candidate is never talked into sweeping, no matter
        //    what the field looks like.
        None => assert_eq!(
            verdict,
            ActingCommitter::NotACandidate,
            "a device with no rank must never be a candidate",
        ),
        // 4. Strict inequality: deferring to an equal-or-worse rank
        //    would let a device stand down for its own echoed beacon.
        Some(own) => {
            if let ActingCommitter::Defer { to_rank } = verdict {
                assert!(
                    to_rank < own,
                    "deferred to rank {to_rank} which is not strictly better than {own}",
                );
            }
        }
    }
});
