// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The character and length rules for a channel's **name** and
//! **description**, defined once
//! ([ADR-0051 §4](../../../docs/adr/0051-group-and-channel-names.md),
//! [ADR-0059](../../../docs/adr/0059-channel-discover.md)).
//!
//! ## Why the rule lives here and not in `lakko-channels`
//!
//! Three wires carry a channel name: the encrypted channel metadata record
//! (`lakko_channels::meta`), the public directory listing
//! (`lakko_channels::listing`), and the group envelope in which a member
//! shares the channels they follow ([`crate::shared_channels`]). A name the
//! channel stated must be a name the member can share, so the three must
//! refuse exactly the same inputs. `lakko-channels` depends on this crate
//! (for the username rule a listing's handle obeys); this crate cannot
//! depend on `lakko-channels`, so the one definition sits here and
//! `lakko_channels::meta` re-exports it under its historical names.
//!
//! The rule is deliberately NOT the profile display-name rule
//! ([`crate::profile::check_display_name`]), which refuses the whole
//! general-category `Cf` table: a channel name refuses only the subset in
//! [`REFUSED_FORMAT_CHAR_RANGES`] (the bidi controls and the invisible
//! spacers), as ADR-0051 §4 specified before this module existed. Moving
//! the code changed no accepted or refused input.
//!
//! ## The rules
//!
//! * name: valid UTF-8, trimmed, 1 to [`NAME_MAX_CHARS`] Unicode scalar
//!   values and at most [`NAME_MAX_BYTES`] bytes, no line break, no control
//!   character (`char::is_control`), no refused format character.
//! * description: trimmed, 0 to [`DESCRIPTION_MAX_CHARS`] scalars and at
//!   most [`DESCRIPTION_MAX_BYTES`] bytes, LINE FEED permitted, every other
//!   rule the same.
//!
//! ZERO WIDTH NON-JOINER (U+200C) and ZERO WIDTH JOINER (U+200D) are
//! permitted: ZWJ builds emoji sequences and ZWNJ is required by Persian and
//! several Indic spellings.

use core::result::Result as CoreResult;

use thiserror::Error;

/// Longest admissible name, in Unicode scalar values.
pub const NAME_MAX_CHARS: usize = 64;

/// Longest admissible name, in UTF-8 bytes — the one-byte `name_len`
/// field's ceiling.
pub const NAME_MAX_BYTES: usize = u8::MAX as usize;

/// Longest admissible description, in Unicode scalar values.
pub const DESCRIPTION_MAX_CHARS: usize = 300;

/// Longest admissible description, in UTF-8 bytes. Below the two-byte
/// `description_len` ceiling on purpose: 300 four-byte scalars would be
/// 1200 bytes, so the byte cap can bite before the scalar cap.
pub const DESCRIPTION_MAX_BYTES: usize = 1024;

/// Format characters (general category `Cf`) refused in both fields, as
/// inclusive code-point ranges. See the module docs for what is refused and
/// why; [`PERMITTED_JOINERS`] carves ZWNJ and ZWJ back out of the second row.
pub const REFUSED_FORMAT_CHAR_RANGES: &[(u32, u32)] = &[
    (0x00AD, 0x00AD), // SOFT HYPHEN
    (0x200B, 0x200F), // ZWSP, ZWNJ, ZWJ, LRM, RLM (ZWNJ + ZWJ permitted below)
    (0x202A, 0x202E), // LRE, RLE, PDF, LRO, RLO
    (0x2060, 0x2064), // WORD JOINER .. INVISIBLE PLUS
    (0x2066, 0x2069), // LRI, RLI, FSI, PDI
    (0xFEFF, 0xFEFF), // ZERO WIDTH NO-BREAK SPACE (BOM)
];

/// ZERO WIDTH NON-JOINER and ZERO WIDTH JOINER: admitted anywhere.
const PERMITTED_JOINERS: [char; 2] = ['\u{200C}', '\u{200D}'];

/// `true` for a format character this module refuses.
#[must_use]
pub fn is_refused_format_char(c: char) -> bool {
    if PERMITTED_JOINERS.contains(&c) {
        return false;
    }
    let cp = u32::from(c);
    REFUSED_FORMAT_CHAR_RANGES
        .iter()
        .any(|&(lo, hi)| (lo..=hi).contains(&cp))
}

/// `true` for a character that breaks a line: every `Cc` line terminator
/// (LF, VT, FF, CR, NEL) plus LINE SEPARATOR and PARAGRAPH SEPARATOR, which
/// are not `Cc` and would otherwise slip past the control check.
const fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// Why a name or a description was refused.
///
/// A reason code: no variant carries the offending text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelTextError {
    /// The name is empty. A channel needs a name; only the name can hit
    /// this, a description may be empty.
    #[error("field is empty")]
    Empty,
    /// Leading or trailing whitespace.
    #[error("field has leading or trailing whitespace")]
    NotTrimmed,
    /// Contains a line break the field does not permit (every one for the
    /// name; every one but LINE FEED for the description).
    #[error("field contains a line break")]
    LineBreak,
    /// Contains a control character or a refused format character (the bidi
    /// controls and the invisible spacers).
    #[error("field contains a control or format character")]
    ControlOrFormatCharacter,
    /// More than the field's scalar-value cap.
    #[error("field is longer than {max_chars} characters")]
    TooLong {
        /// The cap that applied ([`NAME_MAX_CHARS`] or
        /// [`DESCRIPTION_MAX_CHARS`]).
        max_chars: usize,
    },
    /// More than the field's UTF-8 byte cap.
    #[error("field is longer than {max_bytes} bytes")]
    TooManyBytes {
        /// The cap that applied ([`NAME_MAX_BYTES`] or
        /// [`DESCRIPTION_MAX_BYTES`]).
        max_bytes: usize,
    },
    /// The wire bytes are not valid UTF-8. Decode-only.
    #[error("field is not valid UTF-8")]
    NotUtf8,
}

impl ChannelTextError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotTrimmed => "notTrimmed",
            Self::LineBreak => "lineBreak",
            Self::ControlOrFormatCharacter => "controlOrFormatCharacter",
            Self::TooLong { .. } => "tooLong",
            Self::TooManyBytes { .. } => "tooManyBytes",
            Self::NotUtf8 => "notUtf8",
        }
    }
}

/// Check that `name` is an admissible channel name **as it stands** (no
/// trimming is applied).
///
/// Reasons are tested in a fixed order: empty, untrimmed, line break,
/// control / format character, then the two lengths.
///
/// # Errors
///
/// Returns the first [`ChannelTextError`] that applies.
pub fn check_name(name: &str) -> CoreResult<(), ChannelTextError> {
    if name.is_empty() {
        return Err(ChannelTextError::Empty);
    }
    check_field(name, is_line_break, NAME_MAX_CHARS, NAME_MAX_BYTES)
}

/// Check that `description` is an admissible channel description **as it
/// stands** (no trimming is applied). Empty is admissible; LINE FEED is
/// admissible.
///
/// # Errors
///
/// Returns the first [`ChannelTextError`] that applies.
pub fn check_description(description: &str) -> CoreResult<(), ChannelTextError> {
    check_field(
        description,
        |c| c != '\n' && is_line_break(c),
        DESCRIPTION_MAX_CHARS,
        DESCRIPTION_MAX_BYTES,
    )
}

fn check_field(
    text: &str,
    is_refused_break: impl Fn(char) -> bool,
    max_chars: usize,
    max_bytes: usize,
) -> CoreResult<(), ChannelTextError> {
    if text.trim() != text {
        return Err(ChannelTextError::NotTrimmed);
    }
    if text.chars().any(&is_refused_break) {
        return Err(ChannelTextError::LineBreak);
    }
    if text
        .chars()
        .any(|c| (c.is_control() && c != '\n') || is_refused_format_char(c))
    {
        return Err(ChannelTextError::ControlOrFormatCharacter);
    }
    if text.chars().count() > max_chars {
        return Err(ChannelTextError::TooLong { max_chars });
    }
    if text.len() > max_bytes {
        return Err(ChannelTextError::TooManyBytes { max_bytes });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_description_rules_are_the_adr_0051_ones() {
        assert_eq!(check_name("Lakko News"), Ok(()));
        assert_eq!(check_name(""), Err(ChannelTextError::Empty));
        assert_eq!(
            check_name(&"a".repeat(NAME_MAX_CHARS + 1)),
            Err(ChannelTextError::TooLong { max_chars: 64 })
        );
        assert_eq!(
            check_name("La\u{202E}kko"),
            Err(ChannelTextError::ControlOrFormatCharacter)
        );
        assert_eq!(check_name("\u{1F468}\u{200D}\u{1F469}"), Ok(()));
        assert_eq!(check_description(""), Ok(()));
        assert_eq!(check_description("one\ntwo"), Ok(()));
        assert_eq!(
            check_description("one\rtwo"),
            Err(ChannelTextError::LineBreak)
        );
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(ChannelTextError::Empty.code(), "empty");
        assert_eq!(ChannelTextError::NotUtf8.code(), "notUtf8");
        assert_eq!(
            ChannelTextError::TooManyBytes { max_bytes: 1 }.code(),
            "tooManyBytes"
        );
    }
}
