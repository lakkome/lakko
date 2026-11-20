// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_account_deleted` + `verify_account_deleted` + the
//! dispatcher (ADR-0098).
//!
//! An account-deleted statement rides an MLS application message in a
//! conversation, so any member (or the zero-trust DS replaying a corrupt row)
//! chooses the bytes, and they reach this bounded, fail-closed decoder before
//! any state is consulted: the domain, a version byte checked before the
//! length, a fixed-length signature, then the signature under the sender's
//! account key resolved at receipt.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder, the verifier or the dispatcher;
//! * a decoded statement has the one length and re-encodes to the input;
//! * the dispatcher classifies the same bytes the same way;
//! * nothing the fuzzer produces verifies under a fixed key it does not hold
//!   the seed of, for the group id it is given.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::account_deleted::{
    decode_account_deleted, verify_account_deleted, ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1,
    ACCOUNT_DELETED_ENVELOPE_LEN, ACCOUNT_DELETED_VERSION_V1,
};
use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};

/// A valid Ed25519 public key whose seed the fuzzer never sees (RFC 8032
/// test key 1).
const ACCOUNT_KEY: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

fuzz_target!(|data: &[u8]| {
    let decoded = decode_account_deleted(data);
    if let Ok(statement) = &decoded {
        assert_eq!(data.len(), ACCOUNT_DELETED_ENVELOPE_LEN);
        let mut again = Vec::with_capacity(ACCOUNT_DELETED_ENVELOPE_LEN);
        again.extend_from_slice(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1);
        again.push(ACCOUNT_DELETED_VERSION_V1);
        again.extend_from_slice(&statement.signature);
        assert_eq!(again, data);
        assert!(
            verify_account_deleted(data, &[0x33; 32], &ACCOUNT_KEY).is_err(),
            "a forged statement verified"
        );
        assert!(matches!(
            dispatch_incoming_envelope(data),
            Ok(IncomingEnvelope::AccountDeleted(_))
        ));
    } else if data.starts_with(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1) {
        assert!(dispatch_incoming_envelope(data).is_err());
    }
    // The verifier with arbitrary group ids and keys drawn from the input.
    let group = data.get(..data.len().min(300)).unwrap_or(&[]);
    let _ = verify_account_deleted(data, group, data.get(..32).unwrap_or(&[]));
});
