// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_push_routing_secret` (ADR-0042).
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

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::push_routing::decode_push_routing_secret(data);
});
