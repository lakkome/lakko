// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The `@name` a new group or channel form proposes
//! (`docs/ux/group-and-channel-privacy.md` section 2.1, [ADR-0087]
//! section 1).
//!
//! Nobody should have to invent a unique name for a family chat, so the
//! form fills the field in:
//!
//! - From the name: ASCII letters lower-cased, ASCII digits kept, runs of
//!   white space become one `_`, everything else dropped; leading and
//!   trailing `_` trimmed; cut to [`SUGGESTED_BASE_MAX`] characters.
//! - **Private:** plus `_` and [`SUGGESTED_SUFFIX_LEN`] random characters:
//!   `family_k3x9`.
//! - **Public:** no suffix: `hiking_club`.
//! - No usable letters (a Cyrillic or Arabic name, or none): `group_k3x9`,
//!   `channel_k3x9`.
//! - A public name whose base is shorter than the `@name` minimum gets the
//!   suffix too, since the bare base could never be claimed.
//!
//! The random characters come from the caller's `random`, so the rule
//! stays a pure function every client computes alike. Every result passes
//! [`lakko_contacts::username::check_canonical_username`].
//!
//! The well-formedness check the form runs on a typed name is
//! [`crate::lookup::looks_like_a_name`], the same predicate.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_contacts::username::USERNAME_MIN_LEN;

/// Longest base taken from the name, so a private suffix keeps the result
/// within 32 characters.
pub const SUGGESTED_BASE_MAX: usize = 26;

/// Random characters a private suggestion ends with.
pub const SUGGESTED_SUFFIX_LEN: usize = 4;

const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// What the name is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    /// A group.
    Group,
    /// A channel.
    Channel,
}

impl HandleKind {
    const fn fallback(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Channel => "channel",
        }
    }
}

/// The base of a suggestion: the name reduced to `[a-z0-9_]`, without
/// leading, trailing or repeated `_`, at most [`SUGGESTED_BASE_MAX`].
fn base_of(name: &str) -> String {
    let mut out = String::with_capacity(SUGGESTED_BASE_MAX);
    let mut pending_underscore = false;
    for c in name.chars() {
        if out.len() >= SUGGESTED_BASE_MAX {
            break;
        }
        if c.is_whitespace() || c == '_' {
            pending_underscore = !out.is_empty();
            continue;
        }
        if !c.is_ascii_alphanumeric() {
            continue;
        }
        if pending_underscore {
            if out.len() + 1 >= SUGGESTED_BASE_MAX {
                break;
            }
            out.push('_');
            pending_underscore = false;
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn suffix(random: u64) -> String {
    let mut r = random;
    let mut out = String::with_capacity(SUGGESTED_SUFFIX_LEN);
    for _ in 0..SUGGESTED_SUFFIX_LEN {
        // `r % 36 < 36`, so the index is in range.
        let i = usize::try_from(r % 36).unwrap_or(0);
        out.push(char::from(ALPHABET[i]));
        r /= 36;
    }
    out
}

/// The `@name` a new group or channel form proposes for `name`.
///
/// `random` supplies the suffix characters; pass fresh randomness. The
/// result is a canonical `@name`, never longer than 32 characters.
#[must_use]
pub fn suggest_handle(name: &str, kind: HandleKind, private: bool, random: u64) -> String {
    let base = base_of(name);
    if base.is_empty() {
        return format!("{}_{}", kind.fallback(), suffix(random));
    }
    if private || base.len() < USERNAME_MIN_LEN {
        return format!("{base}_{}", suffix(random));
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::username::check_canonical_username;
    use proptest::prelude::*;

    #[test]
    fn the_spec_examples() {
        assert_eq!(
            suggest_handle("Hiking club", HandleKind::Group, false, 0),
            "hiking_club"
        );
        let private = suggest_handle("Family", HandleKind::Group, true, 12_345);
        assert!(private.starts_with("family_"));
        assert_eq!(private.len(), "family_".len() + SUGGESTED_SUFFIX_LEN);
        let cyrillic = suggest_handle("Семья", HandleKind::Group, true, 1);
        assert!(cyrillic.starts_with("group_"), "{cyrillic}");
        let none = suggest_handle("", HandleKind::Channel, false, 1);
        assert!(none.starts_with("channel_"), "{none}");
    }

    #[test]
    fn spaces_collapse_and_symbols_drop() {
        assert_eq!(
            suggest_handle("  Berlin  News!! 2026 ", HandleKind::Channel, false, 0),
            "berlin_news_2026"
        );
        assert_eq!(suggest_handle("a__b", HandleKind::Group, false, 0), "a_b");
        assert_eq!(
            suggest_handle("Café Müller", HandleKind::Group, false, 0),
            "caf_mller"
        );
    }

    #[test]
    fn the_base_is_cut_to_twenty_six() {
        let long = "word ".repeat(20);
        let s = suggest_handle(&long, HandleKind::Group, true, 7);
        assert!(s.len() <= 32);
        let base = s.rsplit_once('_').unwrap().0;
        assert!(base.len() <= SUGGESTED_BASE_MAX);
        assert!(!base.ends_with('_'));
    }

    #[test]
    fn a_short_public_name_gets_a_suffix() {
        let s = suggest_handle("Al", HandleKind::Group, false, 3);
        assert!(s.starts_with("al_"));
        assert_eq!(check_canonical_username(&s), Ok(()));
    }

    #[test]
    fn the_suffix_follows_the_randomness() {
        assert_ne!(
            suggest_handle("x", HandleKind::Group, true, 1),
            suggest_handle("x", HandleKind::Group, true, 2)
        );
        assert_eq!(
            suggest_handle("x", HandleKind::Group, true, 5),
            suggest_handle("x", HandleKind::Group, true, 5)
        );
    }

    proptest! {
        #[test]
        fn every_suggestion_is_a_canonical_name(
            name in ".{0,80}",
            private in any::<bool>(),
            channel in any::<bool>(),
            random in any::<u64>(),
        ) {
            let kind = if channel { HandleKind::Channel } else { HandleKind::Group };
            let s = suggest_handle(&name, kind, private, random);
            prop_assert_eq!(check_canonical_username(&s), Ok(()), "{}", s);
        }
    }
}
