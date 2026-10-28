// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `SelfRemovalProof::decode` and `covers` (the ADR-0081
//! amendment's DeviceGroup self-removal proof). The bytes travel in the
//! removal-authorisation slot of the DeviceGroup journal envelope, which a
//! device of the account or the DS chooses, and every peer decodes them
//! before it decides whether a removal under the attested-removal policy is
//! covered. Asserts no panic, and (inside
//! `fuzz_api::decode_self_removal_proof`) that anything that decodes
//! re-encodes to the identical bytes and that `covers` answers on the
//! attacker-chosen fields.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_self_removal_proof(data);
});
