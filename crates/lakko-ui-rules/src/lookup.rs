// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When a typed name is worth asking the network about.
//!
//! `docs/ux/navigation.md` section 4.3. There is ONE network lookup, not one
//! per kind: `@username` and a channel's `@handle` are the same mechanism
//! over two namespaces, and a group handle would be a third and the same
//! again. The person typing does not know which namespace the name they were
//! given lives in.
//!
//! What is here is only the gate in front of it. The lookup itself is per
//! client, because it is network I/O; deciding whether a string could be a
//! name is a rule, and a rule written three times is three chances to differ.

use lakko_contacts::username::check_canonical_username;

/// Strip the leading `@` people type and no namespace stores.
///
/// Trims first, so a name pasted with a space around it still works.
#[must_use]
pub fn bare_name(typed: &str) -> String {
    typed.trim().trim_start_matches('@').trim().to_string()
}

/// Whether this is worth spending a lookup on.
///
/// The predicate is [`check_canonical_username`] itself, not a copy of its
/// rule: the blinder refuses anything it refuses, so asking first is the
/// difference between a quiet "type the whole name" and making a person pay
/// a proof of work to be told no.
///
/// Note what this deliberately is NOT: a check that the name exists. It
/// cannot be. The server evaluates the blinded name without learning it, so
/// it answers "does this exact name exist" and nothing about prefixes - a
/// prefix search is the server learning the prefix. A client must offer this
/// as "look up the whole name", never as a list of partial matches.
#[must_use]
pub fn looks_like_a_name(typed: &str) -> bool {
    check_canonical_username(&bare_name(typed).to_lowercase()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_what_people_type_and_no_namespace_stores() {
        assert_eq!(bare_name("  @anna "), "anna");
        assert_eq!(bare_name("anna"), "anna");
        assert_eq!(bare_name("@ anna"), "anna");
    }

    #[test]
    fn accepts_a_canonical_name_with_or_without_the_at_sign() {
        assert!(looks_like_a_name("@lakko_news"));
        assert!(looks_like_a_name("anna"));
        assert!(looks_like_a_name("  @Anna  "));
    }

    #[test]
    fn refuses_what_the_blinder_would_refuse() {
        // Each of these costs a proof of work to be told no, if it is not
        // refused here first.
        assert!(!looks_like_a_name("ab"));
        assert!(!looks_like_a_name(&"a".repeat(33)));
        assert!(!looks_like_a_name("anna smith"));
        assert!(!looks_like_a_name("anna!"));
        assert!(!looks_like_a_name(""));
        assert!(!looks_like_a_name("@"));
    }

    #[test]
    fn agrees_with_the_rule_it_gates_rather_than_restating_it() {
        // The property that matters: this predicate and the one the blinder
        // enforces are the same predicate. If the canonical rule ever moves,
        // this moves with it instead of drifting behind.
        for candidate in ["anna", "a_b_c", "ab", "anna smith", "x".repeat(32).as_str()] {
            let bare = bare_name(candidate).to_lowercase();
            assert_eq!(
                looks_like_a_name(candidate),
                check_canonical_username(&bare).is_ok(),
                "disagreed about {candidate:?}"
            );
        }
    }
}
