// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `profile_request::decode_profile_request` (ADR-0048 §2 trigger 4): the
//! newcomer's "introduce yourselves" envelope rides an MLS application
//! message on the group journal, so these bytes reach the decoder after the
//! zero-trust Delivery Service and before any state is consulted, from any
//! member. The parser is tiny (domain, exact length, version byte) and the
//! consequence of getting either gate wrong is a request that renders as a
//! chat bubble or one that is answered when it should be dropped.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::profile_request::{
    compose_profile_request, decode_profile_request, try_unwrap_profile_request,
    PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
};

fuzz_target!(|data: &[u8]| {
    let _ = decode_profile_request(data);
    let classified = try_unwrap_profile_request(data);
    if !data.starts_with(PROFILE_REQUEST_ENVELOPE_DOMAIN_V1) {
        assert_eq!(classified, Ok(None), "a foreign domain is never a request");
    }
    if classified == Ok(Some(Default::default())) {
        assert_eq!(data, compose_profile_request().as_slice(), "only one wire is a request");
    }
});
