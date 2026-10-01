// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Display-name normalisation and word-prefix search terms ([ADR-0060]
//! section 5, UX spec `docs/ux/who-can-find-me.md` section 6.1).
//!
//! A name is found by the beginnings of its words, ignoring letter case,
//! repeated spaces and accents: `ann` and `anna web` find `Anna Wéber`,
//! `nna` does not. Every search path - the server's public name index (OPRF
//! outputs of the terms below, never plaintext), the contacts-of-contacts
//! introduction tags, and the local in-group match - must compute exactly
//! these forms, so they are defined once, here.
//!
//! ## Why it lives here and not in `lakko-contacts`
//!
//! ADR-0060 section 5 places the normaliser in `lakko-contacts`. It needs the
//! Unicode decomposition and mark tables of `unicode-normalization`, and
//! `lakko-contacts` is linked by the size-budgeted iOS notification service
//! extension (`lakko-ffi-extension`), which must not grow those tables. This
//! crate already carries that dependency for username normalisation, and it
//! is where the name OPRF input is built
//! ([`crate::OprfBlinder::blind_name_term`]), so the normaliser sits beside
//! its one consumer on the network path. Both FFI crates link this crate.
//!
//! ## Normalisation, in order
//!
//! 0. Refuse input longer than [`NAME_INPUT_MAX_BYTES`] bytes before any
//!    work (a denial-of-service bound, not a name rule).
//! 1. NFKD: compatibility decomposition, so `é` becomes `e` + U+0301 and
//!    fullwidth or ligature forms become their plain letters.
//! 2. Unicode lowercase ([`str::to_lowercase`], the `Lowercase_Mapping`
//!    including the Greek final-sigma rule). **This is not full case
//!    folding**: `Straße` and `STRASSE` stay different (`straße` vs
//!    `strasse`). A second NFKD follows, since a lowercase mapping may yield
//!    a character that decomposes.
//! 3. Drop every character of General Category Mark (Mn, Mc and Me - the
//!    set `unicode_normalization::char::is_combining_mark` covers, which is
//!    the only category test that crate exposes). Dropping Mc too, not only
//!    the non-spacing Mn, also removes spacing vowel signs in scripts such as
//!    Devanagari, so two names there that differ only in such a sign match.
//!    That widens a match; it never narrows one.
//! 4. Collapse every run of Unicode whitespace to one ASCII space, and trim.
//! 5. Refuse (`None`) a result shorter than [`NAME_SEARCH_MIN_CHARS`]
//!    Unicode scalar values, the search minimum of the UX spec. There is no
//!    upper bound on the result: a valid display name may expand under NFKD
//!    and must stay searchable.
//!
//! ## Terms
//!
//! A published name is indexed under [`name_search_terms`]: for each word,
//! the word itself when it is shorter than [`NAME_TERM_MIN_CHARS`], otherwise
//! every prefix of [`NAME_TERM_MIN_CHARS`] to [`NAME_TERM_MAX_CHARS`]
//! characters; at most [`MAX_NAME_TERMS`] in all. A query looks up exactly
//! one term, [`name_query_term`] (its longest word, cut to
//! [`NAME_TERM_MAX_CHARS`]), and the candidates that come back are filtered
//! locally with [`name_matches_query`], which checks every query word.
//!
//! [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// The shortest normalised name a search accepts, in Unicode scalar values.
pub const NAME_SEARCH_MIN_CHARS: usize = 2;
/// The longest input, in bytes, normalisation looks at; longer input is
/// refused unread. Chosen as a denial-of-service bound, not measured; a
/// display name is at most 255 bytes (ADR-0048).
pub const NAME_INPUT_MAX_BYTES: usize = 1024;
/// The shortest word prefix published as a term. A word shorter than this is
/// published whole. Chosen, not measured.
pub const NAME_TERM_MIN_CHARS: usize = 3;
/// The longest word prefix published as a term, and the length a query term
/// is cut to. Chosen, not measured.
pub const NAME_TERM_MAX_CHARS: usize = 16;
/// The most terms one name publishes; the terms of earlier words come first.
/// Chosen, not measured.
pub const MAX_NAME_TERMS: usize = 64;

/// NFKD, lowercase, NFKD, drop marks, collapse whitespace, trim - steps 1-4
/// of the module doc, with no length rule.
fn normalize_core(input: &str) -> String {
    let lowered = input.nfkd().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for c in lowered.nfkd().filter(|c| !is_combining_mark(*c)) {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// The normalised form a display name or a query is searched under, or
/// `None` when the input exceeds [`NAME_INPUT_MAX_BYTES`] or the result is
/// shorter than [`NAME_SEARCH_MIN_CHARS`].
///
/// Idempotent: normalising a normalised name returns it unchanged. Words in
/// the result are separated by exactly one ASCII space.
#[must_use]
pub fn normalize_name_for_search(input: &str) -> Option<String> {
    if input.len() > NAME_INPUT_MAX_BYTES {
        return None;
    }
    let out = normalize_core(input);
    if out.chars().count() < NAME_SEARCH_MIN_CHARS {
        return None;
    }
    Some(out)
}

/// The first `n` characters of `word`.
fn char_prefix(word: &str, n: usize) -> &str {
    word.char_indices()
        .nth(n)
        .map_or(word, |(at, _)| word.get(..at).unwrap_or(word))
}

/// The terms a display name is published under ([`MAX_NAME_TERMS`] at most,
/// first occurrence kept, earlier words first). Empty when the name is not
/// searchable.
///
/// These are what a person publishes when they choose Everyone, and what a
/// contacts-of-contacts introduction is sealed under.
#[must_use]
pub fn name_search_terms(display_name: &str) -> Vec<String> {
    let Some(normalised) = normalize_name_for_search(display_name) else {
        return Vec::new();
    };
    let mut terms: Vec<String> = Vec::new();
    for word in normalised.split(' ') {
        let len = word.chars().count();
        let lengths = if len < NAME_TERM_MIN_CHARS {
            len..=len
        } else {
            NAME_TERM_MIN_CHARS..=len.min(NAME_TERM_MAX_CHARS)
        };
        for n in lengths {
            if terms.len() == MAX_NAME_TERMS {
                return terms;
            }
            let term = char_prefix(word, n);
            if !term.is_empty() && !terms.iter().any(|t| t == term) {
                terms.push(term.to_owned());
            }
        }
    }
    terms
}

/// The one term a query looks up: the longest word of the normalised query
/// (the first on a tie), cut to [`NAME_TERM_MAX_CHARS`] characters. `None`
/// when the query is not searchable.
///
/// A word of one or two characters is allowed: it matches a name word of
/// exactly that length, which [`name_search_terms`] publishes whole.
#[must_use]
pub fn name_query_term(query: &str) -> Option<String> {
    let normalised = normalize_name_for_search(query)?;
    let mut best: Option<(&str, usize)> = None;
    for word in normalised.split(' ') {
        let len = word.chars().count();
        if best.map_or(true, |(_, best_len)| len > best_len) {
            best = Some((word, len));
        }
    }
    best.map(|(word, _)| char_prefix(word, NAME_TERM_MAX_CHARS).to_owned())
}

/// Whether `display_name` matches `query`: every word of the normalised
/// query is a prefix of some word of the normalised name. A query word
/// longer than [`NAME_TERM_MAX_CHARS`] is compared in full.
///
/// The local filter a client runs over the candidates a term lookup
/// returns, and the whole of a purely local search. `false` when either side
/// is not searchable.
#[must_use]
pub fn name_matches_query(display_name: &str, query: &str) -> bool {
    let (Some(name), Some(query)) = (
        normalize_name_for_search(display_name),
        normalize_name_for_search(query),
    ) else {
        return false;
    };
    query
        .split(' ')
        .all(|q| name.split(' ').any(|w| w.starts_with(q)))
}

/// Whether `term` is a well-formed name term.
///
/// 1 to [`NAME_TERM_MAX_CHARS`] characters, no whitespace, and already in
/// normalised form - so a
/// publisher and a querier who both derived it from this module produce the
/// same bytes.
#[must_use]
pub fn is_name_term(term: &str) -> bool {
    let len = term.chars().count();
    (1..=NAME_TERM_MAX_CHARS).contains(&len)
        && !term.chars().any(char::is_whitespace)
        && normalize_core(term) == term
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Option<String> {
        normalize_name_for_search(s)
    }

    #[test]
    fn case_spaces_and_accents_are_ignored() {
        let want = Some("anna weber".to_owned());
        assert_eq!(n("Anna Weber"), want);
        assert_eq!(n(" anna  WEBER "), want);
        assert_eq!(n("Anna  W\u{e9}ber"), want);
        assert_eq!(n("\tAnna\u{a0}\u{2003}Weber\n"), want);
    }

    #[test]
    fn cyrillic_lowercases_and_keeps_its_letters() {
        assert_eq!(
            n("\u{410}\u{43d}\u{43d}\u{430} \u{412}\u{435}\u{431}\u{435}\u{440}"),
            Some("\u{430}\u{43d}\u{43d}\u{430} \u{432}\u{435}\u{431}\u{435}\u{440}".to_owned())
        );
        // Й (U+0419) decomposes to И + U+0306 and loses the breve.
        assert_eq!(n("\u{419}\u{43e}"), Some("\u{438}\u{43e}".to_owned()));
    }

    #[test]
    fn combining_and_precomposed_forms_are_equal() {
        assert_eq!(n("Jos\u{e9}"), n("Jose\u{301}"));
        assert_eq!(n("Jos\u{e9}"), Some("jose".to_owned()));
        assert_eq!(n("\u{c5}sa"), n("A\u{30a}sa"));
    }

    #[test]
    fn compatibility_forms_fold() {
        assert_eq!(
            n("\u{ff21}\u{ff4e}\u{ff4e}\u{ff41}"),
            Some("anna".to_owned())
        );
        assert_eq!(n("\u{fb01}ona"), Some("fiona".to_owned()));
        // The dotted capital I loses its dot rather than gaining a mark.
        assert_eq!(n("\u{130}pek"), Some("ipek".to_owned()));
    }

    #[test]
    fn case_folding_is_lowercase_only() {
        assert_eq!(n("Stra\u{df}e"), Some("stra\u{df}e".to_owned()));
        assert_ne!(n("Stra\u{df}e"), n("STRASSE"));
    }

    #[test]
    fn length_bounds() {
        assert_eq!(n("a"), None);
        assert_eq!(n("  a  "), None);
        assert_eq!(n(""), None);
        assert_eq!(n("\u{301}\u{301}"), None);
        assert_eq!(n("ab"), Some("ab".to_owned()));
        // No upper bound on the result: a name that expands under NFKD stays
        // searchable.
        assert_eq!(n(&"a".repeat(65)), Some("a".repeat(65)));
        assert_eq!(n(&"\u{fb01}".repeat(33)), Some("fi".repeat(33)));
        // The input bound is in bytes, checked before normalising.
        assert_eq!(
            n(&"a".repeat(NAME_INPUT_MAX_BYTES)).map(|s| s.len()),
            Some(1024)
        );
        assert_eq!(n(&"a".repeat(NAME_INPUT_MAX_BYTES + 1)), None);
        // 342 three-byte characters are 1026 bytes: refused although short
        // in characters.
        assert_eq!(n(&"\u{fb01}".repeat(342)), None);
    }

    #[test]
    fn idempotent() {
        for s in [
            "Anna  W\u{e9}ber",
            "\u{410}\u{43d}\u{43d}\u{430}",
            "\u{130}pek \u{fb01}",
            "O\u{3a3}\u{391}\u{3a3}",
        ] {
            let once = n(s).expect("searchable");
            assert_eq!(n(&once), Some(once.clone()), "{s:?}");
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn terms_are_word_prefixes() {
        let terms = name_search_terms("Anna W\u{e9}ber");
        assert_eq!(terms, strings(&["ann", "anna", "web", "webe", "weber"]));
        assert!(!terms.iter().any(|t| t == "an"));
        assert_eq!(name_search_terms("ANNA weber"), terms);
    }

    #[test]
    fn short_words_are_published_whole() {
        // Ли Вэй: "ли" is 2 characters, "вэи" (й loses its breve) 3.
        let terms = name_search_terms("\u{41b}\u{438} \u{412}\u{44d}\u{439}");
        assert_eq!(terms, strings(&["\u{43b}\u{438}", "\u{432}\u{44d}\u{438}"]));
        assert_eq!(
            name_search_terms("J Smith"),
            strings(&["j", "smi", "smit", "smith"])
        );
    }

    #[test]
    fn unsearchable_names_have_no_terms() {
        assert!(name_search_terms("a").is_empty());
        assert!(name_search_terms("").is_empty());
        assert!(name_search_terms(&"a".repeat(NAME_INPUT_MAX_BYTES + 1)).is_empty());
    }

    #[test]
    fn a_long_word_yields_prefixes_up_to_the_maximum() {
        let word = "abcdefghijklmnopqrstuvwxyzabcd";
        assert_eq!(word.len(), 30);
        let terms = name_search_terms(word);
        assert_eq!(terms.len(), NAME_TERM_MAX_CHARS - NAME_TERM_MIN_CHARS + 1);
        assert_eq!(terms.first().map(String::as_str), Some("abc"));
        assert_eq!(terms.last().map(String::as_str), Some("abcdefghijklmnop"));
        // The full 30-character word as a query: cut to 16, found, and the
        // local filter compares it in full.
        let q = name_query_term(word).expect("term");
        assert_eq!(q, "abcdefghijklmnop");
        assert!(terms.contains(&q));
        assert!(name_matches_query(word, word));
        assert!(!name_matches_query(word, "abcdefghijklmnopX"));
    }

    #[test]
    fn terms_are_deduped_and_capped() {
        assert_eq!(
            name_search_terms("Anna anna ANNA"),
            strings(&["ann", "anna"])
        );
        assert_eq!(name_search_terms("Ann Anna"), strings(&["ann", "anna"]));
        // Five 16+-letter words give 14 terms each: 70, capped at 64, the
        // earlier words' terms first.
        let words = [
            "aaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbb",
            "ccccccccccccccccc",
            "ddddddddddddddddd",
            "eeeeeeeeeeeeeeeee",
        ];
        let terms = name_search_terms(&words.join(" "));
        assert_eq!(terms.len(), MAX_NAME_TERMS);
        assert_eq!(terms.first().map(String::as_str), Some("aaa"));
        assert!(terms.iter().any(|t| t == "dddddddddddddddd"));
        assert_eq!(terms.last().map(String::as_str), Some("eeeeeeeeee"));
        assert!(!terms.iter().any(|t| t == "eeeeeeeeeeeeeeee"));
        assert!(terms.iter().any(|t| t == "eeeeeeeeee"));
    }

    #[test]
    fn the_query_term_is_the_longest_word() {
        assert_eq!(name_query_term("anna web"), Some("anna".to_owned()));
        assert_eq!(name_query_term("an weber"), Some("weber".to_owned()));
        assert_eq!(name_query_term("ann web"), Some("ann".to_owned()));
        assert_eq!(
            name_query_term("\u{41b}\u{438}"),
            Some("\u{43b}\u{438}".to_owned())
        );
        assert_eq!(name_query_term("A"), None);
    }

    #[test]
    fn matching_is_by_word_prefix() {
        let name = "Anna W\u{e9}ber";
        assert!(name_matches_query(name, "ann"));
        assert!(name_matches_query(name, "ANN"));
        assert!(!name_matches_query(name, "nna"));
        assert!(name_matches_query(name, "anna web"));
        assert!(name_matches_query(name, "weber  ANNA"));
        assert!(name_matches_query(name, "W\u{c9}B"));
        assert!(!name_matches_query(name, "anna x"));
        assert!(!name_matches_query(name, "a"));
        // A query term is always among the published terms of a name the
        // query matches (for query words of 3 characters or more).
        for q in ["ann", "anna web", "webe", "W\u{c9}BER anna"] {
            let t = name_query_term(q).expect("term");
            assert!(name_search_terms(name).contains(&t), "{q:?}");
        }
        assert!(!name_search_terms(name).contains(&name_query_term("nna").expect("term")));
    }

    #[test]
    fn terms_are_well_formed() {
        for t in name_search_terms("Anna W\u{e9}ber \u{41b}\u{438} J") {
            assert!(is_name_term(&t), "{t:?}");
        }
        assert!(!is_name_term(""));
        assert!(!is_name_term("anna weber"));
        assert!(!is_name_term("Anna"));
        assert!(!is_name_term("w\u{e9}ber"));
        assert!(!is_name_term(&"a".repeat(17)));
        assert!(is_name_term(&"a".repeat(16)));
    }
}
