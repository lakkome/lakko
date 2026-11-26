// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `RecoveryOwnerProof::parse` and
//! `verify_recovery_owner_change` (ADR-0101 section 7, the channel recovery
//! key).
//!
//! `PUT /v1/channels/<id>/owner` with a recovery proof takes a header anyone
//! can send; the service parses it before it looks at the stored recovery
//! key.
//!
//! Properties asserted, not merely exercised:
//!
//! 1. The parser never panics.
//! 2. A header that parses re-encodes to text that parses to the same proof.
//! 3. A parsed proof never verifies under a fixed recovery key the fuzzer
//!    does not hold.

#![no_main]

use lakko_channels::{verify_recovery_owner_change, ChannelRecoveryKey, RecoveryOwnerProof};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Ok(proof) = RecoveryOwnerProof::parse(text) else {
        return;
    };
    assert_eq!(
        RecoveryOwnerProof::parse(&proof.encode()).ok(),
        Some(proof.clone()),
        "a parsed proof must survive a re-encode"
    );
    let key = ChannelRecoveryKey::from_seed(&[0x31; 32]);
    let new_owner = ChannelRecoveryKey::from_seed(&[0x32; 32])
        .verifying_key()
        .to_bytes();
    assert!(verify_recovery_owner_change(
        &proof,
        &[0x0C; 16],
        &new_owner,
        &key.verifying_key().to_bytes(),
        proof.unix_ms,
    )
    .is_err());
});
