// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `DeviceGroup::parse_devicegroup_payload` (the receiver-side
//! DeviceGroup journal parser). The bytes are attacker-touchable: a DeviceGroup
//! journal `Incoming::Application` payload arrives from the ZERO-TRUST, opaque
//! Delivery Service, so a malicious / corrupt entry reaches this prost-decode +
//! body-variant-extraction path before any group state is consulted. Any
//! panic / overflow / runaway here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::parse_devicegroup_payload(data);
});
