// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko_mnemonic::word_check`: the local
//! three-word check ("enter words 7, 3 and 12").
//!
//! A device that holds the recovery phrase asks for three words at random
//! positions and compares them itself. Nothing here talks to a server and
//! nothing about the words is stored or sent anywhere.
//!
//! - `phraseCheckRandomPositions() -> Uint8Array`: three distinct 1-based
//!   positions in `1..=12`, in the order to ask for them. Draw afresh for
//!   every prompt.
//! - `phraseCheckWords(mnemonic, positions, words) -> boolean`: whether
//!   `words[i]` is the phrase's word at `positions[i]` for all three. Typed
//!   words are trimmed and lower-cased. A wrong or unknown word is `false`;
//!   malformed input throws.

use lakko_mnemonic::word_check;
use wasm_bindgen::prelude::*;

use crate::mnemonic::RecoveryMnemonic;

/// Draw three distinct 1-based word positions in `1..=12`, uniformly at
/// random, in the order drawn (not sorted).
#[wasm_bindgen(js_name = phraseCheckRandomPositions)]
#[must_use]
pub fn phrase_check_random_positions() -> Vec<u8> {
    word_check::random_check_positions().to_vec()
}

/// Do the three typed words match the phrase at the three positions?
/// `words[i]` answers `positions[i]`.
///
/// # Errors
///
/// Throws an `Error` whose message is `"<code>: <reason>"` when the input
/// is malformed: `wrong-position-count`, `position-out-of-range`,
/// `repeated-position` or `wrong-word-count`. A wrong word does not throw.
#[wasm_bindgen(js_name = phraseCheckWords)]
pub fn phrase_check_words(
    mnemonic: &RecoveryMnemonic,
    positions: &[u8],
    words: Vec<String>,
) -> Result<bool, JsError> {
    check(mnemonic, positions, &words).map_err(|e| JsError::new(&format!("{}: {e}", e.code())))
}

fn check(
    mnemonic: &RecoveryMnemonic,
    positions: &[u8],
    words: &[String],
) -> Result<bool, word_check::WordCheckError> {
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    word_check::check_words(mnemonic.inner(), positions, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawn_positions_and_the_right_words_match() {
        let m = RecoveryMnemonic::generate();
        let positions = phrase_check_random_positions();
        assert_eq!(positions.len(), 3);
        let all = m.words();
        let mut words: Vec<String> = positions
            .iter()
            .map(|&p| all[usize::from(p) - 1].to_uppercase())
            .collect();
        assert!(phrase_check_words(&m, &positions, words.clone()).unwrap());
        words[2] = "notaword".into();
        assert!(!phrase_check_words(&m, &positions, words).unwrap());
    }

    #[test]
    fn malformed_input_is_refused() {
        // The throwing path constructs a `JsError`, which only exists on
        // wasm32; the inner function carries the same decision.
        let m = RecoveryMnemonic::generate();
        let w: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        assert!(check(&m, &[1, 1, 2], &w).is_err());
        assert!(check(&m, &[1, 2, 13], &w).is_err());
        assert!(check(&m, &[1, 2], &w).is_err());
        assert!(check(&m, &[1, 2, 3], &w[..2]).is_err());
    }
}
