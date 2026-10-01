// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entry points for the fuzz harness. Not a supported API.

use crate::rule::{decode_feed_rule, encode_feed_rule, FEED_RULE_MAX_LEN};

/// Decode arbitrary bytes and check the properties the decoder promises.
///
/// # Panics
///
/// When a property fails: that is the finding the fuzzer reports.
pub fn decode_and_check(data: &[u8]) {
    let Ok(rule) = decode_feed_rule(data) else {
        return;
    };
    assert!(
        data.len() <= FEED_RULE_MAX_LEN,
        "a decoded rule is within the size bound"
    );
    assert!(rule.validate().is_ok(), "a decoded rule validates");
    let again = encode_feed_rule(&rule);
    assert_eq!(again.as_deref(), Ok(data), "the encoding is canonical");
    let exported = rule.export();
    assert!(
        exported.include.is_empty() && exported.exclude.is_empty(),
        "an exported rule carries no conversation id"
    );
    assert!(
        encode_feed_rule(&exported).is_ok(),
        "an exported rule encodes"
    );
}
