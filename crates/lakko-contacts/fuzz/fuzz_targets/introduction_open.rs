// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - introduction record parse + open (ADR-0060 §4).
//!
//! A record reaches a searcher inside a contact's bundle, and a relaying
//! contact inside an offer; either peer can craft it. The input is run
//! through three surfaces:
//!
//! * `IntroductionRecord::decode` + `introduction_tags` - the structure a
//!   searcher parses before any key is derived;
//! * `open_introduction` under a fixed OPRF output - the tag match among up
//!   to 65 entries, the content-key unwrap and the payload open (a forged
//!   entry is an `Open` error, never a panic) and, on the rare input whose
//!   tag matches and whose wrap authenticates, the payload decode;
//! * `IntroductionPayload::decode` on the raw bytes - the payload parser is
//!   otherwise reachable only past the AEAD, so it is fed directly too.
//!
//! Asserted: no panic, and round-trip stability of whatever decodes (the
//! record and payload encodings are canonical).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::introduction::{
    introduction_tags, open_introduction, IntroductionPayload, IntroductionRecord,
    INTRODUCTION_RECORD_MAX_LEN, INTRODUCTION_RECORD_MIN_LEN, OPRF_OUTPUT_LEN,
};

const OPRF: [u8; OPRF_OUTPUT_LEN] = [0x5A; OPRF_OUTPUT_LEN];

fuzz_target!(|data: &[u8]| {
    if let Ok(record) = IntroductionRecord::decode(data) {
        assert!((INTRODUCTION_RECORD_MIN_LEN..=INTRODUCTION_RECORD_MAX_LEN).contains(&data.len()));
        let encoded = record
            .encode()
            .expect("encode must accept what decode accepted");
        assert_eq!(
            encoded.as_slice(),
            data,
            "the record encoding must be canonical"
        );
        assert_eq!(
            introduction_tags(data).expect("tags of a decoded record"),
            record.tags()
        );
        let _ = open_introduction(data, &OPRF);
    }
    if let Ok(payload) = IntroductionPayload::decode(data) {
        let encoded = payload
            .encode()
            .expect("encode must accept what decode accepted");
        assert_eq!(
            encoded.as_slice(),
            data,
            "the payload encoding must be canonical"
        );
    }
});
