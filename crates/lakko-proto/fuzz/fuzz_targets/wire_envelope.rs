// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `wire::v1::Envelope` prost decoder.

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message as _;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_proto::wire::v1::Envelope::decode(data);
});
