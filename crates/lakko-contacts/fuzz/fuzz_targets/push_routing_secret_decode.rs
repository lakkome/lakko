// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_push_routing_secret` (ADR-0042, v2
//! envelope) + the `push_routing_secret_is_newer` total order.
//!
//! The in-group push-routing-secret distribution envelope rides
//! an MLS application message on the group journal, so the bytes
//! reach this bounded, fail-closed decoder after arriving from
//! the ZERO-TRUST, opaque Delivery Service — a malicious member
//! or a corrupt journal entry could have crafted them, and the
//! receive-loop dispatch reaches this parser before any state is
//! consulted (the same threat class as `media_state_decode` /
//! `welcome_rendezvous_decode`). Any arbitrary `data` is a valid
//! input — a plaintext that is not a push-routing-secret envelope
//! must surface as a graceful `Err` (wrong domain / bad length /
//! unknown version), never a panic. `decode_push_routing_secret`
//! wraps the same parse path the composite
//! `dispatch_incoming_envelope` uses, so this target pins the
//! standalone strict decoder surface.
//!
//! ## What the v2 bump added to this surface
//!
//! The decode now splits an 8-byte big-endian `mint_epoch` out of
//! the payload ahead of the 32-byte secret, and the version gate
//! moved AHEAD of the exact-length gate — so the parser reads
//! `payload[0]` before it knows the payload length, and an empty
//! payload must be caught by the split rather than by an index.
//! Both are reachable from raw bytes here.
//!
//! The target also drives the ordering decision the decoded
//! envelope feeds, because a decoded envelope is never used for
//! anything else: the attacker-supplied `(mint_epoch, secret)` is
//! compared against a fixed stored pair via
//! `push_routing_secret_is_newer` and, through
//! `PushRoutingSecretEnvelope::supersedes`, against itself. The
//! order is total and infallible by contract — no input may panic
//! it — and `supersedes` against an identical value must be
//! `false` (the F-A "a retry is not an update" property), which is
//! asserted here on every corpus entry that parses.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::push_routing::{
    decode_push_routing_secret, push_routing_secret_is_newer, PUSH_ROUTING_SECRET_LEN,
};

/// Fixed "already stored" secret to order the fuzzer's envelope
/// against. Arbitrary but constant, so a crash reproduces.
const STORED_SECRET: [u8; PUSH_ROUTING_SECRET_LEN] = [0x5Au8; PUSH_ROUTING_SECRET_LEN];
const STORED_EPOCH: u64 = 0x0000_0000_DEAD_BEEF;

fuzz_target!(|data: &[u8]| {
    // 1. The bounded, fail-closed decode: domain prefix, version
    //    gate, exact length, epoch/secret split.
    let Ok(envelope) = decode_push_routing_secret(data) else {
        return;
    };

    // 2. The total order the decoded envelope feeds. Must be
    //    infallible for every parseable envelope, in both
    //    directions.
    let _ = push_routing_secret_is_newer(
        envelope.mint_epoch,
        &envelope.secret,
        STORED_EPOCH,
        &STORED_SECRET,
    );
    let _ = push_routing_secret_is_newer(
        STORED_EPOCH,
        &STORED_SECRET,
        envelope.mint_epoch,
        &envelope.secret,
    );

    // 3. Irreflexivity — the F-A property, on attacker-chosen
    //    bytes: an envelope never supersedes itself, so a replayed
    //    or retried broadcast can never read as an update no
    //    matter what epoch/secret the attacker picked.
    assert!(
        !envelope.supersedes(&envelope),
        "an envelope must never supersede itself",
    );
});
