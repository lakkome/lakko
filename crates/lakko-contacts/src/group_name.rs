// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-name envelope — the shared name of a group, carried end-to-end
//! encrypted to every member
//! ([ADR-0051](../../../docs/adr/0051-group-and-channel-names.md) §1).
//!
//! ## What travels, and where
//!
//! A group name is `{name, updated_at_ms}`. It rides as an MLS application
//! message on the group's own journal, classified by
//! [`crate::incoming_envelope`] so it is never rendered as a chat bubble.
//! The server sees only ciphertext under the group's epoch secret
//! (invariants #1, #2), and no server-side table says which accounts share
//! a name (invariant #5).
//!
//! It is the [`crate::profile`] template applied to a group: one bounded,
//! fail-closed record, one stamp, one strict order. The differences are the
//! two the ADR names — there is no username field, and an **empty name is
//! admissible and means "clear the name"**, after which receivers fall back
//! to the member-derived title.
//!
//! ## What the envelope does NOT establish
//!
//! **A group name is a shared fact one person asserts about the whole
//! group.** This module bounds the shape of that assertion; it does not
//! decide whether the sender was entitled to make it:
//!
//! * **Authority is the RECEIVER's check, through the role table** (ADR-0051
//!   §2). The receiver resolves the MLS sender leaf to the account key at
//!   receipt (C-68) and reads the live ADR-0022 role table: owner or admin
//!   → accept if newer; plain member → drop; unresolvable sender or
//!   unreadable table → drop; no table (legacy group) → accept if newer.
//!   The envelope deliberately carries no identity and no role of its own —
//!   a role named in the payload would be an unauthenticated claim beside
//!   the MLS-authenticated one, and the only correct reading of it is to
//!   ignore it.
//! * The gate runs **before** the order comparison, and the whole
//!   read-decide-write runs under the per-group lock the push routing
//!   secret already uses (C-62 sub-invariant 5: a gate must read current
//!   state).
//! * The name is unverifiable by construction and is shown as the setter's
//!   statement.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       GROUP_NAME_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || name_len || name
//!      └────── 20 bytes, ASCII ──────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └── 1 B ──┘ └ .. ┘
//!
//!                     "lakko/group-name/v1\x00"
//! ```
//!
//! A `name_len` of `0` means "clear the name". Total length is between
//! [`GROUP_NAME_ENVELOPE_MIN_LEN`] and [`GROUP_NAME_ENVELOPE_MAX_LEN`].
//!
//! ## Bounds, and why a violation rejects the whole envelope
//!
//! A non-empty `name` must satisfy exactly the profile display-name rule,
//! [`crate::profile::check_display_name`]: valid UTF-8, trimmed, 1 to
//! [`crate::profile::DISPLAY_NAME_MAX_CHARS`] Unicode scalar values and at most
//! [`DISPLAY_NAME_MAX_BYTES`] bytes, no line break, no control character,
//! no format character outside the permitted joiners and subdivision-flag
//! tags. The rule is **reused, not restated**, so the two names cannot
//! drift apart. An envelope violating any bound is **rejected as a whole**;
//! **`compose_group_name` refuses exactly what `decode_group_name`
//! refuses**, since both run the same [`GroupName::validate`].
//!
//! ## Order — and the stamp must be re-emitted VERBATIM
//!
//! [`group_name_is_newer`]: a receiver keeps the name with the greater
//! `updated_at_ms` **per group**; equal is not newer. The stamp is the
//! setter's own clock; ties keep what is stored (UX spec: "two admins
//! rename at once — the newer stamp wins on every device; equal stamps keep
//! what is stored").
//!
//! The ADR-0044 lesson applies without change: a **growth re-send** (the
//! owner and admins re-broadcast the current name when the group gains a
//! member, so a newcomer learns it) must carry the ORIGINAL stamp — the
//! stamp is durable per `(account, group)` state, persisted beside the name
//! and re-emitted unchanged. A re-send that re-stamps the current time makes
//! an old name arrive **newer** than a rename that landed in between, and
//! every member clobbers the rename with the stale name. This module owns
//! the order; persisting and replaying the stamp verbatim is the caller's
//! obligation.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it, since
//! [`crate::incoming_envelope`] dispatches on a byte-prefix match.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::profile::{
    check_display_name, normalize_display_name, DisplayNameError, DISPLAY_NAME_MAX_BYTES,
};

/// 20-byte domain separator prepended to the group-name payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with, mirroring the sibling envelopes. The printable
/// prefix is `lakko/group-name/v1`.
///
/// **The `v1` names the envelope FAMILY, not the schema version.** These
/// bytes are frozen: [`crate::incoming_envelope`] dispatches on this exact
/// prefix. The in-payload version byte is the knob that moves.
pub const GROUP_NAME_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-name/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const GROUP_NAME_VERSION_V1: u8 = 0x01;

/// Bytes after the separator that every envelope carries: version,
/// `updated_at_ms`, `name_len`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1;

/// Shortest well-formed envelope: an empty name ("clear the name").
pub const GROUP_NAME_ENVELOPE_MIN_LEN: usize =
    GROUP_NAME_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Longest well-formed envelope: a maximal name.
pub const GROUP_NAME_ENVELOPE_MAX_LEN: usize = GROUP_NAME_ENVELOPE_MIN_LEN + DISPLAY_NAME_MAX_BYTES;

/// A group's shared name, as set by an owner or admin.
///
/// **The name is the setter's assertion about the whole group.** See the
/// module docs: the receiver decides through the role table whether the
/// sender may make it, and through [`group_name_is_newer`] whether it is
/// current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupName {
    /// The name. **Empty means "clear the name"** — receivers fall back to
    /// the member-derived title. When non-empty it must satisfy
    /// [`crate::profile::check_display_name`] — in particular it is already
    /// trimmed. Use [`normalize_group_name`] on user input.
    pub name: String,
    /// Setter-clock UNIX milliseconds at which this name was set. Orders
    /// names for the same group via [`group_name_is_newer`]. Persisted and
    /// re-emitted verbatim on a growth re-send (see the module docs).
    pub updated_at_ms: u64,
}

impl GroupName {
    /// Check every bound. The single rule both [`compose_group_name`] and
    /// [`decode_group_name`] run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// * [`GroupNameError::Name`] — a non-empty name violates a display-name
    ///   bound.
    pub fn validate(&self) -> CoreResult<(), GroupNameError> {
        if !self.name.is_empty() {
            check_display_name(&self.name).map_err(GroupNameError::Name)?;
        }
        Ok(())
    }
}

/// Turn what a person typed into the group name to store and send.
///
/// Trims, maps "nothing left" to `Ok(String::new())` (ADR-0051 §1: empty
/// clears the name), and otherwise runs
/// [`crate::profile::check_display_name`] on the trimmed text.
///
/// # Errors
///
/// Returns the [`DisplayNameError`] the trimmed text violates.
pub fn normalize_group_name(input: &str) -> CoreResult<String, DisplayNameError> {
    Ok(normalize_display_name(input)?.unwrap_or_default())
}

/// `true` when a name stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` for the **same group**.
///
/// Strictly greater wins; **equal is not newer**, so a growth re-send of the
/// name a receiver already holds is not an update, and two renames with one
/// stamp keep what is stored.
#[must_use]
pub const fn group_name_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a group-name envelope.
///
/// The output is the **inner** plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// # Errors
///
/// Exactly the errors [`GroupName::validate`] returns — `compose` refuses
/// what [`decode_group_name`] would refuse.
pub fn compose_group_name(group_name: &GroupName) -> CoreResult<Vec<u8>, GroupNameError> {
    group_name.validate()?;
    let name = group_name.name.as_bytes();
    // `validate` bounded the length below `u8::MAX`; the conversion is
    // re-checked rather than cast so a future bound change cannot truncate.
    let name_len = u8::try_from(name.len()).map_err(|_| {
        GroupNameError::Name(DisplayNameError::TooManyBytes {
            max_bytes: DISPLAY_NAME_MAX_BYTES,
        })
    })?;

    let mut out = Vec::with_capacity(GROUP_NAME_ENVELOPE_MIN_LEN + name.len());
    out.extend_from_slice(GROUP_NAME_ENVELOPE_DOMAIN_V1);
    out.push(GROUP_NAME_VERSION_V1);
    out.extend_from_slice(&group_name.updated_at_ms.to_be_bytes());
    out.push(name_len);
    out.extend_from_slice(name);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(group_name))` — the domain separator matched and the envelope
///   is well-formed and within every bound.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed or out of
///   bounds. The caller drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`GroupNameError::Truncated`] — the payload ends before a declared
///   field.
/// * [`GroupNameError::UnknownVersion`] — the version byte is not
///   [`GROUP_NAME_VERSION_V1`].
/// * [`GroupNameError::TrailingBytes`] — bytes remain after the name.
/// * [`GroupNameError::Name`] — the name is out of bounds.
pub fn try_unwrap_group_name(plaintext: &[u8]) -> CoreResult<Option<GroupName>, GroupNameError> {
    let Some(payload) = plaintext.strip_prefix(GROUP_NAME_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;

    let version = take_u8(&mut cur)?;
    if version != GROUP_NAME_VERSION_V1 {
        return Err(GroupNameError::UnknownVersion(version));
    }
    let updated_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| GroupNameError::Truncated)?;
    let updated_at_ms = u64::from_be_bytes(updated_at_bytes);

    let name_len = usize::from(take_u8(&mut cur)?);
    let name_bytes = take(&mut cur, name_len)?;
    if !cur.is_empty() {
        return Err(GroupNameError::TrailingBytes { count: cur.len() });
    }

    let name = core::str::from_utf8(name_bytes)
        .map_err(|_| GroupNameError::Name(DisplayNameError::NotUtf8))?
        .to_string();

    let group_name = GroupName {
        name,
        updated_at_ms,
    };
    group_name.validate()?;
    Ok(Some(group_name))
}

/// Strict standalone decoder: a non-group-name plaintext is a hard error.
///
/// # Errors
///
/// * [`GroupNameError::WrongDomain`] — `bytes` does not start with
///   [`GROUP_NAME_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_group_name`] returns.
pub fn decode_group_name(bytes: &[u8]) -> CoreResult<GroupName, GroupNameError> {
    try_unwrap_group_name(bytes)?.ok_or(GroupNameError::WrongDomain)
}

/// Why a group-name envelope was refused.
///
/// A reason code: no variant carries the offending name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupNameError {
    /// The plaintext does not carry the group-name domain separator. Only
    /// from [`decode_group_name`]; [`try_unwrap_group_name`] returns
    /// `Ok(None)`.
    #[error("plaintext does not carry the group-name domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("group-name envelope is truncated")]
    Truncated,
    /// Bytes remain after the name field.
    #[error("group-name envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the name.
        count: usize,
    },
    /// The version byte is not [`GROUP_NAME_VERSION_V1`].
    #[error("group-name envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The name violates a display-name bound.
    #[error("invalid group name: {0}")]
    Name(DisplayNameError),
}

impl GroupNameError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    ///
    /// Name errors are namespaced (`name.tooLong`,
    /// `name.controlOrFormatCharacter`), the profile's shape.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::Name(e) => format!("name.{}", e.code()),
        }
    }
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, GroupNameError> {
    let (&first, rest) = cur.split_first().ok_or(GroupNameError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], GroupNameError> {
    if cur.len() < n {
        return Err(GroupNameError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::DISPLAY_NAME_MAX_CHARS;

    fn group_name(name: &str, at: u64) -> GroupName {
        GroupName {
            name: name.to_string(),
            updated_at_ms: at,
        }
    }

    /// Hand-build an envelope with arbitrary (possibly invalid) fields,
    /// bypassing `compose`'s validation — so decode's own gates are tested.
    fn raw(version: u8, at: u64, name: &[u8]) -> Vec<u8> {
        let mut out = GROUP_NAME_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&at.to_be_bytes());
        out.push(u8::try_from(name.len()).unwrap());
        out.extend_from_slice(name);
        out
    }

    // --- wire format ---------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        for g in [
            group_name("Trio", 1_700_000_000_000),
            group_name("", 0),
            group_name("Lakko News", u64::MAX),
            group_name("Семейный чат", 1),
            group_name("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} Family", 7),
        ] {
            let wire = compose_group_name(&g).unwrap();
            assert_eq!(decode_group_name(&wire).unwrap(), g);
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire = compose_group_name(&group_name("Al", 0x0102_0304_0506_0708)).unwrap();
        let sep = GROUP_NAME_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(sep, 20);
        assert_eq!(&wire[..sep], b"lakko/group-name/v1\x00");
        assert_eq!(wire[sep], GROUP_NAME_VERSION_V1);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 2);
        assert_eq!(&wire[sep + 10..], b"Al");
    }

    #[test]
    fn envelope_length_bounds_are_the_documented_construction() {
        assert_eq!(GROUP_NAME_ENVELOPE_MIN_LEN, 30);
        assert_eq!(GROUP_NAME_ENVELOPE_MAX_LEN, 30 + 255);
        let empty = compose_group_name(&group_name("", 0)).unwrap();
        assert_eq!(empty.len(), GROUP_NAME_ENVELOPE_MIN_LEN);
        // A maximal envelope: 255 bytes of name in exactly 64 chars (63
        // four-byte scalars + one three-byte scalar).
        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        assert_eq!(name.len(), 255);
        assert_eq!(name.chars().count(), DISPLAY_NAME_MAX_CHARS);
        let max = compose_group_name(&group_name(&name, 0)).unwrap();
        assert_eq!(max.len(), GROUP_NAME_ENVELOPE_MAX_LEN);
    }

    // --- the empty name --------------------------------------------------------

    #[test]
    fn an_empty_name_clears_and_round_trips() {
        // ADR-0051 §1: empty is admissible and means "clear the name" — the
        // one place this codec departs from the profile display-name rule,
        // which spells "no name" as `None` and refuses `Some("")`.
        let wire = compose_group_name(&group_name("", 42)).unwrap();
        let decoded = decode_group_name(&wire).unwrap();
        assert_eq!(decoded.name, "");
        assert_eq!(decoded.updated_at_ms, 42);
        assert_eq!(decode_group_name(&raw(1, 42, b"")), Ok(group_name("", 42)));
    }

    #[test]
    fn normalize_trims_and_maps_blank_to_clear() {
        assert_eq!(normalize_group_name("  Trio  "), Ok("Trio".to_string()));
        assert_eq!(normalize_group_name(""), Ok(String::new()));
        assert_eq!(normalize_group_name(" \t\u{3000} "), Ok(String::new()));
        assert_eq!(
            normalize_group_name(" A\u{202E}B "),
            Err(DisplayNameError::ControlOrFormatCharacter)
        );
        assert_eq!(
            normalize_group_name(&"a".repeat(DISPLAY_NAME_MAX_CHARS + 1)),
            Err(DisplayNameError::TooLong { max_chars: 64 })
        );
    }

    // --- fail-closed decode --------------------------------------------------

    #[test]
    fn non_group_name_plaintext_falls_through() {
        assert_eq!(try_unwrap_group_name(b"hi alice"), Ok(None));
        assert_eq!(try_unwrap_group_name(&[]), Ok(None));
        assert_eq!(
            decode_group_name(b"hi alice"),
            Err(GroupNameError::WrongDomain)
        );
    }

    #[test]
    fn bare_separator_is_truncated_not_a_fall_through() {
        assert_eq!(
            try_unwrap_group_name(GROUP_NAME_ENVELOPE_DOMAIN_V1),
            Err(GroupNameError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_group_name(&group_name("Trio", 9)).unwrap();
        for cut in GROUP_NAME_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_group_name(&wire[..cut]),
                Err(GroupNameError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut wire = compose_group_name(&group_name("Trio", 9)).unwrap();
        wire.push(0);
        assert_eq!(
            decode_group_name(&wire),
            Err(GroupNameError::TrailingBytes { count: 1 })
        );
        // Also on the empty (clear) envelope, where a stray byte could be
        // misread as a one-byte name if the length were not consulted.
        let mut empty = compose_group_name(&group_name("", 9)).unwrap();
        empty.push(b'x');
        assert_eq!(
            decode_group_name(&empty),
            Err(GroupNameError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn unknown_versions_are_refused() {
        for v in [0x00, 0x02, 0xFF] {
            assert_eq!(
                decode_group_name(&raw(v, 0, b"")),
                Err(GroupNameError::UnknownVersion(v))
            );
        }
    }

    #[test]
    fn non_utf8_name_is_refused() {
        assert_eq!(
            decode_group_name(&raw(1, 0, &[0xFF, 0xFE])),
            Err(GroupNameError::Name(DisplayNameError::NotUtf8))
        );
    }

    #[test]
    fn every_bound_is_refused_on_both_compose_and_decode() {
        // Each invalid name: compose errs, and the same bytes on the wire
        // (hand-built, bypassing compose) produce the same error from decode.
        let cases: &[(&str, DisplayNameError)] = &[
            (" Trio", DisplayNameError::NotTrimmed),
            ("Trio ", DisplayNameError::NotTrimmed),
            ("Tr\nio", DisplayNameError::LineBreak),
            ("Tr\u{2028}io", DisplayNameError::LineBreak),
            ("Tr\u{0}io", DisplayNameError::ControlOrFormatCharacter),
            ("Tr\u{202E}io", DisplayNameError::ControlOrFormatCharacter),
            ("Tr\u{200B}io", DisplayNameError::ControlOrFormatCharacter),
            ("Tr\u{FEFF}io", DisplayNameError::ControlOrFormatCharacter),
        ];
        for &(name, why) in cases {
            let compose_err = compose_group_name(&group_name(name, 0)).unwrap_err();
            let decode_err = decode_group_name(&raw(1, 0, name.as_bytes())).unwrap_err();
            assert_eq!(compose_err, GroupNameError::Name(why), "{name:?}");
            assert_eq!(compose_err, decode_err, "{name:?}");
        }
        // Too many scalars (fits the one-byte length, so hand-buildable).
        let long = "a".repeat(DISPLAY_NAME_MAX_CHARS + 1);
        let expected = GroupNameError::Name(DisplayNameError::TooLong { max_chars: 64 });
        assert_eq!(compose_group_name(&group_name(&long, 0)), Err(expected));
        assert_eq!(
            decode_group_name(&raw(1, 0, long.as_bytes())),
            Err(expected)
        );
        // Too many bytes within the scalar cap: 64 four-byte scalars = 256
        // bytes. Compose refuses; on the wire it cannot even be expressed,
        // since the one-byte length caps at 255.
        let fat = "\u{1F600}".repeat(64);
        assert_eq!(
            compose_group_name(&group_name(&fat, 0)),
            Err(GroupNameError::Name(DisplayNameError::TooManyBytes {
                max_bytes: 255
            }))
        );
    }

    #[test]
    fn the_profile_rule_is_reused_not_restated() {
        // Whatever the profile display-name check admits or refuses, the
        // group name does too — pinned over the profile's own corpus of
        // accepted scripts, so a future divergence in either table shows.
        for ok in [
            "Alice Petrova",
            "Анна",
            "李小龙",
            "محمد",
            "O'Brien-Smith",
            "\u{1F600} Party",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} Family",
            "Ian \u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
        ] {
            assert_eq!(check_display_name(ok), Ok(()));
            assert!(compose_group_name(&group_name(ok, 0)).is_ok(), "{ok:?}");
        }
        for bad in ["Al\u{E0067}ice", "\u{1F3F4}\u{E007F}", "Al\u{AD}ice"] {
            assert!(check_display_name(bad).is_err());
            assert!(compose_group_name(&group_name(bad, 0)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn error_messages_never_echo_the_name() {
        let err = decode_group_name(&raw(1, 0, "Secret\u{202E}".as_bytes())).unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        assert!(!format!("{err:?}").contains("Secret"));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(GroupNameError::WrongDomain.code(), "wrongDomain");
        assert_eq!(GroupNameError::Truncated.code(), "truncated");
        assert_eq!(
            GroupNameError::TrailingBytes { count: 3 }.code(),
            "trailingBytes"
        );
        assert_eq!(GroupNameError::UnknownVersion(9).code(), "unknownVersion");
        assert_eq!(
            GroupNameError::Name(DisplayNameError::TooLong { max_chars: 64 }).code(),
            "name.tooLong"
        );
        assert_eq!(
            GroupNameError::Name(DisplayNameError::ControlOrFormatCharacter).code(),
            "name.controlOrFormatCharacter"
        );
    }

    // --- order ---------------------------------------------------------------

    #[test]
    fn newer_is_strictly_greater() {
        assert!(group_name_is_newer(2, 1));
        assert!(!group_name_is_newer(1, 1), "equal is not newer");
        assert!(!group_name_is_newer(0, 1));
        assert!(group_name_is_newer(u64::MAX, u64::MAX - 1));
        assert!(!group_name_is_newer(u64::MAX, u64::MAX));
    }

    #[test]
    fn a_growth_resend_with_the_original_stamp_never_clobbers_a_rename() {
        // The ADR-0044 lesson, on this envelope: the setter persists the
        // stamp and re-emits it VERBATIM. An admin set "Trio" at t=10; a
        // rename to "Quartet" landed at t=20; the growth re-send of "Trio"
        // still carries 10 and so loses on every receiver.
        let original = group_name("Trio", 10);
        let renamed = group_name("Quartet", 20);
        let resend = decode_group_name(&compose_group_name(&original).unwrap()).unwrap();
        assert_eq!(resend.updated_at_ms, 10);
        assert!(!group_name_is_newer(
            resend.updated_at_ms,
            renamed.updated_at_ms
        ));
        // Had the re-send been re-stamped at "now" (t=30), it would win —
        // exactly the defect the verbatim rule exists to prevent.
        assert!(group_name_is_newer(30, renamed.updated_at_ms));
    }

    // --- cross-kind isolation ------------------------------------------------

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, GROUP_NAME_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(GROUP_NAME_ENVELOPE_DOMAIN_V1)
                    && !GROUP_NAME_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn group_name_does_not_cross_dispatch_as_a_sibling() {
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::committer_beacon::{compose_committer_beacon, try_unwrap_committer_beacon};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};
        use crate::profile::{compose_profile, try_unwrap_profile, Profile};
        use crate::push_routing::{compose_push_routing_secret, try_unwrap_push_routing_secret};

        let wire = compose_group_name(&group_name("Trio", 1)).unwrap();
        assert!(try_unwrap_call_welcome(&wire).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&wire).unwrap().is_none());
        assert!(try_unwrap_media_state(&wire).unwrap().is_none());
        assert!(try_unwrap_push_routing_secret(&wire).unwrap().is_none());
        assert!(try_unwrap_committer_beacon(&wire).unwrap().is_none());
        assert!(try_unwrap_profile(&wire).unwrap().is_none());

        let profile = Profile {
            display_name: Some("Alice".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 1,
            account_id: None,
        };
        for sibling in [
            wrap_call_welcome(&[0xAB; 256]),
            wrap_call_kp_forward(&[0xCD; 512]),
            compose_media_state(true),
            compose_push_routing_secret(7, &[0x11; 32]),
            compose_committer_beacon(),
            compose_profile(&profile).unwrap(),
        ] {
            assert_eq!(try_unwrap_group_name(&sibling), Ok(None));
        }
    }
}
