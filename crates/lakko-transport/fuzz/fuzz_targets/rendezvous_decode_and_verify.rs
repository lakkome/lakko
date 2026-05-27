// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — full rendezvous `BootstrapConfig` decode
//! + verify pipeline.
//!
//! Stage 1: `serde_json::from_slice::<BootstrapConfig>`
//! over arbitrary attacker-controlled bytes. Stage 2: if
//! parse succeeds, call `BootstrapConfig::verify` with a
//! fixed trust anchor + fixed `now`. The verify step
//! exercises the 4-stage gate (version + expiry +
//! issued-in-future + signature). Both stages are
//! pre-trust-decision parsers; any panic / overflow /
//! stack abuse on attacker input is a vulnerability on
//! the bootstrap-config delivery path (DoH → IP-pinned
//! HTTPS → verify). Pinned in audit-pack invariants T-4
//! / T-5 / T-7 / T-8.

#![no_main]

use std::time::{Duration, UNIX_EPOCH};

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::SigningKey;
use lakko_transport::rendezvous::BootstrapConfig;

fuzz_target!(|data: &[u8]| {
    // Fixed trust anchor — derive deterministically from
    // an all-zero seed so the fuzz harness is reproducible
    // and the verify-side branch coverage is stable.
    let trust_anchor = SigningKey::from_seed(&[0u8; 32]).verifying_key();
    // Fixed `now` so the expiry / issued-in-future
    // branches are deterministic across machines.
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let Ok(cfg) = serde_json::from_slice::<BootstrapConfig>(data) else {
        return;
    };
    let _ = cfg.verify(&trust_anchor, now);
});
