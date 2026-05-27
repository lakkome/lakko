// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse_unverified` JWS Compact parser.
//!
//! Splits the input on `'.'`, base64url-decodes the three
//! segments, JSON-decodes the header + claims. The
//! signature segment is decoded but NOT verified — this
//! function is "look at what the JWT claims to be" without
//! trusting it. Attacker controls the full string on every
//! client / SFU receive path. JWS parsers are a notorious
//! vulnerability surface (algorithm confusion, base64
//! length tricks, JSON injection on unknown fields,
//! integer-overflow in claims). Goal: prove the parser
//! never panics / overflows / loops forever on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_call_jwt::parse_unverified;

fuzz_target!(|data: &[u8]| {
    // The parser takes &str. Many JWT-style attacks rely
    // on non-UTF8 byte sequences smuggled into the header
    // / payload base64; UTF-8 decoding rejects them at the
    // outer layer before parse_unverified sees them. The
    // fuzzer is best served by feeding only well-formed
    // &str inputs so it learns to navigate the deeper
    // base64 / JSON branches.
    let Ok(jwt) = std::str::from_utf8(data) else {
        return;
    };
    let _ = parse_unverified(jwt);
});
