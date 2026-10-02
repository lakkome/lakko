// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko_mnemonic::word_check`: the local three-word
//! check ("enter words 7, 3 and 12"). Mirror of
//! `crates/lakko-ffi-wasm/src/phrase_check.rs`.
//!
//! A device that holds the recovery phrase asks for three words at random
//! positions and compares them itself. Nothing here talks to a server and
//! nothing about the words is stored or sent anywhere.

use std::sync::Arc;

use lakko_mnemonic::word_check;

use crate::mnemonic::RecoveryMnemonic;
use crate::FfiError;

/// Draw three distinct 1-based word positions in `1..=12`, uniformly at
/// random, in the order to ask for them (not sorted). Three bytes. Draw
/// afresh for every prompt.
#[uniffi::export]
#[must_use]
pub fn phrase_check_random_positions() -> Vec<u8> {
    word_check::random_check_positions().to_vec()
}

/// Do the three typed words match the phrase at the three positions?
/// `words[i]` answers `positions[i]`. Typed words are trimmed and
/// lower-cased; a wrong or unknown word is `false`.
///
/// # Errors
///
/// [`FfiError::PhraseCheck`] when the input is malformed. The message starts
/// with a code (`wrong-position-count`, `position-out-of-range`,
/// `repeated-position`, `wrong-word-count`) followed by `": "` and the
/// reason.
#[uniffi::export]
pub fn phrase_check_words(
    mnemonic: Arc<RecoveryMnemonic>,
    positions: Vec<u8>,
    words: Vec<String>,
) -> Result<bool, FfiError> {
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    word_check::check_words(mnemonic.inner(), &positions, &refs)
        .map_err(|e| FfiError::PhraseCheck(format!("{}: {e}", e.code())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(m: &RecoveryMnemonic, positions: &[u8]) -> Vec<String> {
        let words = m.words();
        positions
            .iter()
            .map(|&p| words[usize::from(p) - 1].clone())
            .collect()
    }

    #[test]
    fn drawn_positions_are_three_distinct_values_in_range() {
        for _ in 0..200 {
            let p = phrase_check_random_positions();
            assert_eq!(p.len(), 3);
            assert!(p.iter().all(|i| (1..=12).contains(i)));
            assert!(p[0] != p[1] && p[0] != p[2] && p[1] != p[2]);
        }
    }

    #[test]
    fn the_right_words_match_and_a_wrong_one_does_not() {
        let m = Arc::new(RecoveryMnemonic::generate());
        let positions = phrase_check_random_positions();
        let mut words = picked(&m, &positions);
        assert!(phrase_check_words(m.clone(), positions.clone(), words.clone()).unwrap());
        let shouted: Vec<String> = words
            .iter()
            .map(|w| format!(" {} ", w.to_uppercase()))
            .collect();
        assert!(phrase_check_words(m.clone(), positions.clone(), shouted).unwrap());
        words[0] = "notaword".into();
        assert!(!phrase_check_words(m, positions, words).unwrap());
    }

    #[test]
    fn malformed_input_carries_a_code() {
        let m = Arc::new(RecoveryMnemonic::generate());
        let w = || vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let code = |r: Result<bool, FfiError>| match r {
            Err(FfiError::PhraseCheck(msg)) => msg.split(':').next().unwrap_or("").to_string(),
            other => panic!("expected PhraseCheck, got {other:?}"),
        };
        assert_eq!(
            code(phrase_check_words(m.clone(), vec![1, 2], w())),
            "wrong-position-count"
        );
        assert_eq!(
            code(phrase_check_words(m.clone(), vec![0, 2, 3], w())),
            "position-out-of-range"
        );
        assert_eq!(
            code(phrase_check_words(m.clone(), vec![1, 2, 13], w())),
            "position-out-of-range"
        );
        assert_eq!(
            code(phrase_check_words(m.clone(), vec![5, 2, 5], w())),
            "repeated-position"
        );
        assert_eq!(
            code(phrase_check_words(m, vec![1, 2, 3], vec!["a".into()])),
            "wrong-word-count"
        );
    }
}
