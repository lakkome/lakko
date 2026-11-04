// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The letters drawn in an avatar that has no photo.
//!
//! The rule (`.claude/skills/ux-ui/design-system.md`, Avatar, decided
//! 2026-11-04):
//!
//! * a name of two or more words gives the first letter of each of the
//!   first two words (`QA Phase A` -> `QP`);
//! * a name of one word gives its first two letters (`VG3` -> `VG`,
//!   `Анна` -> `АН`);
//! * the result is upper case.
//!
//! What this module decides on top of that, so the three clients cannot:
//!
//! * A **letter** is an extended grapheme cluster, so an emoji, a flag or
//!   a letter with combining marks counts as one.
//! * **Words** are separated by Unicode white space; leading, trailing and
//!   repeated white space is ignored.
//! * A cluster **counts** when it holds a letter or a digit
//!   (`char::is_alphanumeric`, which is Unicode-aware) or is an emoji (the
//!   rule of [`crate::emoji`]). Every other cluster (punctuation, other
//!   symbols, a lone combining mark) is skipped, so `(Anna)` gives `AN`
//!   and `O'Brien` gives `OB`. A word with no counting cluster, such as
//!   `-` or `...`, is skipped as a word, so `Anna - Bob` gives `AB`.
//!   Digits count: `3D Studio` gives `3S`.
//! * **Upper case** is Unicode's default mapping (no locale). A cluster
//!   whose upper-case form is not one cluster (`ß` -> `SS`) is kept as it
//!   is, so the result never holds more than two clusters.
//! * A name with nothing that counts (empty, blank, punctuation only)
//!   gives the empty string, and the client draws its placeholder.

use unicode_segmentation::UnicodeSegmentation;

use crate::emoji::is_emoji_cluster;

/// The avatar initials of `name`, by the rule in the module docs: at most
/// two grapheme clusters, upper case, empty when nothing in the name counts.
#[must_use]
pub fn avatar_initials(name: &str) -> String {
    let mut words = name
        .split_whitespace()
        .map(counting_clusters)
        .filter(|clusters| !clusters.is_empty());
    let Some(first) = words.next() else {
        return String::new();
    };
    let picked: Vec<&str> = match words.next() {
        Some(second) => vec![first[0], second[0]],
        None => first.into_iter().take(2).collect(),
    };
    picked.into_iter().map(upper_cluster).collect()
}

/// The clusters of one word that count as letters, in order.
fn counting_clusters(word: &str) -> Vec<&str> {
    word.graphemes(true).filter(|c| counts(c)).collect()
}

/// Whether one cluster counts as a letter of a name.
fn counts(cluster: &str) -> bool {
    cluster.chars().any(char::is_alphanumeric) || is_emoji_cluster(cluster)
}

/// The upper-case form of one cluster, or the cluster itself when its
/// upper-case form is not a single cluster.
fn upper_cluster(cluster: &str) -> String {
    let upper = cluster.to_uppercase();
    if upper.graphemes(true).count() == 1 {
        upper
    } else {
        cluster.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_decided_examples() {
        assert_eq!(avatar_initials("QA Phase A"), "QP");
        assert_eq!(avatar_initials("VG3"), "VG");
        assert_eq!(avatar_initials("Анна"), "АН");
    }

    #[test]
    fn two_words_take_one_letter_each() {
        assert_eq!(avatar_initials("valentin goncharov"), "VG");
        assert_eq!(avatar_initials("Анна Петрова"), "АП");
        assert_eq!(avatar_initials("Mary Ann Smith"), "MA");
    }

    #[test]
    fn white_space_is_ignored_wherever_it_is() {
        assert_eq!(avatar_initials("  anna   bob  "), "AB");
        assert_eq!(avatar_initials("\tanna\n"), "AN");
        assert_eq!(avatar_initials("anna\u{3000}bob"), "AB");
    }

    #[test]
    fn empty_and_blank_give_nothing() {
        assert_eq!(avatar_initials(""), "");
        assert_eq!(avatar_initials("   \t\n"), "");
        assert_eq!(avatar_initials("..."), "");
        assert_eq!(avatar_initials("- ! ?"), "");
    }

    #[test]
    fn a_single_letter_name_gives_one_letter() {
        assert_eq!(avatar_initials("a"), "A");
        assert_eq!(avatar_initials("Я"), "Я");
    }

    #[test]
    fn punctuation_is_skipped_inside_a_word_and_as_a_word() {
        assert_eq!(avatar_initials("(Anna)"), "AN");
        assert_eq!(avatar_initials("O'Brien"), "OB");
        assert_eq!(avatar_initials("Anna - Bob"), "AB");
        assert_eq!(avatar_initials("@anna"), "AN");
    }

    #[test]
    fn digits_count_as_letters() {
        assert_eq!(avatar_initials("3D Studio"), "3S");
        assert_eq!(avatar_initials("42"), "42");
        assert_eq!(avatar_initials("Team 7"), "T7");
    }

    #[test]
    fn an_emoji_counts_as_one_letter() {
        assert_eq!(avatar_initials("🌸 Anna"), "🌸A");
        assert_eq!(avatar_initials("🌸🌼"), "🌸🌼");
        // A family is one cluster of several code points.
        assert_eq!(avatar_initials("👨‍👩‍👧 home"), "👨‍👩‍👧H");
        assert_eq!(avatar_initials("🇫🇮"), "🇫🇮");
    }

    #[test]
    fn a_combined_letter_counts_as_one() {
        // "e" followed by COMBINING ACUTE ACCENT.
        assert_eq!(avatar_initials("e\u{301}mile"), "E\u{301}M");
        assert_eq!(avatar_initials("ébène"), "ÉB");
    }

    #[test]
    fn upper_case_never_grows_the_result() {
        // "ß" upper-cases to "SS", two clusters: kept as it is.
        assert_eq!(avatar_initials("ßa"), "ßA");
        assert_eq!(avatar_initials("straße weg"), "SW");
    }

    #[test]
    fn scripts_without_case_pass_through() {
        assert_eq!(avatar_initials("李小龙"), "李小");
        assert_eq!(avatar_initials("محمد علي"), "مع");
    }

    proptest! {
        #[test]
        fn never_more_than_two_clusters_and_never_panics(name in ".{0,64}") {
            let initials = avatar_initials(&name);
            prop_assert!(initials.graphemes(true).count() <= 2);
            prop_assert_eq!(initials.is_empty(), !name.split_whitespace().any(|w| !counting_clusters(w).is_empty()));
        }
    }
}
