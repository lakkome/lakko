// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which parts of a message's text are links (`docs/ux/message-links.md`).
//!
//! The text comes back as pieces, each with or without a link, rather than as
//! offsets: the clients count characters three different ways (UTF-16 on web
//! and Android and on iOS through `NSString`, grapheme clusters in Swift), and
//! pieces leave nothing to convert. Joining every piece gives the input back.
//!
//! This decides what is drawn as a link, nothing more. It says nothing about
//! whether a link is safe, and Lakko never fetches one.

use lakko_contacts::is_format_char;

/// The longest link, in bytes. A longer one stays text.
pub const MAX_LINK_BYTES: usize = 2048;

/// One run of a message's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageTextPiece {
    /// The text as sent.
    pub text: String,
    /// `true` when this run is a link to open. The address is the text.
    pub is_link: bool,
}

/// Length of a scheme this rule takes at the start of `rest`, if any.
fn scheme_len(rest: &str) -> Option<usize> {
    for scheme in ["https://", "http://"] {
        if let Some(head) = rest.get(..scheme.len()) {
            if head.eq_ignore_ascii_case(scheme) {
                return Some(scheme.len());
            }
        }
    }
    None
}

/// Whether `c` ends a link.
fn ends_link(c: char) -> bool {
    c.is_whitespace() || c.is_control() || is_format_char(c) || matches!(c, '<' | '>' | '"' | '`')
}

fn counts_of(body: &str, open: char, close: char) -> (usize, usize) {
    (body.matches(open).count(), body.matches(close).count())
}

/// `body` without what belongs to the sentence around it: punctuation at the
/// end, and a closing bracket the link did not open.
fn without_trailing_sentence(body: &str) -> &str {
    let mut end = body.len();
    loop {
        let trimmed = &body[..end];
        let Some(last) = trimmed.chars().next_back() else {
            return trimmed;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' | '~' | '\u{2019}'
            | '\u{201d}' | '\u{00bb}' | '\u{2026}' => true,
            ')' | ']' | '}' => {
                let open = match last {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let (opened, closed) = counts_of(trimmed, open, last);
                closed > opened
            },
            _ => false,
        };
        if !drop {
            return trimmed;
        }
        end -= last.len_utf8();
    }
}

/// Whether the host part after the scheme can be drawn as a link: not empty,
/// and naming no user. `https://lakko.me@evil.example/` opens `evil.example`
/// while reading as `lakko.me`.
fn authority_is_clean(after_scheme: &str) -> bool {
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    !authority.is_empty() && !authority.contains('@')
}

/// Whether a link may start at byte `at`: the start of the text, or after
/// something that is not a letter or a digit.
fn may_start_at(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .map_or(true, |c| !c.is_alphanumeric())
}

/// The text of a message as pieces, in order. Joining every piece's text gives
/// `text` back.
#[must_use]
pub fn split_message_links(text: &str) -> Vec<MessageTextPiece> {
    let mut pieces = Vec::new();
    let mut plain_from = 0;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let Some(scheme) = scheme_len(rest).filter(|_| may_start_at(text, at)) else {
            at += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let raw_end = rest.find(ends_link).unwrap_or(rest.len());
        let body = without_trailing_sentence(&rest[..raw_end]);
        if body.len() > scheme
            && body.len() <= MAX_LINK_BYTES
            && authority_is_clean(&body[scheme..])
        {
            if plain_from < at {
                pieces.push(MessageTextPiece {
                    text: text[plain_from..at].to_owned(),
                    is_link: false,
                });
            }
            pieces.push(MessageTextPiece {
                text: body.to_owned(),
                is_link: true,
            });
            at += body.len();
            plain_from = at;
        } else {
            // Not a link. Skip what was read, so no second link starts inside
            // the same address.
            at += raw_end.max(scheme);
        }
    }
    if plain_from < text.len() {
        pieces.push(MessageTextPiece {
            text: text[plain_from..].to_owned(),
            is_link: false,
        });
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn links(text: &str) -> Vec<String> {
        split_message_links(text)
            .into_iter()
            .filter(|p| p.is_link)
            .map(|p| p.text)
            .collect()
    }

    #[test]
    fn plain_text_is_one_piece_and_empty_text_is_none() {
        assert_eq!(
            split_message_links("hello there"),
            vec![MessageTextPiece {
                text: "hello there".to_owned(),
                is_link: false
            }]
        );
        assert!(split_message_links("").is_empty());
    }

    #[test]
    fn a_link_in_a_sentence_keeps_the_text_around_it() {
        let pieces = split_message_links("join https://lakko.me/i/abcdefgh#secret now");
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].text, "join ");
        assert!(!pieces[0].is_link);
        assert_eq!(pieces[1].text, "https://lakko.me/i/abcdefgh#secret");
        assert!(pieces[1].is_link);
        assert_eq!(pieces[2].text, " now");
    }

    #[test]
    fn a_message_that_is_only_a_link_is_one_link() {
        let pieces = split_message_links("https://lakko.me/g/abcdefgh#0123456789abcdef");
        assert_eq!(pieces.len(), 1);
        assert!(pieces[0].is_link);
    }

    #[test]
    fn the_scheme_is_matched_in_any_case_and_only_http_counts() {
        assert_eq!(links("HTTPS://Lakko.me/x"), vec!["HTTPS://Lakko.me/x"]);
        assert_eq!(links("http://a.example"), vec!["http://a.example"]);
        assert!(links("ftp://a.example javascript://x lakko://i/abc").is_empty());
    }

    #[test]
    fn a_bare_domain_is_text() {
        assert!(links("see lakko.me or v1.2.3").is_empty());
    }

    #[test]
    fn a_scheme_inside_a_word_starts_nothing() {
        assert!(links("xhttps://a.example").is_empty());
        assert_eq!(links("(https://a.example)"), vec!["https://a.example"]);
    }

    #[test]
    fn sentence_punctuation_stays_out_of_the_link() {
        assert_eq!(
            links("go to https://a.example/b."),
            vec!["https://a.example/b"]
        );
        assert_eq!(
            links("https://a.example/b, then"),
            vec!["https://a.example/b"]
        );
        assert_eq!(
            links("really https://a.example/b?!"),
            vec!["https://a.example/b"]
        );
        assert_eq!(
            links("(see https://a.example/b)."),
            vec!["https://a.example/b"]
        );
    }

    #[test]
    fn a_bracket_the_link_opened_stays_in_it() {
        assert_eq!(
            links("https://a.example/wiki/Foo_(bar)"),
            vec!["https://a.example/wiki/Foo_(bar)"]
        );
    }

    #[test]
    fn a_query_and_a_fragment_stay_in_the_link() {
        assert_eq!(
            links("https://a.example/p?q=1&r=2#top"),
            vec!["https://a.example/p?q=1&r=2#top"]
        );
    }

    #[test]
    fn a_user_before_the_host_stays_text() {
        assert!(links("https://lakko.me@evil.example/").is_empty());
        assert!(links("https://user:pass@a.example").is_empty());
        assert_eq!(
            links("https://a.example/p@q"),
            vec!["https://a.example/p@q"]
        );
    }

    #[test]
    fn a_link_with_no_host_stays_text() {
        assert!(links("https:// and http:///path").is_empty());
    }

    #[test]
    fn an_invisible_character_ends_a_link() {
        assert_eq!(
            links("https://a.example/\u{202e}evil"),
            vec!["https://a.example/"]
        );
        assert_eq!(
            links("https://a.example\u{200b}.evil"),
            vec!["https://a.example"]
        );
    }

    #[test]
    fn a_link_over_the_limit_stays_text() {
        let long = format!("https://a.example/{}", "x".repeat(MAX_LINK_BYTES));
        assert!(links(&long).is_empty());
        let exact = format!(
            "https://a.example/{}",
            "x".repeat(MAX_LINK_BYTES - "https://a.example/".len())
        );
        assert_eq!(links(&exact), vec![exact.clone()]);
    }

    #[test]
    fn two_links_are_two_pieces() {
        assert_eq!(
            links("https://a.example and https://b.example"),
            vec!["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn unicode_around_a_link_is_kept() {
        let text = "\u{0441}\u{043c}. https://a.example/\u{043f}\u{0443}\u{0442}\u{044c} \u{1F600}";
        let joined: String = split_message_links(text)
            .into_iter()
            .map(|p| p.text)
            .collect();
        assert_eq!(joined, text);
        assert_eq!(links(text).len(), 1);
    }

    proptest! {
        #[test]
        fn pieces_join_back_to_the_input(text in "\\PC{0,200}") {
            let joined: String = split_message_links(&text)
                .into_iter()
                .map(|p| p.text)
                .collect();
            prop_assert_eq!(joined, text);
        }

        #[test]
        fn pieces_join_back_on_text_built_around_links(
            head in "[ a-z(.]{0,8}",
            path in "[a-z0-9/._()@-]{0,20}",
            tail in "[ .,)!?]{0,8}",
        ) {
            let text = format!("{head}https://a.example/{path}{tail}");
            let joined: String = split_message_links(&text)
                .into_iter()
                .map(|p| p.text)
                .collect();
            prop_assert_eq!(joined, text);
        }

        #[test]
        fn every_link_is_a_clean_http_address(text in "(https?://|[ a-z@/.()]){0,40}") {
            for piece in split_message_links(&text).iter().filter(|p| p.is_link) {
                let scheme = scheme_len(&piece.text).expect("a link starts with a scheme");
                prop_assert!(authority_is_clean(&piece.text[scheme..]));
                prop_assert!(!piece.text.contains(char::is_whitespace));
                prop_assert!(piece.text.len() <= MAX_LINK_BYTES);
            }
        }
    }
}
