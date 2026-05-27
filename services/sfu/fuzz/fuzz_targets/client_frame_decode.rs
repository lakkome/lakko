// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — SFU `ClientFrame` JSON decoder.
//!
//! `services/sfu/src/signaling.rs` parses every inbound
//! WS text frame via `serde_json::from_str::<ClientFrame>`.
//! The parser runs AFTER JWT admission (the JWT check
//! happens at WS upgrade time) but BEFORE any routing
//! decision; an admitted-but-malicious peer can ship
//! arbitrary text frames over its already-open connection
//! and reach this parser with fully attacker-controlled
//! bytes. A panic / overflow / stack abuse here crashes
//! the per-call task and would amplify into a reconnect
//! storm across every peer in the call.
//!
//! Pinned in audit-pack invariant C-39 (SFU signaling
//! layer enforces zero-payload-inspection + cross-call
//! isolation); this harness backs the "parser does not
//! panic on arbitrary input" sub-property.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_svc_sfu::protocol::ClientFrame;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ClientFrame>(data);
});
