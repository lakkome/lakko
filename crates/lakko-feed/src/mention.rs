// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Whether a message mentions the person (ADR-0058 section 2).
//!
//! No envelope carries a mention, so the device derives one: `@` followed by
//! the person's canonical username, with no word character before the `@`
//! (so `bob@alice.com` is not a mention) and no username character after it
//! (so `@alice` does not match `@alicex`). A display name never counts:
//! names are not unique and would mark strangers' messages.

use lakko_contacts::check_canonical_username;

fn is_username_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'
}

/// Whether `text` mentions `username`. `username` must be canonical
/// (`[a-z0-9_]`, 3-32); anything else matches nothing.
#[must_use]
pub fn mentions_username(text: &str, username: &str) -> bool {
    if check_canonical_username(username).is_err() {
        return false;
    }
    let lowered = text.to_lowercase();
    let needle = format!("@{username}");
    let mut from = 0;
    while let Some(off) = lowered[from..].find(&needle) {
        let start = from + off;
        let end = start + needle.len();
        let before_ok = lowered[..start]
            .chars()
            .next_back()
            .map_or(true, |ch| !(ch.is_alphanumeric() || ch == '_'));
        let after_ok = lowered[end..]
            .chars()
            .next()
            .map_or(true, |ch| !is_username_char(ch));
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_whole_mention_in_any_case() {
        assert!(mentions_username("hi @alice!", "alice"));
        assert!(mentions_username("@Alice, look", "alice"));
        assert!(mentions_username("(@alice)", "alice"));
    }

    #[test]
    fn refuses_partial_and_email_like_matches() {
        assert!(!mentions_username("hi @alicex", "alice"));
        assert!(!mentions_username("bob@alice.com", "alice"));
        assert!(!mentions_username("alice", "alice"));
        assert!(
            !mentions_username("@al", "al"),
            "a non-canonical username matches nothing"
        );
    }
}
