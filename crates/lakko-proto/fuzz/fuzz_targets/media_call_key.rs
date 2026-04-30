// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `media::v1::CallKeyEpoch` and `CodecCapability` decoders.

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message as _;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_proto::media::v1::CallKeyEpoch::decode(data);
    let _ = lakko_proto::media::v1::CodecCapability::decode(data);
});
