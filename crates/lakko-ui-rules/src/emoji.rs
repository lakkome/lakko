// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What counts as ONE emoji, written once (UX `docs/ux/reactions-and-emoji.md`
//! section 17 item 5, decision D10: the rule in Rust, not three
//! implementations).
//!
//! Two questions use it: whether a string is a reaction a client offers and
//! shows ([`is_single_emoji`]), and whether a message is one to three emoji
//! and nothing else, drawn large ([`emoji_only_count`]). Before this the web
//! client asked `Intl.Segmenter`, iOS asked Swift's `Character`, and Android
//! had a check of its own, so the three disagreed on edge cases.
//!
//! ## The rule
//!
//! The text is split into Unicode extended grapheme clusters
//! (`unicode-segmentation`). A cluster is an emoji when it is one of:
//!
//! * a **flag**: exactly two regional indicators;
//! * a **keycap**: `0`-`9`, `#` or `*`, an optional VARIATION SELECTOR-16,
//!   then COMBINING ENCLOSING KEYCAP (U+20E3);
//! * a **subdivision flag**: WAVING BLACK FLAG (U+1F3F4), one or more tag
//!   characters (U+E0020..=U+E007E), then CANCEL TAG (U+E007F);
//! * a **pictograph sequence**: it opens with an Extended_Pictographic
//!   character that is not a skin-tone modifier, and every other character
//!   is VARIATION SELECTOR-16, a skin-tone modifier (U+1F3FB..=U+1F3FF), or
//!   a ZERO WIDTH JOINER followed directly by another Extended_Pictographic
//!   character.
//!
//! Anything else is not an emoji, notably a cluster holding a letter, a digit
//! without the keycap mark, a combining mark, or VARIATION SELECTOR-15 (the
//! sender asked for the text presentation, so it is not drawn as an emoji).
//! Extended_Pictographic covers the code points Unicode reserves for future
//! emoji in the emoji blocks, so an emoji newer than this build's data still
//! counts.
//!
//! The Extended_Pictographic data comes from `icu_properties` (ICU4X, the
//! family `lakko-l10n` already uses): `unicode-segmentation` uses the
//! property internally but does not expose it.
//!
//! ## What the rule does NOT decide
//!
//! It is a display rule. Whether the wire carries a reaction is the parser's
//! byte bound (`lakko_attachments::is_reaction_emoji`); the fold in
//! [`crate::reaction`] counts every reaction the parser accepted, and the
//! chip rule hides one that is not an emoji, as it hides one the
//! conversation does not allow.

use icu_properties::props::ExtendedPictographic;
use icu_properties::CodePointSetData;
use unicode_segmentation::UnicodeSegmentation;

/// The most emoji a message may hold and still be drawn large (UX section
/// 13: one to three).
pub const EMOJI_ONLY_MAX: u8 = 3;

const ZWJ: char = '\u{200D}';
const VS16: char = '\u{FE0F}';
const KEYCAP: char = '\u{20E3}';
const BLACK_FLAG: char = '\u{1F3F4}';
const CANCEL_TAG: char = '\u{E007F}';

fn is_ext_pict(c: char) -> bool {
    CodePointSetData::new::<ExtendedPictographic>().contains(c)
}

const fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

const fn is_skin_tone(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

const fn is_tag(c: char) -> bool {
    matches!(c, '\u{E0020}'..='\u{E007E}')
}

/// Whether one grapheme cluster is an emoji, by the rule in the module docs.
pub(crate) fn is_emoji_cluster(cluster: &str) -> bool {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let rest: &str = chars.as_str();

    // A flag: two regional indicators and nothing else.
    if is_regional_indicator(first) {
        let mut r = rest.chars();
        return matches!((r.next(), r.next()), (Some(c), None) if is_regional_indicator(c));
    }

    // A keycap: base, optional VS16, the keycap mark.
    if matches!(first, '0'..='9' | '#' | '*') {
        let tail: Vec<char> = rest.chars().take(3).collect();
        return tail == [KEYCAP] || tail == [VS16, KEYCAP];
    }

    // A subdivision flag: the black flag, tags, the cancel tag.
    if first == BLACK_FLAG && rest.chars().any(|c| is_tag(c) || c == CANCEL_TAG) {
        let tail: Vec<char> = rest.chars().collect();
        return match tail.split_last() {
            Some((&last, tags)) => {
                last == CANCEL_TAG && !tags.is_empty() && tags.iter().all(|&c| is_tag(c))
            },
            None => false,
        };
    }

    // A pictograph sequence.
    if !is_ext_pict(first) || is_skin_tone(first) {
        return false;
    }
    let mut it = rest.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            VS16 => {},
            c if is_skin_tone(c) => {},
            ZWJ => match it.next() {
                Some(next) if is_ext_pict(next) && !is_skin_tone(next) => {},
                _ => return false,
            },
            _ => return false,
        }
    }
    true
}

/// Whether `text` is exactly one emoji: one grapheme cluster that is an
/// emoji by the rule in the module docs. Leading or trailing white space is
/// not trimmed: `" 👍"` is not one emoji.
#[must_use]
pub fn is_single_emoji(text: &str) -> bool {
    let mut clusters = text.graphemes(true);
    match (clusters.next(), clusters.next()) {
        (Some(only), None) => is_emoji_cluster(only),
        _ => false,
    }
}

/// How many emoji `text` holds when it is one to [`EMOJI_ONLY_MAX`] emoji and
/// nothing else, white space allowed between and around them; `0` otherwise
/// (no emoji, more than three, or any other character). A message the
/// caller draws large has this in 1..=3 and also no attachment, reply quote
/// or forward line (UX section 13), which the caller checks.
///
/// Stops at the fourth emoji or the first other cluster, so a long text costs
/// no more than its leading white space.
#[must_use]
pub fn emoji_only_count(text: &str) -> u8 {
    let mut count: u8 = 0;
    for cluster in text.graphemes(true) {
        if cluster.chars().all(char::is_whitespace) {
            continue;
        }
        if !is_emoji_cluster(cluster) {
            return 0;
        }
        count += 1;
        if count > EMOJI_ONLY_MAX {
            return 0;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_emoji_of_every_shape() {
        for e in [
            "\u{1F44D}",                                                       // thumbs up
            "\u{2764}\u{FE0F}",                            // red heart, qualified
            "\u{2764}",                                    // red heart, unqualified
            "\u{1F44D}\u{1F3FD}",                          // thumbs up, skin tone
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // family ZWJ
            "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}",          // rainbow flag
            "\u{1F1EB}\u{1F1EE}",                          // flag of Finland
            "1\u{FE0F}\u{20E3}",                           // keycap one
            "#\u{20E3}",                                   // keycap number sign
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}", // Scotland
            "\u{00A9}",                                    // copyright sign
            "\u{1FAE8}",                                   // shaking face
            "\u{1FAFA}",                                   // a reserved pictograph code point
        ] {
            assert!(is_single_emoji(e), "{e:?}");
            assert_eq!(emoji_only_count(e), 1, "{e:?}");
        }
    }

    #[test]
    fn not_a_single_emoji() {
        for t in [
            "",
            " ",
            "a",
            "1",
            "#",
            "\u{1F44D}\u{1F44D}",          // two
            " \u{1F44D}",                  // leading space
            "\u{1F44D} ",                  // trailing space
            "\u{1F1EB}",                   // a lone regional indicator
            "\u{1F3FB}",                   // a lone skin tone
            "\u{2764}\u{FE0E}",            // text presentation asked for
            "\u{1F600}\u{0301}",           // a combining mark on an emoji
            "\u{1F600}\u{200D}",           // a dangling joiner
            "\u{1F600}\u{200D}a",          // a joiner to a letter
            "1\u{FE0F}",                   // a digit without the keycap
            "\u{1F3F4}\u{E0067}\u{E0062}", // tags without the cancel tag
            "\u{1F3F4}\u{E007F}",          // a cancel tag and no tags
            "\u{05D0}",                    // a Hebrew letter
            "\u{200D}",
            "\u{FE0F}",
        ] {
            assert!(!is_single_emoji(t), "{t:?}");
        }
    }

    #[test]
    fn emoji_only_counts_one_to_three_and_nothing_else() {
        assert_eq!(emoji_only_count("\u{1F44D}\u{1F44D}"), 2);
        assert_eq!(
            emoji_only_count(" \u{1F44D} \u{2764}\u{FE0F}\n\u{1F602} "),
            3
        );
        assert_eq!(emoji_only_count("\u{1F44D}\u{1F44D}\u{1F44D}\u{1F44D}"), 0);
        assert_eq!(emoji_only_count("\u{1F44D} ok"), 0);
        assert_eq!(emoji_only_count("ok"), 0);
        assert_eq!(emoji_only_count(""), 0);
        assert_eq!(emoji_only_count("   "), 0);
        assert_eq!(
            emoji_only_count("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{1F1EB}\u{1F1EE}"),
            2
        );
    }
}
