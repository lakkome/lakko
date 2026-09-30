// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `DeviceGroup::parse_devicegroup_payload` (the receiver-side
//! DeviceGroup journal parser). The bytes are attacker-touchable: a DeviceGroup
//! journal `Incoming::Application` payload arrives from the ZERO-TRUST, opaque
//! Delivery Service, so a malicious / corrupt entry reaches this prost-decode +
//! body-variant-extraction path before any group state is consulted. Any
//! panic / overflow / runaway here is a vulnerability.
//!
//! ADR-0058: the same bytes reach the `FolderChanges` / `StateSnapshot`
//! bounds (folder id length, name rule, ref kind and id length, unknown
//! bitfield bits, pin / order / register caps, snapshot part bounds). The
//! harness ASSERTS that nothing the decoder accepts breaks one of them, and
//! that the fold accepts every register of an accepted payload.
//!
//! ADR-0060 section 7 added the `ServiceEnvelope` body (field 20): an
//! accepted one must be 1 to `MAX_SERVICE_ENVELOPE_LEN` bytes, asserted
//! below, so a bound regression is a crash rather than a silent pass.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::parse_devicegroup_payload(data);
    assert!(lakko_mls::fuzz_api::devicegroup_payload_accepted_within_bounds(data));
    assert!(lakko_mls::fuzz_api::devicegroup_service_envelope_within_bounds(data));
});
