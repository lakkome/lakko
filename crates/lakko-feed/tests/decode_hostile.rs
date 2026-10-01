// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The fuzz harness's properties, driven by proptest so they run in every
//! `cargo test` and not only where cargo-fuzz is installed.
//!
//! Random bytes rarely get past the domain prefix, so most cases start from
//! a valid encoding and damage it: one byte replaced, or the input cut short.

use lakko_feed::{encode_feed_rule, fuzz_api, preset_rule, FeedRule, ALL_PRESETS};
use proptest::prelude::*;

fn valid_encodings() -> Vec<Vec<u8>> {
    let mut rich = FeedRule::new("Books");
    rich.include = vec![vec![7; 32], vec![8; 3]];
    rich.exclude = vec![vec![9; 64]];
    rich.hide_words = vec!["spoiler".into()];
    rich.only_words = vec!["книги".into(), "books".into()];
    rich.min_reactions = 300;
    let mut out: Vec<Vec<u8>> = ALL_PRESETS
        .iter()
        .map(|p| encode_feed_rule(&preset_rule(*p)).unwrap())
        .collect();
    out.push(encode_feed_rule(&rich).unwrap());
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn random_bytes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        fuzz_api::decode_and_check(&data);
    }

    #[test]
    fn one_byte_changed(which in 0usize..4, pos in any::<usize>(), byte in any::<u8>()) {
        let mut data = valid_encodings()[which].clone();
        let i = pos % data.len();
        data[i] = byte;
        fuzz_api::decode_and_check(&data);
    }

    #[test]
    fn cut_short_or_extended(which in 0usize..4, cut in any::<usize>(), tail in proptest::collection::vec(any::<u8>(), 0..8)) {
        let mut data = valid_encodings()[which].clone();
        data.truncate(cut % (data.len() + 1));
        data.extend_from_slice(&tail);
        fuzz_api::decode_and_check(&data);
    }
}
