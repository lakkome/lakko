// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `mls_app::v1::ApplicationPayload` prost decoder.
//!
//! Exercises the full nested oneof / repeated / bytes surface of the
//! application-message schema.

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message as _;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_proto::mls_app::v1::ApplicationPayload::decode(data);
});
