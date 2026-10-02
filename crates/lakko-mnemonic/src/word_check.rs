// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The local three-word check: "enter words 7, 3 and 12".
//!
//! A device that holds an account's keys also holds the recovery phrase
//! under its device key. When the person forgot the password, the client
//! draws three random positions ([`random_check_positions`], fresh for every
//! prompt), the person types the words at those positions, and the client
//! compares them with the phrase it holds ([`check_words`]). On a match the
//! client signs in and replaces the password with the seed it already has.
//!
//! The check runs entirely on the device. Nothing about the three words, the
//! positions or the outcome is stored on, or sent to, a server, and nothing
//! is derived from the words: they prove to the device that the person at
//! the keyboard has the written phrase, and that is all.
//!
//! The rule is written once here so the three clients cannot disagree on
//! how positions are drawn or how a typed word is normalised.

use lakko_crypto::random;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{Mnemonic, WORD_COUNT};

/// How many words the person is asked for.
pub const CHECK_WORDS: usize = 3;

/// Malformed input to [`check_words`]. A wrong or unknown word is not an
/// error: it is `Ok(false)`.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WordCheckError {
    /// Not exactly three positions.
    #[error("expected {CHECK_WORDS} positions")]
    WrongPositionCount,
    /// A position is outside `1..=12`.
    #[error("a position is outside 1..=12")]
    PositionOutOfRange,
    /// The same position appears more than once.
    #[error("positions must be distinct")]
    RepeatedPosition,
    /// Not exactly three words.
    #[error("expected {CHECK_WORDS} words")]
    WrongWordCount,
}

impl WordCheckError {
    /// A stable kebab-case code for the FFI surfaces.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongPositionCount => "wrong-position-count",
            Self::PositionOutOfRange => "position-out-of-range",
            Self::RepeatedPosition => "repeated-position",
            Self::WrongWordCount => "wrong-word-count",
        }
    }
}

/// Draw three distinct 1-based positions in `1..=12`, uniformly at random,
/// in the order drawn (not sorted: the prompt reads e.g. "words 7, 3 and
/// 12"). Call it afresh for every prompt.
#[must_use]
pub fn random_check_positions() -> [u8; CHECK_WORDS] {
    // WORD_COUNT is 12; the fallback is unreachable.
    let count = u8::try_from(WORD_COUNT).unwrap_or(12);
    // Largest multiple of `count` that fits a byte: bytes at or above it are
    // dropped so the low positions are not favoured (252 = 21 * 12).
    let limit = u8::MAX - (u8::MAX % count + 1) % count;
    let mut chosen = [0u8; CHECK_WORDS];
    let mut filled = 0;
    let mut byte = [0u8; 1];
    while filled < CHECK_WORDS {
        random::fill(&mut byte);
        if byte[0] > limit {
            continue;
        }
        let candidate = byte[0] % count + 1;
        if chosen[..filled].contains(&candidate) {
            continue;
        }
        chosen[filled] = candidate;
        filled += 1;
    }
    chosen
}

/// Do the three typed `words` match the phrase at the three `positions`?
///
/// `words[i]` answers `positions[i]`. Each typed word is trimmed and
/// lower-cased. All three are compared before the result is formed, so the
/// time taken does not tell which word was wrong.
///
/// Returns `Ok(false)` for a wrong word or a word outside the word list.
///
/// # Errors
///
/// [`WordCheckError`] when the input is malformed: not exactly three
/// positions, a position outside `1..=12`, a repeated position, or not
/// exactly three words.
pub fn check_words(
    mnemonic: &Mnemonic,
    positions: &[u8],
    words: &[&str],
) -> Result<bool, WordCheckError> {
    if positions.len() != CHECK_WORDS {
        return Err(WordCheckError::WrongPositionCount);
    }
    if positions
        .iter()
        .any(|&p| p == 0 || usize::from(p) > WORD_COUNT)
    {
        return Err(WordCheckError::PositionOutOfRange);
    }
    if positions
        .iter()
        .enumerate()
        .any(|(i, p)| positions[..i].contains(p))
    {
        return Err(WordCheckError::RepeatedPosition);
    }
    if words.len() != CHECK_WORDS {
        return Err(WordCheckError::WrongWordCount);
    }

    let phrase = Zeroizing::new(mnemonic.words());
    let mut all_match = true;
    for (&position, typed) in positions.iter().zip(words) {
        let typed = Zeroizing::new(typed.trim().to_lowercase());
        // The range check above makes the lookup total; a miss counts as a
        // mismatch rather than a panic.
        let expected = phrase
            .get(usize::from(position) - 1)
            .map_or("", String::as_str);
        // `&` not `&&`: every word is compared, whatever the earlier ones
        // gave.
        all_match &= !expected.is_empty() & bytes_equal(expected.as_bytes(), typed.as_bytes());
    }
    Ok(all_match)
}

/// Equality that looks at every byte of the shorter input before answering.
fn bytes_equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = u8::from(a.len() != b.len());
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(m: &Mnemonic, positions: &[u8]) -> Vec<String> {
        let words = m.words();
        positions
            .iter()
            .map(|&p| words[usize::from(p) - 1].clone())
            .collect()
    }

    fn refs(words: &[String]) -> Vec<&str> {
        words.iter().map(String::as_str).collect()
    }

    /// A word of the list that differs from `word`.
    fn another(word: &str) -> String {
        if word == "zoo" { "abandon" } else { "zoo" }.to_string()
    }

    #[test]
    fn every_draw_is_three_distinct_positions_in_range() {
        for _ in 0..5_000 {
            let p = random_check_positions();
            assert!(p.iter().all(|i| (1..=12).contains(i)), "{p:?}");
            assert!(p[0] != p[1] && p[0] != p[2] && p[1] != p[2], "{p:?}");
        }
    }

    #[test]
    fn draws_cover_every_position_in_every_slot() {
        // 12 values per slot, 5000 draws: a value missing from a slot has
        // probability about 12 * (11/12)^5000, far below anything observable.
        let mut seen = [[false; WORD_COUNT + 1]; CHECK_WORDS];
        for _ in 0..5_000 {
            for (slot, position) in random_check_positions().into_iter().enumerate() {
                seen[slot][usize::from(position)] = true;
            }
        }
        for slot in &seen {
            assert!(slot[1..].iter().all(|&s| s), "{slot:?}");
        }
    }

    #[test]
    fn draws_are_not_sorted() {
        // A sorted draw has probability 1/6; 200 sorted draws in a row do
        // not happen.
        let unsorted = (0..200).any(|_| {
            let p = random_check_positions();
            !(p[0] < p[1] && p[1] < p[2])
        });
        assert!(unsorted, "positions are returned in the order drawn");
    }

    #[test]
    fn draws_are_roughly_uniform() {
        let mut counts = [0u32; WORD_COUNT + 1];
        let draws = 12_000u32;
        for _ in 0..draws {
            for position in random_check_positions() {
                counts[usize::from(position)] += 1;
            }
        }
        // Each position is expected draws * 3 / 12 = 3000 times; the
        // standard deviation is about 47, so 2500..3500 is over ten of them.
        for (position, &n) in counts.iter().enumerate().skip(1) {
            assert!((2_500..3_500).contains(&n), "position {position}: {n}");
        }
    }

    #[test]
    fn the_right_words_match_for_every_drawn_position_set() {
        let m = Mnemonic::generate();
        for _ in 0..200 {
            let positions = random_check_positions();
            let words = picked(&m, &positions);
            assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(true));
        }
    }

    #[test]
    fn words_answer_positions_in_the_order_asked() {
        let m = Mnemonic::from_phrase(
            "legal winner thank year wave sausage worth useful legal winner thank yellow",
        )
        .unwrap();
        assert_eq!(
            check_words(&m, &[7, 3, 12], &["worth", "thank", "yellow"]),
            Ok(true)
        );
        // The same words in ascending-position order answer another question.
        assert_eq!(
            check_words(&m, &[7, 3, 12], &["thank", "worth", "yellow"]),
            Ok(false)
        );
    }

    #[test]
    fn a_wrong_word_in_any_slot_fails() {
        let m = Mnemonic::generate();
        let positions = [2u8, 11, 5];
        for slot in 0..CHECK_WORDS {
            let mut words = picked(&m, &positions);
            words[slot] = another(&words[slot]);
            assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(false));
        }
    }

    #[test]
    fn an_unknown_or_empty_word_is_false_not_an_error() {
        let m = Mnemonic::generate();
        let positions = [1u8, 2, 3];
        let mut words = picked(&m, &positions);
        words[1] = "notaword".into();
        assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(false));
        words[1] = String::new();
        assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(false));
        // A prefix or an extension of the right word is still wrong.
        let right = picked(&m, &positions);
        words[1] = format!("{}x", right[1]);
        assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(false));
        words[1] = right[1][..right[1].len() - 1].to_string();
        assert_eq!(check_words(&m, &positions, &refs(&words)), Ok(false));
    }

    #[test]
    fn typed_words_are_trimmed_and_lower_cased() {
        let m = Mnemonic::generate();
        let positions = [12u8, 1, 6];
        let shouted: Vec<String> = picked(&m, &positions)
            .iter()
            .map(|w| format!("  {}\t", w.to_uppercase()))
            .collect();
        assert_eq!(check_words(&m, &positions, &refs(&shouted)), Ok(true));
    }

    #[test]
    fn malformed_input_is_an_error() {
        let m = Mnemonic::generate();
        let w = ["abandon", "ability", "able"];
        assert_eq!(
            check_words(&m, &[1, 2], &w),
            Err(WordCheckError::WrongPositionCount)
        );
        assert_eq!(
            check_words(&m, &[1, 2, 3, 4], &w),
            Err(WordCheckError::WrongPositionCount)
        );
        assert_eq!(
            check_words(&m, &[0, 2, 3], &w),
            Err(WordCheckError::PositionOutOfRange)
        );
        assert_eq!(
            check_words(&m, &[1, 2, 13], &w),
            Err(WordCheckError::PositionOutOfRange)
        );
        assert_eq!(
            check_words(&m, &[4, 9, 4], &w),
            Err(WordCheckError::RepeatedPosition)
        );
        assert_eq!(
            check_words(&m, &[1, 2, 3], &w[..2]),
            Err(WordCheckError::WrongWordCount)
        );
        assert_eq!(
            check_words(&m, &[1, 2, 3], &["a", "b", "c", "d"]),
            Err(WordCheckError::WrongWordCount)
        );
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(
            WordCheckError::WrongPositionCount.code(),
            "wrong-position-count"
        );
        assert_eq!(
            WordCheckError::PositionOutOfRange.code(),
            "position-out-of-range"
        );
        assert_eq!(WordCheckError::RepeatedPosition.code(), "repeated-position");
        assert_eq!(WordCheckError::WrongWordCount.code(), "wrong-word-count");
    }
}
