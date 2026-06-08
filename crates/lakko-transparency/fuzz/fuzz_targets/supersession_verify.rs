// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — full `verify_supersession` pipeline (G-4).
//!
//! Wider than `supersession_open`: after the parse + embedded-key signature
//! check, this adds the trusted-old-key match, the `<= 30-day`
//! validity-window gate, and the freshness / expiry comparison. Fixed
//! trusted old key (derived from an all-zero seed) + fixed `now` so coverage
//! focuses on the parser / gate branches rather than drifting on the wall
//! clock. The peer fully controls the proof bytes on the
//! verify-on-pubkey-change path, which is now FFI-exposed via
//! `verify_supersession` on UniFFI + wasm.

#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::SigningKey;
use lakko_transparency::verify_supersession;

/// The fixed key the harness "already trusts", derived once per process from
/// an all-zero seed so the run is reproducible. The fuzzer can in principle
/// learn to emit a proof whose embedded `old_pubkey` matches this, exercising
/// the deeper window / expiry branches past the old-key-mismatch wall.
fn fixture_trusted_old_pubkey() -> &'static [u8; 32] {
    static PK: OnceLock<[u8; 32]> = OnceLock::new();
    PK.get_or_init(|| SigningKey::from_seed(&[0u8; 32]).verifying_key().to_bytes())
}

fuzz_target!(|data: &[u8]| {
    // Fixed `now` so the expiry branch is deterministic — a wall-clock-
    // dependent harness would turn a parser panic into a flaky `Expired`
    // rejection on slow runs.
    let now_unix_secs: i64 = 1_700_000_000;
    let _ = verify_supersession(data, fixture_trusted_old_pubkey(), now_unix_secs);
});
