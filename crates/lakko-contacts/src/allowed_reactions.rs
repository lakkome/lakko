// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which reactions a group or a channel takes, as its owner and admins set
//! them (UX `docs/ux/reactions-and-emoji.md` section 9, maintainer
//! 2026-11-02): all, none, or a list of 1 to [`ALLOWED_REACTIONS_MAX`] emoji.
//!
//! One type and one encoding for both carriers, so the two cannot drift: the
//! channel metadata record (`lakko_channels::meta`, where the type was first
//! written) and the group envelope ([`crate::group_reactions`]). This module
//! owns the value, its bounds and its bytes; each carrier owns its own domain,
//! version and stamp.
//!
//! ## Encoding
//!
//! ```text
//!   mode(1)                  0x00 all, 0x01 none, 0x02 a list
//!   [count(1)                1..=ALLOWED_REACTIONS_MAX, list only
//!    count x (len(1) || emoji)]   len 1..=REACTION_EMOJI_MAX_BYTES, UTF-8
//! ```
//!
//! Any other mode byte refuses the carrier. The list keeps the setter's order
//! (the order a composer offers the emoji in); a repeated emoji refuses it.
//! The encoding is canonical: whatever decodes re-encodes to the identical
//! bytes.
//!
//! ## What the value does NOT establish
//!
//! **It is advisory by construction.** Under a channel post the service sees
//! labels only and cannot refuse an emoji; in a group the only party able to
//! refuse a reaction is each member's own client. So it is a DISPLAY rule:
//! readers hide chips of reactions the conversation does not take, and the
//! composer does not offer them. A reaction already given stays given; it
//! reappears if it is allowed again (UX D11).
//!
//! Each emoji here passes the wire bound every reaction parser enforces
//! ([`is_reaction_emoji_bytes`]: 1 to [`REACTION_EMOJI_MAX_BYTES`] bytes,
//! equal to `lakko_attachments::MAX_EMOJI_BYTES`, pinned by a test in
//! `lakko-channels`, which depends on both). Whether an entry is ONE emoji is
//! the grapheme rule of `lakko_ui_rules::reaction`, which the composer and the
//! chip rule apply on top; this crate cannot reach that rule without a
//! dependency cycle (`lakko-ui-rules` depends on `lakko-contacts`).

use thiserror::Error;

/// The most emoji an allowed-reactions list holds. Chosen (Telegram's
/// `reactions_in_chat_max` is 100 too).
pub const ALLOWED_REACTIONS_MAX: usize = 100;

/// Longest reaction emoji in bytes, the bound every reaction parser enforces.
/// Equal to `lakko_attachments::MAX_EMOJI_BYTES` (pinned by a test where both
/// crates are in reach).
pub const REACTION_EMOJI_MAX_BYTES: usize = 64;

/// Longest encoding of an [`AllowedReactions`]: the mode, the count and a
/// full list of maximal emoji.
pub const ALLOWED_REACTIONS_ENCODED_MAX_LEN: usize =
    1 + 1 + ALLOWED_REACTIONS_MAX * (1 + REACTION_EMOJI_MAX_BYTES);

const MODE_ALL: u8 = 0x00;
const MODE_NONE: u8 = 0x01;
const MODE_LIST: u8 = 0x02;

/// The byte bound a reaction emoji passes on every wire: not empty and at
/// most [`REACTION_EMOJI_MAX_BYTES`] bytes. The same rule as
/// `lakko_attachments::is_reaction_emoji`.
#[must_use]
pub const fn is_reaction_emoji_bytes(emoji: &str) -> bool {
    !emoji.is_empty() && emoji.len() <= REACTION_EMOJI_MAX_BYTES
}

/// Which reactions a group's messages or a channel's posts take.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AllowedReactions {
    /// Any reaction.
    #[default]
    All,
    /// No reactions.
    Off,
    /// Only these, 1 to [`ALLOWED_REACTIONS_MAX`] distinct emoji, in the
    /// setter's order (the order a composer offers them in).
    Only(Vec<String>),
}

/// Why an allowed-reactions value was refused. A reason, never the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AllowedReactionsError {
    /// The bytes end before a declared field.
    #[error("allowed-reactions value is truncated")]
    Truncated,
    /// The mode byte is none of all, none and list.
    #[error("unknown allowed-reactions mode {0:#04x}")]
    Mode(u8),
    /// The list is empty, longer than [`ALLOWED_REACTIONS_MAX`], repeats an
    /// emoji, or holds an entry that is not UTF-8 or fails
    /// [`is_reaction_emoji_bytes`].
    #[error("invalid allowed-reactions list")]
    List,
}

impl AllowedReactionsError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Truncated => "truncated",
            Self::Mode(_) => "reactionsMode",
            Self::List => "reactions",
        }
    }
}

impl AllowedReactions {
    /// Whether the conversation takes `emoji`. `All` admits whatever passes
    /// the wire bound ([`is_reaction_emoji_bytes`]); whether it is ONE emoji
    /// is the caller's grapheme rule (`lakko_ui_rules::reaction`).
    #[must_use]
    pub fn allows(&self, emoji: &str) -> bool {
        match self {
            Self::All => is_reaction_emoji_bytes(emoji),
            Self::Off => false,
            Self::Only(list) => list.iter().any(|e| e == emoji),
        }
    }

    /// Check the list bounds. The single rule every carrier's compose and
    /// decode run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// [`AllowedReactionsError::List`] for an empty or over-long list, an
    /// entry failing [`is_reaction_emoji_bytes`], or a repeat.
    pub fn validate(&self) -> Result<(), AllowedReactionsError> {
        let Self::Only(list) = self else {
            return Ok(());
        };
        if list.is_empty() || list.len() > ALLOWED_REACTIONS_MAX {
            return Err(AllowedReactionsError::List);
        }
        for (i, e) in list.iter().enumerate() {
            if !is_reaction_emoji_bytes(e) || list[..i].contains(e) {
                return Err(AllowedReactionsError::List);
            }
        }
        Ok(())
    }

    /// Append the encoding (see the module docs) after validating.
    ///
    /// # Errors
    ///
    /// Exactly what [`Self::validate`] returns.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), AllowedReactionsError> {
        self.validate()?;
        match self {
            Self::All => out.push(MODE_ALL),
            Self::Off => out.push(MODE_NONE),
            Self::Only(list) => {
                out.push(MODE_LIST);
                // `validate` bounded both; re-checked rather than cast.
                out.push(u8::try_from(list.len()).map_err(|_| AllowedReactionsError::List)?);
                for e in list {
                    out.push(u8::try_from(e.len()).map_err(|_| AllowedReactionsError::List)?);
                    out.extend_from_slice(e.as_bytes());
                }
            },
        }
        Ok(())
    }

    /// Read one value from the front of `cur`, advancing it. Bounded: the
    /// count and every length are checked before anything is copied.
    ///
    /// **Structural only: a repeated emoji is not checked here.** The carrier
    /// runs [`Self::validate`] as part of its whole-record validation, after
    /// its own trailing-bytes check, so the order in which a malformed record
    /// is refused stays the carrier's ([`Self::from_bytes`] validates).
    ///
    /// # Errors
    ///
    /// Every [`AllowedReactionsError`].
    pub fn decode_from(cur: &mut &[u8]) -> Result<Self, AllowedReactionsError> {
        Ok(match take_u8(cur)? {
            MODE_ALL => Self::All,
            MODE_NONE => Self::Off,
            MODE_LIST => {
                let count = usize::from(take_u8(cur)?);
                if count == 0 || count > ALLOWED_REACTIONS_MAX {
                    return Err(AllowedReactionsError::List);
                }
                let mut list = Vec::with_capacity(count);
                for _ in 0..count {
                    let len = usize::from(take_u8(cur)?);
                    if len == 0 || len > REACTION_EMOJI_MAX_BYTES {
                        return Err(AllowedReactionsError::List);
                    }
                    let e = core::str::from_utf8(take(cur, len)?)
                        .map_err(|_| AllowedReactionsError::List)?;
                    list.push(e.to_string());
                }
                Self::Only(list)
            },
            other => return Err(AllowedReactionsError::Mode(other)),
        })
    }

    /// The encoding alone (see the module docs).
    ///
    /// # Errors
    ///
    /// Exactly what [`Self::validate`] returns.
    pub fn to_bytes(&self) -> Result<Vec<u8>, AllowedReactionsError> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    /// Decode a value that is the whole of `bytes`.
    ///
    /// # Errors
    ///
    /// Every [`AllowedReactionsError`]. Bytes left over after the value are
    /// [`AllowedReactionsError::List`]: a carrier reports its own trailing
    /// bytes, and a standalone value has nothing after it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AllowedReactionsError> {
        let mut cur = bytes;
        let value = Self::decode_from(&mut cur)?;
        if !cur.is_empty() {
            return Err(AllowedReactionsError::List);
        }
        value.validate()?;
        Ok(value)
    }
}

fn take_u8(cur: &mut &[u8]) -> Result<u8, AllowedReactionsError> {
    let (&first, rest) = cur.split_first().ok_or(AllowedReactionsError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8], AllowedReactionsError> {
    if cur.len() < n {
        return Err(AllowedReactionsError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUMBS: &str = "\u{1F44D}";
    const HEART: &str = "\u{2764}\u{FE0F}";

    #[test]
    fn the_three_modes_round_trip_and_the_bytes_are_pinned() {
        assert_eq!(AllowedReactions::All.to_bytes().unwrap(), vec![0]);
        assert_eq!(AllowedReactions::Off.to_bytes().unwrap(), vec![1]);
        let only = AllowedReactions::Only(vec![HEART.to_string(), THUMBS.to_string()]);
        let wire = only.to_bytes().unwrap();
        let mut expected = vec![2, 2, 6];
        expected.extend_from_slice(HEART.as_bytes());
        expected.push(4);
        expected.extend_from_slice(THUMBS.as_bytes());
        assert_eq!(wire, expected);
        for v in [AllowedReactions::All, AllowedReactions::Off, only] {
            assert_eq!(
                AllowedReactions::from_bytes(&v.to_bytes().unwrap()),
                Ok(v.clone())
            );
        }
    }

    #[test]
    fn allows_follows_the_mode() {
        let only = AllowedReactions::Only(vec![HEART.to_string(), THUMBS.to_string()]);
        assert!(only.allows(THUMBS));
        assert!(!only.allows("\u{1F525}"));
        assert!(AllowedReactions::All.allows("\u{1F525}"));
        assert!(!AllowedReactions::All.allows(""));
        assert!(!AllowedReactions::All.allows(&"x".repeat(65)));
        assert!(!AllowedReactions::Off.allows(THUMBS));
        assert_eq!(AllowedReactions::default(), AllowedReactions::All);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        let t = THUMBS.to_string();
        for bad in [
            AllowedReactions::Only(vec![]),
            AllowedReactions::Only(vec![t.clone(), t.clone()]),
            AllowedReactions::Only(vec![String::new()]),
            AllowedReactions::Only(vec!["x".repeat(65)]),
            AllowedReactions::Only(vec![t; 101]),
        ] {
            assert_eq!(bad.to_bytes(), Err(AllowedReactionsError::List), "{bad:?}");
        }
        // The same shapes hand-built on the wire.
        let mut dup = vec![2, 2, 4];
        dup.extend_from_slice(THUMBS.as_bytes());
        dup.push(4);
        dup.extend_from_slice(THUMBS.as_bytes());
        assert_eq!(
            AllowedReactions::from_bytes(&dup),
            Err(AllowedReactionsError::List)
        );
        assert_eq!(
            AllowedReactions::from_bytes(&[2, 0]),
            Err(AllowedReactionsError::List)
        );
        // A count past the bound is refused before anything is read.
        assert_eq!(
            AllowedReactions::from_bytes(&[2, 101]),
            Err(AllowedReactionsError::List)
        );
        assert_eq!(
            AllowedReactions::from_bytes(&[2, 1, 0]),
            Err(AllowedReactionsError::List)
        );
        assert_eq!(
            AllowedReactions::from_bytes(&[2, 1, 65]),
            Err(AllowedReactionsError::List)
        );
        assert_eq!(
            AllowedReactions::from_bytes(&[2, 1, 2, 0xFF, 0xFE]),
            Err(AllowedReactionsError::List)
        );
    }

    #[test]
    fn decode_is_fail_closed() {
        assert_eq!(
            AllowedReactions::from_bytes(&[]),
            Err(AllowedReactionsError::Truncated)
        );
        assert_eq!(
            AllowedReactions::from_bytes(&[3]),
            Err(AllowedReactionsError::Mode(3))
        );
        let wire = AllowedReactions::Only(vec![THUMBS.to_string()])
            .to_bytes()
            .unwrap();
        for cut in 1..wire.len() {
            assert_eq!(
                AllowedReactions::from_bytes(&wire[..cut]),
                Err(AllowedReactionsError::Truncated),
                "cut at {cut}"
            );
        }
        let mut trailing = wire;
        trailing.push(0);
        assert_eq!(
            AllowedReactions::from_bytes(&trailing),
            Err(AllowedReactionsError::List)
        );
    }

    #[test]
    fn the_longest_encoding_is_the_documented_construction() {
        let full: Vec<String> = (0..ALLOWED_REACTIONS_MAX)
            .map(|i| format!("{}{i:03}", "x".repeat(REACTION_EMOJI_MAX_BYTES - 3)))
            .collect();
        let wire = AllowedReactions::Only(full).to_bytes().unwrap();
        assert_eq!(wire.len(), ALLOWED_REACTIONS_ENCODED_MAX_LEN);
        assert_eq!(ALLOWED_REACTIONS_ENCODED_MAX_LEN, 6502);
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(AllowedReactionsError::Truncated.code(), "truncated");
        assert_eq!(AllowedReactionsError::Mode(9).code(), "reactionsMode");
        assert_eq!(AllowedReactionsError::List.code(), "reactions");
    }
}
