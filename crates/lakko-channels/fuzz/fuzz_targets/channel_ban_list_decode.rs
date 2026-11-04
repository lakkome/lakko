// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_ban_list` (ADR-0087 section 13a).
//!
//! A public channel's ban list is the plaintext of a post, so the bytes come
//! from any publisher key (a malicious co-publisher, or a key that leaked),
//! and every reader decodes them: the domain, the version byte, the stamp,
//! the two-byte count checked against its cap before anything is copied, the
//! exact length, and the no-repeat rule.
//!
//! Asserted: no panic; a decoded list is within its bounds, validates, has
//! no repeat, and re-composes to the identical bytes (the encoding is
//! canonical); the classifier agrees with the strict decoder.

#![no_main]

use lakko_channels::{
    classify_post_plaintext, compose_ban_list, decode_ban_list, PostContent, BAN_LIST_DOMAIN_V1,
    BAN_LIST_MAX_KEYS, BAN_LIST_MAX_LEN, BAN_LIST_MIN_LEN,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = BAN_LIST_DOMAIN_V1.to_vec();
    input.extend_from_slice(data);
    let strict = decode_ban_list(&input);
    match (&strict, classify_post_plaintext(&input)) {
        (Ok(list), PostContent::BanList(classified)) => {
            assert_eq!(list, &classified, "the classifier must agree");
            assert!(input.len() >= BAN_LIST_MIN_LEN && input.len() <= BAN_LIST_MAX_LEN);
            assert!(list.authors.len() <= BAN_LIST_MAX_KEYS);
            assert!(list.validate().is_ok());
            let recomposed = compose_ban_list(list).expect("a decoded list must re-compose");
            assert_eq!(recomposed, input, "the encoding must be canonical");
        },
        (Err(e), PostContent::MalformedBanList(c)) => assert_eq!(*e, c),
        _ => panic!("the classifier and the strict decoder disagree"),
    }
    // Bytes without the domain are never a ban list.
    assert!(decode_ban_list(data).is_err() || data.starts_with(BAN_LIST_DOMAIN_V1));
});
