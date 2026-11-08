// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Profile envelope — a person's own display name and username, carried
//! end-to-end encrypted ([ADR-0048](../../../docs/adr/0048-profile-names.md)).
//!
//! ## What travels, and where
//!
//! A profile is `{display_name, username, updated_at_ms, account_id,
//! username_certificate, grants}`. It rides:
//!
//! * inside a [`crate::request::ContactRequest`]'s sealed envelope, so a
//!   recipient can show who is asking before accepting (the framing lives
//!   in [`crate::request`]);
//! * as an MLS application message in every group the person is in,
//!   classified by [`crate::incoming_envelope`] so it is never rendered as
//!   a chat bubble.
//!
//! The server sees neither: the first is sealed to the recipient's X25519
//! key, the second is sealed under the group's epoch secret
//! (invariants #1, #2).
//!
//! ## What the envelope does NOT establish
//!
//! **Everything in it is the sender's own claim.** This module bounds the
//! shape of that claim; it does not make it true:
//!
//! * The profile is bound to a person by the **receiver**, which stores it
//!   under the sender's signature key resolved at receipt (C-68) — the MLS
//!   sender leaf for an in-group profile, the request-signature-authenticated
//!   `from_pubkey` for a contact request. The envelope carries no identity
//!   of its own, on purpose: a key named in the payload would be a second,
//!   unauthenticated identity claim beside the authenticated one.
//! * A `username` here is a **claim to verify**, never a fact to display.
//!   ADR-0048 §3 has the client resolve it through discovery and show it
//!   only if the resolved key equals the sender's authenticated key. What
//!   this codec guarantees is narrower and still load-bearing: a claimed
//!   username is always canonical ([`crate::username`]), so a look-alike
//!   (`pаvel` with a Cyrillic `а`) is refused before verification is even
//!   attempted.
//! * `display_name` is unverifiable by construction and is shown as the
//!   person's own statement.
//! * `username_certificate` (v3, [ADR-0060](../../../docs/adr/0060-scoped-discovery.md)
//!   §5) is opaque here: discovery signs `(OPRF(username), account_id)` and a
//!   viewer verifies it (one OPRF evaluation plus the signature) in
//!   `lakko-attest`, which owns the layout. This codec checks only its
//!   length and that it never comes without a username.
//! * `grants` (v3, ADR-0060 §3 and §6) are reach keys the sender hands to
//!   the receivers of the envelope. Which classes travel where:
//!
//!   | Carrier | Grants |
//!   |---|---|
//!   | a group's profile | `group` (when the sender's setting lets that group find them) + `member` (always) |
//!   | a one-to-one conversation's profile | `contact` + `member` |
//!   | a channel member card | `channel` |
//!
//!   The codec admits any of the four classes in any profile (at most two,
//!   one per class); **where a grant is kept is the receiver's rule**, since
//!   only the receiver knows the carrier. In particular a receiver keeps a
//!   `contact` grant **only from the other participant of a one-to-one
//!   conversation** (resolved at receipt, C-68) and drops it from a group's
//!   profile or a channel card: a `contact` key in a group would hand every
//!   co-member the reach of a contact. Every grant carries the owner's
//!   mailbox binding (ADR-0078) and all grants of one profile must carry the
//!   same one; **the receiver verifies it against the sender's account key
//!   resolved at receipt** (`ReachGrant::mailbox_pk_verified`) before it
//!   keeps or uses a grant, since this codec does not know the sender. A
//!   grant is a **secret**
//!   ([`crate::reach::ReachGrant`]); the envelope is only ever sent inside an
//!   E2EE carrier.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       PROFILE_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || name_len || name || username_len || username
//!      └──── 17 bytes, ASCII ────┘ └─ 0x03 ─┘ └── 8 B, BE ───┘ └── 1 B ──┘ └ .. ┘ └──── 1 B ────┘ └── .. ──┘
//!
//!       || account_id_len || account_id || cert_len || cert || grant_count || grants
//!          └─ 1 B: 0|16 ─┘ └── .. ────┘  └ 1 B: 0|153 ┘ └ .. ┘ └── 1 B ───┘ └ 134 B each ┘
//!
//!       || avatar_len || avatar
//!          └ 1 B: 0|84|170 ┘
//!
//!   avatar   = still_ref | still_ref || video_ref || duration_ms(2 B, BE)
//!   *_ref    = blob_id(32) || key(32) || nonce(12) || size(4 B, BE) || width(2 B, BE) || height(2 B, BE)
//!
//!                     "lakko/profile/v1\x00"
//! ```
//!
//! A length of `0` means "no name" / "no username" / "no account id" / "no
//! certificate". Total length is between [`PROFILE_ENVELOPE_MIN_LEN`] and
//! [`PROFILE_ENVELOPE_MAX_LEN`]. v4 (ADR-0084, the profile photo) is a flag
//! day: v1, v2 and v3 are refused as [`ProfileError::UnknownVersion`].
//!
//! ## Bounds, and why a violation rejects the whole envelope
//!
//! * `display_name`: valid UTF-8, trimmed, 1 to [`DISPLAY_NAME_MAX_CHARS`]
//!   Unicode scalar values and at most [`DISPLAY_NAME_MAX_BYTES`] bytes, no
//!   line break, no control character (general category `Cc`) and no format
//!   character (`Cf`) — see [`DisplayNameError`].
//! * `username`: canonical per [`crate::username::check_canonical_username`].
//! * `username_certificate`: exactly [`USERNAME_CERT_LEN`] bytes, and only
//!   beside a username.
//! * `grants`: at most [`MAX_PROFILE_GRANTS`], no two of one class, and only
//!   of a class a profile may carry ([`is_profile_grant_class`]).
//! * `avatar` (ADR-0084): a reference to an encrypted still (a square JPEG,
//!   1..=[`AVATAR_STILL_MAX_PX`] px, 1..=[`AVATAR_STILL_MAX_BYTES`] bytes)
//!   and optionally to an encrypted video (a square MP4,
//!   1..=[`AVATAR_VIDEO_MAX_PX`] px, 1..=[`AVATAR_VIDEO_MAX_BYTES`] bytes,
//!   1..=[`AVATAR_VIDEO_MAX_DURATION_MS`] ms). The key of each reference is
//!   a secret like a grant: the envelope only travels inside an E2EE
//!   carrier. That the referenced bytes are what the slot says is the
//!   receiver's check after it downloads them; this codec bounds only the
//!   reference.
//!
//! An envelope violating any bound is **rejected as a whole**. Unlike
//! attachment metadata, a profile has no remainder worth keeping: a valid
//! username beside a name smuggling a right-to-left override is not a
//! profile anyone should render half of.
//!
//! **`compose` refuses exactly what `decode` refuses.** Both run the same
//! [`Profile::validate`], so a conforming sender cannot emit an envelope a
//! conforming receiver drops.
//!
//! ### The byte cap is 255, not 256
//!
//! ADR-0048 §1 says "at most 64 characters and 256 bytes", but the same
//! section fixes `name_len` at one byte, whose ceiling is 255. The one-byte
//! length wins, because it is the wire: [`DISPLAY_NAME_MAX_BYTES`] is 255.
//! The practical effect is that 64 characters of four-byte scalars (a name
//! made entirely of emoji or supplementary-plane CJK) is refused by one
//! byte; 63 such characters fit.
//!
//! ### The format-character table
//!
//! Rust's standard library answers `Cc` (`char::is_control`) but not `Cf`,
//! and this crate adds no Unicode-tables dependency for one category. So
//! [`FORMAT_CHAR_RANGES`] enumerates `Cf` explicitly. It was checked against
//! Python's `unicodedata` at Unicode 14.0, and widened by one known later
//! addition (the Egyptian hieroglyph format controls U+13439..U+1343F, added
//! in Unicode 15.0 — taken from the Unicode 15.0 release, not re-verified
//! against a table in this repository). A `Cf` code point assigned after
//! that would be admitted until the table is updated.
//!
//! **Three `Cf` exceptions, because refusing them breaks ordinary names and
//! buys nothing.** ZERO WIDTH NON-JOINER (U+200C) and ZERO WIDTH JOINER
//! (U+200D) are admitted anywhere: ZWJ builds emoji sequences (family,
//! profession-with-gender) and ZWNJ is required by Persian and several Indic
//! spellings. The tag characters U+E0020..U+E007F are admitted only as a
//! subdivision-flag sequence — directly after WAVING BLACK FLAG U+1F3F4 and
//! ending in CANCEL TAG U+E007F — so they cannot hide inside ordinary text.
//! An invisible joiner can make two names render alike, but a display name
//! is neither unique nor trusted (ADR-0048 §3: the verified username and the
//! safety number disambiguate), so that costs nothing an attacker could not
//! already get by typing the same name twice. What stays refused is what can
//! mislead beyond a collision: the bidi controls, which reorder how
//! neighbouring text renders — a name ending in U+202E can visually swallow
//! the `· @username` shown beside it — plus the invisible spacers (ZWSP,
//! WORD JOINER, BOM, SOFT HYPHEN) that no name needs.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it, since
//! [`crate::incoming_envelope`] dispatches on a byte-prefix match.
//!
//! ## Order
//!
//! [`profile_is_newer`]: a receiver keeps the profile with the greater
//! `updated_at_ms` **per signature key**; equal is not newer. The stamp is
//! the sender's own clock, which is acceptable here and nowhere near a
//! security decision: a sender with a backwards clock only delays its own
//! update, and a sender that stamps the far future only prevents its own
//! later updates from landing — it can do nothing to anyone else's profile,
//! because profiles are keyed by the authenticated sender.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::avatar::{encode_avatar_field, read_avatar_field, AvatarError};
use crate::reach::{ReachClass, ReachError, ReachGrant, REACH_GRANT_LEN};

/// The avatar field moved to [`crate::avatar`] (ADR-0087 section 18), shared
/// with the group and channel carriers; re-exported here so the profile's
/// public paths and its wire are unchanged.
pub use crate::avatar::{
    Avatar, AvatarRef, AvatarVideo, AVATAR_REF_LEN, AVATAR_STILL_MAX_BYTES, AVATAR_STILL_MAX_PX,
    AVATAR_STILL_ONLY_LEN, AVATAR_VIDEO_MAX_BYTES, AVATAR_VIDEO_MAX_DURATION_MS,
    AVATAR_VIDEO_MAX_PX, AVATAR_WITH_VIDEO_LEN,
};
use crate::username::{check_canonical_username, UsernameError, USERNAME_MAX_LEN};

/// 17-byte domain separator prepended to the profile payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with, mirroring the sibling envelopes. The printable
/// prefix is `lakko/profile/v1`.
///
/// **The `v1` names the envelope FAMILY, not the schema version.** These
/// bytes are frozen: [`crate::incoming_envelope`] dispatches on this exact
/// prefix. The in-payload version byte is the knob that moves.
pub const PROFILE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/profile/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const PROFILE_VERSION_V1: u8 = 0x01;

/// Schema version carried in the first payload byte. v2 (2026-09-19)
/// appended the optional account id after the username; v1 is refused
/// (`UnknownVersion`), since nothing was deployed and a v1 envelope
/// cannot name an account for the call roster.
pub const PROFILE_VERSION_V2: u8 = 0x02;

/// Schema version carried in the first payload byte since ADR-0060
/// (2026-09-28): the username certificate and the reach grants follow the
/// account id. v1 and v2 are refused (`UnknownVersion`) — a flag day, since
/// nothing is deployed.
pub const PROFILE_VERSION_V3: u8 = 0x03;

/// Schema version carried in the first payload byte since ADR-0084: v3
/// plus an optional avatar after the grants. The only version decoded.
pub const PROFILE_VERSION_V4: u8 = 0x04;

/// Length of the discovery-signed username certificate a profile may carry
/// (ADR-0060 §5). Opaque to this crate: `lakko-attest` defines the layout
/// with this same length, and verification (one OPRF evaluation plus the
/// signature) happens there, never here.
pub const USERNAME_CERT_LEN: usize = 153;

/// Most reach grants one profile may carry (ADR-0060 §3, §6): the `group`
/// and the `member` key of a group, the `contact` and the `member` key of a
/// one-to-one conversation, or the `channel` key of a channel.
pub const MAX_PROFILE_GRANTS: usize = 2;

/// Length of the account id the envelope may carry: the 16-byte auth
/// account id, the value the SFU's per-call peer tag is derived from
/// (ADR-0021), so a call tile can be matched to this profile.
pub const ACCOUNT_ID_LEN: usize = 16;

/// Longest admissible display name, in Unicode scalar values (not grapheme
/// clusters — a flag or a skin-toned emoji counts as more than one).
pub const DISPLAY_NAME_MAX_CHARS: usize = 64;

/// Longest admissible display name, in UTF-8 bytes. The one-byte `name_len`
/// field's ceiling — see the module docs for why this is 255 and not the
/// ADR's 256.
pub const DISPLAY_NAME_MAX_BYTES: usize = u8::MAX as usize;

/// Bytes after the separator that every envelope carries: version,
/// `updated_at_ms`, `name_len`, `username_len`, `account_id_len`,
/// `cert_len`, `grant_count`, `avatar_len`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1 + 1 + 1 + 1 + 1 + 1;

/// Shortest well-formed envelope: no name, no username.
pub const PROFILE_ENVELOPE_MIN_LEN: usize = PROFILE_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Longest well-formed envelope: a maximal name, a maximal username, an
/// account id, a certificate, the most grants and an avatar with a video.
pub const PROFILE_ENVELOPE_MAX_LEN: usize = PROFILE_ENVELOPE_MIN_LEN
    + DISPLAY_NAME_MAX_BYTES
    + USERNAME_MAX_LEN
    + ACCOUNT_ID_LEN
    + USERNAME_CERT_LEN
    + MAX_PROFILE_GRANTS * REACH_GRANT_LEN
    + AVATAR_WITH_VIDEO_LEN;

/// `true` for the reach classes a profile may hand out (ADR-0060 §3, §6):
/// `contact` (in a one-to-one conversation's profile), `group`, `member`,
/// `channel`. The others travel elsewhere — a `link` key inside an invite
/// capability, a `reply` key inside a contact request or a `/g/` join
/// attribution, a `fof` key inside an introduction record.
///
/// The codec does not know the carrier, so it cannot say which of these a
/// given profile should carry; the receiver keeps a `contact` grant only
/// from the other participant of a one-to-one conversation (module docs).
#[must_use]
pub const fn is_profile_grant_class(class: ReachClass) -> bool {
    matches!(
        class,
        ReachClass::Contact | ReachClass::Group | ReachClass::Member | ReachClass::Channel
    )
}

/// Unicode general category `Cf` (format), as inclusive code-point ranges.
///
/// See the module docs for provenance, and for the characters this refuses
/// that a reader might not expect (ZWJ, ZWNJ, tag characters).
pub const FORMAT_CHAR_RANGES: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),   // SOFT HYPHEN
    (0x0600, 0x0605),   // ARABIC NUMBER SIGN .. ARABIC NUMBER MARK ABOVE
    (0x061C, 0x061C),   // ARABIC LETTER MARK (bidi)
    (0x06DD, 0x06DD),   // ARABIC END OF AYAH
    (0x070F, 0x070F),   // SYRIAC ABBREVIATION MARK
    (0x0890, 0x0891),   // ARABIC POUND / PIASTRE MARK ABOVE
    (0x08E2, 0x08E2),   // ARABIC DISPUTED END OF AYAH
    (0x180E, 0x180E),   // MONGOLIAN VOWEL SEPARATOR
    (0x200B, 0x200F),   // ZWSP, ZWNJ, ZWJ, LRM, RLM
    (0x202A, 0x202E),   // LRE, RLE, PDF, LRO, RLO
    (0x2060, 0x2064),   // WORD JOINER .. INVISIBLE PLUS
    (0x2066, 0x206F),   // LRI, RLI, FSI, PDI, deprecated format controls
    (0xFEFF, 0xFEFF),   // ZERO WIDTH NO-BREAK SPACE (BOM)
    (0xFFF9, 0xFFFB),   // INTERLINEAR ANNOTATION ANCHOR .. TERMINATOR
    (0x110BD, 0x110BD), // KAITHI NUMBER SIGN
    (0x110CD, 0x110CD), // KAITHI NUMBER SIGN ABOVE
    (0x13430, 0x1343F), // EGYPTIAN HIEROGLYPH format controls (..38 in 14.0, ..3F in 15.0)
    (0x1BCA0, 0x1BCA3), // SHORTHAND FORMAT LETTER OVERLAP .. UP STEP
    (0x1D173, 0x1D17A), // MUSICAL SYMBOL BEGIN BEAM .. END PHRASE
    (0xE0001, 0xE0001), // LANGUAGE TAG
    (0xE0020, 0xE007F), // TAG SPACE .. CANCEL TAG
];

/// `true` for a Unicode format character (general category `Cf`), per
/// [`FORMAT_CHAR_RANGES`].
#[must_use]
pub fn is_format_char(c: char) -> bool {
    let cp = u32::from(c);
    FORMAT_CHAR_RANGES
        .iter()
        .any(|&(lo, hi)| (lo..=hi).contains(&cp))
}

/// `true` for a character that breaks a line: every `Cc` line terminator
/// Unicode names (LF, VT, FF, CR, NEL) plus LINE SEPARATOR and PARAGRAPH
/// SEPARATOR, which are not `Cc` and would otherwise slip past the control
/// check.
const fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// A person's self-stated profile.
///
/// **Every field is the sender's claim.** See the module docs: the receiver
/// binds it to the authenticated sender key, and verifies `username`
/// through discovery before showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The display name, or `None` for "no name". When `Some`, it must
    /// satisfy [`check_display_name`] — in particular it is already trimmed
    /// and never empty (`Some("")` is refused; "no name" is `None`). Use
    /// [`normalize_display_name`] on user input.
    pub display_name: Option<String>,
    /// The claimed username in canonical form, or `None` for "no
    /// username". When `Some`, it must satisfy
    /// [`crate::username::check_canonical_username`].
    pub username: Option<String>,
    /// Sender-clock UNIX milliseconds at which this profile was set. Orders
    /// profiles from the same sender via [`profile_is_newer`].
    pub updated_at_ms: u64,
    /// The sender's auth account id (16 bytes), the value a per-call peer
    /// tag is derived from (ADR-0021): what lets a call tile be matched to
    /// a group member who is not a contact. Like every other field it is
    /// the sender's claim; a receiver keys nothing security-relevant on it
    /// and stores it under the sender's account key resolved at receipt
    /// (C-68). `None` when the sender did not say (a guest, or a device
    /// that has no account id at hand).
    pub account_id: Option<[u8; ACCOUNT_ID_LEN]>,
    /// The discovery-signed username certificate (ADR-0060 §5), exactly
    /// [`USERNAME_CERT_LEN`] bytes, opaque here. Requires `username`.
    pub username_certificate: Option<Vec<u8>>,
    /// Reach grants handed to the receivers (ADR-0060 §3): at most
    /// [`MAX_PROFILE_GRANTS`], one per class, of a class
    /// [`is_profile_grant_class`] admits. Secret — see the module docs.
    pub grants: Vec<ReachGrant>,
    /// The profile photo (ADR-0084), or `None` for "no photo". Its keys are
    /// secret — see the module docs.
    pub avatar: Option<Avatar>,
}

impl Profile {
    /// Check every bound. The single rule both [`compose_profile`] and
    /// [`decode_profile`] run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// * [`ProfileError::DisplayName`] — the display name violates a bound.
    /// * [`ProfileError::Username`] — the username is not canonical.
    /// * [`ProfileError::CertificateLength`] /
    ///   [`ProfileError::CertificateWithoutUsername`] — the certificate is
    ///   the wrong length, or there is no username for it to certify.
    /// * [`ProfileError::TooManyGrants`] / [`ProfileError::DuplicateGrantClass`]
    ///   / [`ProfileError::GrantClassNotAllowed`] /
    ///   [`ProfileError::GrantBindingsDiffer`] — a grant bound.
    pub fn validate(&self) -> CoreResult<(), ProfileError> {
        if let Some(name) = &self.display_name {
            check_display_name(name).map_err(ProfileError::DisplayName)?;
        }
        if let Some(username) = &self.username {
            check_canonical_username(username).map_err(ProfileError::Username)?;
        }
        if let Some(cert) = &self.username_certificate {
            if cert.len() != USERNAME_CERT_LEN {
                return Err(ProfileError::CertificateLength { got: cert.len() });
            }
            if self.username.is_none() {
                return Err(ProfileError::CertificateWithoutUsername);
            }
        }
        if self.grants.len() > MAX_PROFILE_GRANTS {
            return Err(ProfileError::TooManyGrants {
                count: self.grants.len(),
            });
        }
        for (i, grant) in self.grants.iter().enumerate() {
            if !is_profile_grant_class(grant.class) {
                return Err(ProfileError::GrantClassNotAllowed(grant.class.to_byte()));
            }
            if self.grants[..i].iter().any(|g| g.class == grant.class) {
                return Err(ProfileError::DuplicateGrantClass(grant.class.to_byte()));
            }
            // One profile, one owner, one mailbox (ADR-0078).
            if self.grants[..i].iter().any(|g| g.binding != grant.binding) {
                return Err(ProfileError::GrantBindingsDiffer);
            }
        }
        if let Some(avatar) = &self.avatar {
            avatar.validate().map_err(ProfileError::from)?;
        }
        Ok(())
    }
}

/// Why a display name was refused.
///
/// A reason code: no variant carries the offending character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DisplayNameError {
    /// `Some("")` — "no name" is spelled `None`. Only reachable from a
    /// struct built by hand; the wire encodes an empty name as `None`.
    #[error("display name is empty")]
    Empty,
    /// Leading or trailing whitespace. [`normalize_display_name`] trims.
    #[error("display name has leading or trailing whitespace")]
    NotTrimmed,
    /// Contains a line break.
    #[error("display name contains a line break")]
    LineBreak,
    /// Contains a control (`Cc`) or format (`Cf`) character — which
    /// includes the right-to-left override and the zero-width characters.
    #[error("display name contains a control or format character")]
    ControlOrFormatCharacter,
    /// More than [`DISPLAY_NAME_MAX_CHARS`] Unicode scalar values.
    #[error("display name is longer than {max_chars} characters")]
    TooLong {
        /// Mirror of [`DISPLAY_NAME_MAX_CHARS`].
        max_chars: usize,
    },
    /// More than [`DISPLAY_NAME_MAX_BYTES`] UTF-8 bytes.
    #[error("display name is longer than {max_bytes} bytes")]
    TooManyBytes {
        /// Mirror of [`DISPLAY_NAME_MAX_BYTES`].
        max_bytes: usize,
    },
    /// The wire bytes are not valid UTF-8. Decode-only.
    #[error("display name is not valid UTF-8")]
    NotUtf8,
}

impl DisplayNameError {
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

/// Check that `name` is an admissible display name **as it stands** (no
/// trimming is applied).
///
/// Reasons are tested in a fixed order: empty, untrimmed, line break,
/// control / format character, then the two lengths.
///
/// # Errors
///
/// Returns the first [`DisplayNameError`] that applies.
pub fn check_display_name(name: &str) -> CoreResult<(), DisplayNameError> {
    if name.is_empty() {
        return Err(DisplayNameError::Empty);
    }
    if name.trim() != name {
        return Err(DisplayNameError::NotTrimmed);
    }
    if name.chars().any(is_line_break) {
        return Err(DisplayNameError::LineBreak);
    }
    if has_refused_control_or_format(name) {
        return Err(DisplayNameError::ControlOrFormatCharacter);
    }
    if name.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err(DisplayNameError::TooLong {
            max_chars: DISPLAY_NAME_MAX_CHARS,
        });
    }
    if name.len() > DISPLAY_NAME_MAX_BYTES {
        return Err(DisplayNameError::TooManyBytes {
            max_bytes: DISPLAY_NAME_MAX_BYTES,
        });
    }
    Ok(())
}

/// ZERO WIDTH NON-JOINER and ZERO WIDTH JOINER: `Cf`, admitted anywhere in a
/// display name (see the module doc).
const PERMITTED_JOINERS: [char; 2] = ['\u{200C}', '\u{200D}'];
/// WAVING BLACK FLAG, the base a subdivision-flag tag sequence follows.
const FLAG_BASE: char = '\u{1F3F4}';
/// CANCEL TAG, which terminates a subdivision-flag tag sequence.
const CANCEL_TAG: char = '\u{E007F}';

const fn is_tag_char(c: char) -> bool {
    matches!(c, '\u{E0020}'..='\u{E007F}')
}

/// `true` when `name` holds a control character, or a format character that
/// is neither a permitted joiner nor part of a well-formed subdivision-flag
/// tag sequence (`U+1F3F4`, one or more tags, ending in `U+E007F`).
fn has_refused_control_or_format(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == FLAG_BASE {
            // Consume a following tag run; it is admissible only if it ends
            // in CANCEL TAG and contains at least one tag before it.
            let mut j = i + 1;
            while j < chars.len() && is_tag_char(chars[j]) && chars[j] != CANCEL_TAG {
                j += 1;
            }
            if j < chars.len() && chars[j] == CANCEL_TAG && j > i + 1 {
                i = j + 1;
                continue;
            }
        } else if c.is_control() || (is_format_char(c) && !PERMITTED_JOINERS.contains(&c)) {
            return true;
        }
        i += 1;
    }
    false
}

/// Turn what a person typed into the display name to store and send.
///
/// Trims, maps "nothing left" to `Ok(None)` (ADR-0048: empty = no name),
/// and otherwise runs [`check_display_name`] on the trimmed text.
///
/// # Errors
///
/// Returns the [`DisplayNameError`] the trimmed text violates.
pub fn normalize_display_name(input: &str) -> CoreResult<Option<String>, DisplayNameError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    check_display_name(trimmed)?;
    Ok(Some(trimmed.to_string()))
}

/// `true` when a profile stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` from the **same signature key**.
///
/// Strictly greater wins; **equal is not newer**, so a re-broadcast of the
/// profile a receiver already holds is not an update.
#[must_use]
pub const fn profile_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a profile envelope.
///
/// The output is the **inner** plaintext handed to the MLS
/// application-message send path, or placed in a contact request's sealed
/// envelope by [`crate::request::compose`].
///
/// # Errors
///
/// Exactly the errors [`Profile::validate`] returns — `compose` refuses what
/// [`decode_profile`] would refuse.
pub fn compose_profile(profile: &Profile) -> CoreResult<Vec<u8>, ProfileError> {
    profile.validate()?;
    let name = profile.display_name.as_deref().unwrap_or("").as_bytes();
    let username = profile.username.as_deref().unwrap_or("").as_bytes();
    // `validate` bounded both lengths below `u8::MAX`; the conversions are
    // re-checked rather than cast so a future bound change cannot truncate.
    let name_len = u8::try_from(name.len()).map_err(|_| {
        ProfileError::DisplayName(DisplayNameError::TooManyBytes {
            max_bytes: DISPLAY_NAME_MAX_BYTES,
        })
    })?;
    let username_len = u8::try_from(username.len()).map_err(|_| {
        ProfileError::Username(UsernameError::TooLong {
            max: USERNAME_MAX_LEN,
        })
    })?;

    let mut out = Vec::with_capacity(PROFILE_ENVELOPE_MIN_LEN + name.len() + username.len());
    out.extend_from_slice(PROFILE_ENVELOPE_DOMAIN_V1);
    out.push(PROFILE_VERSION_V4);
    out.extend_from_slice(&profile.updated_at_ms.to_be_bytes());
    out.push(name_len);
    out.extend_from_slice(name);
    out.push(username_len);
    out.extend_from_slice(username);
    match &profile.account_id {
        Some(id) => {
            out.push(u8::try_from(ACCOUNT_ID_LEN).unwrap_or(u8::MAX));
            out.extend_from_slice(id);
        },
        None => out.push(0),
    }
    match &profile.username_certificate {
        Some(cert) => {
            // `validate` pinned the length to USERNAME_CERT_LEN (< 256).
            out.push(
                u8::try_from(cert.len())
                    .map_err(|_| ProfileError::CertificateLength { got: cert.len() })?,
            );
            out.extend_from_slice(cert);
        },
        None => out.push(0),
    }
    // `validate` bounded the count to MAX_PROFILE_GRANTS.
    out.push(
        u8::try_from(profile.grants.len()).map_err(|_| ProfileError::TooManyGrants {
            count: profile.grants.len(),
        })?,
    );
    for grant in &profile.grants {
        out.extend_from_slice(&grant.encode());
    }
    encode_avatar_field(profile.avatar.as_ref(), &mut out);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(profile))` — the domain separator matched and the envelope
///   is well-formed and within every bound.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed or out of
///   bounds. The caller drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`ProfileError::Truncated`] — the payload ends before a declared field.
/// * [`ProfileError::UnknownVersion`] — the version byte is not
///   [`PROFILE_VERSION_V4`].
/// * [`ProfileError::TrailingBytes`] — bytes remain after the avatar.
/// * [`ProfileError::AvatarLength`] — the avatar length byte is not 0, 84
///   or 170.
/// * [`ProfileError::AccountIdLength`] / [`ProfileError::CertificateLength`]
///   — a length byte that is neither 0 nor the field's one length.
/// * [`ProfileError::Grant`] — a grant's class byte is not a class.
/// * [`ProfileError::DisplayName`] / [`ProfileError::Username`] — a field is
///   out of bounds.
pub fn try_unwrap_profile(plaintext: &[u8]) -> CoreResult<Option<Profile>, ProfileError> {
    let Some(payload) = plaintext.strip_prefix(PROFILE_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;

    let version = take_u8(&mut cur)?;
    if version != PROFILE_VERSION_V4 {
        return Err(ProfileError::UnknownVersion(version));
    }
    let updated_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| ProfileError::Truncated)?;
    let updated_at_ms = u64::from_be_bytes(updated_at_bytes);

    let name_len = usize::from(take_u8(&mut cur)?);
    let name_bytes = take(&mut cur, name_len)?;
    let username_len = usize::from(take_u8(&mut cur)?);
    let username_bytes = take(&mut cur, username_len)?;
    let account_id_len = usize::from(take_u8(&mut cur)?);
    let account_id = match account_id_len {
        0 => None,
        ACCOUNT_ID_LEN => {
            let bytes: [u8; ACCOUNT_ID_LEN] = take(&mut cur, ACCOUNT_ID_LEN)?
                .try_into()
                .map_err(|_| ProfileError::Truncated)?;
            Some(bytes)
        },
        got => return Err(ProfileError::AccountIdLength { got }),
    };
    let cert_len = usize::from(take_u8(&mut cur)?);
    let username_certificate = match cert_len {
        0 => None,
        USERNAME_CERT_LEN => Some(take(&mut cur, USERNAME_CERT_LEN)?.to_vec()),
        got => return Err(ProfileError::CertificateLength { got }),
    };
    let grant_count = usize::from(take_u8(&mut cur)?);
    if grant_count > MAX_PROFILE_GRANTS {
        return Err(ProfileError::TooManyGrants { count: grant_count });
    }
    let mut grants = Vec::with_capacity(grant_count);
    for _ in 0..grant_count {
        let grant =
            ReachGrant::decode(take(&mut cur, REACH_GRANT_LEN)?).map_err(ProfileError::Grant)?;
        grants.push(grant);
    }
    let avatar = read_avatar_field(&mut cur).map_err(ProfileError::from)?;
    if !cur.is_empty() {
        return Err(ProfileError::TrailingBytes { count: cur.len() });
    }

    let display_name = if name_bytes.is_empty() {
        None
    } else {
        let name = core::str::from_utf8(name_bytes)
            .map_err(|_| ProfileError::DisplayName(DisplayNameError::NotUtf8))?;
        Some(name.to_string())
    };
    let username = if username_bytes.is_empty() {
        None
    } else {
        // Non-UTF-8 bytes are by definition outside `[a-z0-9_]`.
        let username = core::str::from_utf8(username_bytes)
            .map_err(|_| ProfileError::Username(UsernameError::InvalidCharacters))?;
        Some(username.to_string())
    };

    let profile = Profile {
        display_name,
        username,
        updated_at_ms,
        account_id,
        username_certificate,
        grants,
        avatar,
    };
    profile.validate()?;
    Ok(Some(profile))
}

/// Strict standalone decoder: a non-profile plaintext is a hard error.
///
/// # Errors
///
/// * [`ProfileError::WrongDomain`] — `bytes` does not start with
///   [`PROFILE_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_profile`] returns.
pub fn decode_profile(bytes: &[u8]) -> CoreResult<Profile, ProfileError> {
    try_unwrap_profile(bytes)?.ok_or(ProfileError::WrongDomain)
}

/// Why a profile envelope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProfileError {
    /// The plaintext does not carry the profile domain separator. Only from
    /// [`decode_profile`]; [`try_unwrap_profile`] returns `Ok(None)`.
    #[error("plaintext does not carry the profile domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("profile envelope is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("profile envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the last field.
        count: usize,
    },
    /// The version byte is not [`PROFILE_VERSION_V4`].
    #[error("profile envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The display name violates a bound.
    #[error("invalid display name: {0}")]
    DisplayName(DisplayNameError),
    /// The username is not canonical.
    #[error("invalid username: {0}")]
    Username(UsernameError),
    /// The account id length byte is neither 0 nor 16.
    #[error("profile account id is {got} bytes, expected 0 or 16")]
    AccountIdLength {
        /// The length byte as read.
        got: usize,
    },
    /// The username certificate is neither absent nor exactly
    /// [`USERNAME_CERT_LEN`] bytes.
    #[error("profile username certificate is {got} bytes, expected 0 or {USERNAME_CERT_LEN}")]
    CertificateLength {
        /// The length as read.
        got: usize,
    },
    /// A username certificate without a username to certify.
    #[error("profile carries a username certificate but no username")]
    CertificateWithoutUsername,
    /// More than [`MAX_PROFILE_GRANTS`] grants.
    #[error("profile carries {count} reach grants, at most {MAX_PROFILE_GRANTS} allowed")]
    TooManyGrants {
        /// The count as read.
        count: usize,
    },
    /// Two grants of one class (the class byte).
    #[error("profile carries two reach grants of class {0:#04x}")]
    DuplicateGrantClass(u8),
    /// A grant of a class a profile may not carry (the class byte).
    #[error("profile may not carry a reach grant of class {0:#04x}")]
    GrantClassNotAllowed(u8),
    /// A grant failed to decode.
    #[error("invalid reach grant: {0}")]
    Grant(ReachError),
    /// Two grants name different mailboxes (ADR-0078): every grant of one
    /// profile writes to its one owner's mailbox.
    #[error("profile reach grants name different mailboxes")]
    GrantBindingsDiffer,
    /// The avatar length byte is not 0, [`AVATAR_STILL_ONLY_LEN`] or
    /// [`AVATAR_WITH_VIDEO_LEN`].
    #[error("profile avatar is {got} bytes, expected 0, 84 or 170")]
    AvatarLength {
        /// The length byte as read.
        got: usize,
    },
    /// An avatar image or video is not square.
    #[error("profile avatar is not square")]
    AvatarNotSquare,
    /// An avatar side is 0 or above its slot's limit.
    #[error("profile avatar dimensions are out of bounds")]
    AvatarDimensions,
    /// An avatar blob size is 0 or above its slot's limit.
    #[error("profile avatar size is out of bounds")]
    AvatarSize,
    /// The avatar video duration is 0 or above the limit.
    #[error("profile avatar video duration is out of bounds")]
    AvatarDuration,
}

impl ProfileError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    ///
    /// Field errors are namespaced (`displayName.tooLong`,
    /// `username.invalidCharacters`) so one code set covers both fields.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::DisplayName(e) => format!("displayName.{}", e.code()),
            Self::Username(e) => format!("username.{}", e.code()),
            Self::AccountIdLength { .. } => "accountId.length".to_string(),
            Self::CertificateLength { .. } => "certificate.length".to_string(),
            Self::CertificateWithoutUsername => "certificate.withoutUsername".to_string(),
            Self::TooManyGrants { .. } => "grants.tooMany".to_string(),
            Self::DuplicateGrantClass(_) => "grants.duplicateClass".to_string(),
            Self::GrantClassNotAllowed(_) => "grants.classNotAllowed".to_string(),
            Self::Grant(e) => format!("grants.{}", e.code()),
            Self::GrantBindingsDiffer => "grants.bindingsDiffer".to_string(),
            Self::AvatarLength { .. } => "avatar.length".to_string(),
            Self::AvatarNotSquare => "avatar.notSquare".to_string(),
            Self::AvatarDimensions => "avatar.dimensions".to_string(),
            Self::AvatarSize => "avatar.size".to_string(),
            Self::AvatarDuration => "avatar.duration".to_string(),
        }
    }
}

impl From<AvatarError> for ProfileError {
    fn from(e: AvatarError) -> Self {
        match e {
            AvatarError::Truncated => Self::Truncated,
            AvatarError::Length { got } => Self::AvatarLength { got },
            AvatarError::NotSquare => Self::AvatarNotSquare,
            AvatarError::Dimensions => Self::AvatarDimensions,
            AvatarError::Size => Self::AvatarSize,
            AvatarError::Duration => Self::AvatarDuration,
        }
    }
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, ProfileError> {
    let (&first, rest) = cur.split_first().ok_or(ProfileError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], ProfileError> {
    if cur.len() < n {
        return Err(ProfileError::Truncated);
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

    fn profile(name: Option<&str>, username: Option<&str>, at: u64) -> Profile {
        Profile {
            display_name: name.map(str::to_string),
            username: username.map(str::to_string),
            updated_at_ms: at,
            account_id: None,
            username_certificate: None,
            grants: Vec::new(),
            avatar: None,
        }
    }

    fn grant(class: ReachClass, byte: u8) -> ReachGrant {
        ReachGrant {
            class,
            version: 1,
            seed: lakko_crypto::SecretBytes::new([byte; 32]),
            binding: crate::mailbox::MailboxBinding::for_account_seed(&[0x77; 32]),
        }
    }

    fn cert(byte: u8) -> Vec<u8> {
        vec![byte; USERNAME_CERT_LEN]
    }

    fn avatar_ref(byte: u8, side: u16, bytes: u32) -> AvatarRef {
        AvatarRef {
            blob_id: [byte; 32],
            key: [byte.wrapping_add(1); 32],
            nonce: [byte.wrapping_add(2); 12],
            size: bytes,
            width: side,
            height: side,
        }
    }

    fn still_avatar() -> Avatar {
        Avatar {
            still: avatar_ref(0x10, 640, 40_000),
            video: None,
        }
    }

    fn full_avatar() -> Avatar {
        Avatar {
            still: avatar_ref(0x20, 480, 30_000),
            video: Some(AvatarVideo {
                reference: avatar_ref(0x30, 480, 1_500_000),
                duration_ms: 9_500,
            }),
        }
    }

    // --- avatar (ADR-0084) ---------------------------------------------------

    #[test]
    fn avatar_round_trips_with_and_without_video() {
        for avatar in [still_avatar(), full_avatar()] {
            let mut p = profile(Some("Alice"), Some("alice"), 3);
            p.avatar = Some(avatar.clone());
            let wire = compose_profile(&p).unwrap();
            let expected_avatar_len = if avatar.video.is_some() { 170 } else { 84 };
            assert_eq!(wire.len(), 32 + 5 + 5 + expected_avatar_len);
            assert_eq!(
                wire[wire.len() - expected_avatar_len - 1],
                u8::try_from(expected_avatar_len).unwrap()
            );
            assert_eq!(decode_profile(&wire).unwrap(), p);
        }
    }

    #[test]
    fn avatar_layout_is_pinned() {
        let mut p = profile(None, None, 0);
        p.avatar = Some(Avatar {
            still: AvatarRef {
                blob_id: [1; 32],
                key: [2; 32],
                nonce: [3; 12],
                size: 0x0001_0203,
                width: 0x0140,
                height: 0x0140,
            },
            video: None,
        });
        let wire = compose_profile(&p).unwrap();
        let at = PROFILE_ENVELOPE_MIN_LEN - 1;
        assert_eq!(wire[at], 84);
        let r = &wire[at + 1..];
        assert_eq!(&r[..32], &[1; 32]);
        assert_eq!(&r[32..64], &[2; 32]);
        assert_eq!(&r[64..76], &[3; 12]);
        assert_eq!(&r[76..80], &[0, 1, 2, 3]);
        assert_eq!(&r[80..84], &[0x01, 0x40, 0x01, 0x40]);
    }

    #[test]
    fn avatar_bounds_are_refused_by_compose_and_decode_alike() {
        let mut too_big = still_avatar();
        too_big.still.size = AVATAR_STILL_MAX_BYTES + 1;
        let mut zero_size = still_avatar();
        zero_size.still.size = 0;
        let mut not_square = still_avatar();
        not_square.still.height = 639;
        let mut too_wide = still_avatar();
        too_wide.still.width = 641;
        too_wide.still.height = 641;
        let mut zero_px = still_avatar();
        zero_px.still.width = 0;
        zero_px.still.height = 0;
        let mut video_too_wide = full_avatar();
        if let Some(v) = video_too_wide.video.as_mut() {
            v.reference.width = 481;
            v.reference.height = 481;
        }
        let mut video_too_big = full_avatar();
        if let Some(v) = video_too_big.video.as_mut() {
            v.reference.size = AVATAR_VIDEO_MAX_BYTES + 1;
        }
        let mut too_long = full_avatar();
        if let Some(v) = too_long.video.as_mut() {
            v.duration_ms = AVATAR_VIDEO_MAX_DURATION_MS + 1;
        }
        let mut zero_long = full_avatar();
        if let Some(v) = zero_long.video.as_mut() {
            v.duration_ms = 0;
        }
        let cases = [
            (too_big, ProfileError::AvatarSize),
            (zero_size, ProfileError::AvatarSize),
            (not_square, ProfileError::AvatarNotSquare),
            (too_wide, ProfileError::AvatarDimensions),
            (zero_px, ProfileError::AvatarDimensions),
            (video_too_wide, ProfileError::AvatarDimensions),
            (video_too_big, ProfileError::AvatarSize),
            (too_long, ProfileError::AvatarDuration),
            (zero_long, ProfileError::AvatarDuration),
        ];
        for (avatar, why) in cases {
            let mut p = profile(Some("Alice"), None, 5);
            p.avatar = Some(avatar.clone());
            assert_eq!(compose_profile(&p), Err(why), "{avatar:?}");
            // The same avatar on the wire, hand-built past compose.
            let mut wire = raw(4, 5, b"Alice", b"");
            wire.pop();
            wire.push(u8::try_from(avatar.encoded_len()).unwrap());
            avatar.still.encode_into(&mut wire);
            if let Some(v) = &avatar.video {
                v.reference.encode_into(&mut wire);
                wire.extend_from_slice(&v.duration_ms.to_be_bytes());
            }
            assert_eq!(decode_profile(&wire), Err(why), "{avatar:?}");
        }
    }

    #[test]
    fn an_avatar_length_other_than_0_84_or_170_is_refused() {
        for got in [1_u8, 83, 85, 169, 171, 255] {
            let mut wire = raw(4, 5, b"", b"");
            wire.pop();
            wire.push(got);
            wire.extend(core::iter::repeat_n(0_u8, usize::from(got)));
            assert_eq!(
                decode_profile(&wire),
                Err(ProfileError::AvatarLength {
                    got: usize::from(got)
                })
            );
        }
    }

    #[test]
    fn every_truncation_point_of_an_avatar_envelope_is_refused() {
        let mut p = profile(Some("Alice"), Some("alice"), 9);
        p.avatar = Some(full_avatar());
        let wire = compose_profile(&p).unwrap();
        for cut in PROFILE_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_profile(&wire[..cut]),
                Err(ProfileError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn debug_of_an_avatar_never_prints_its_key() {
        let mut p = profile(None, None, 1);
        p.avatar = Some(Avatar {
            still: AvatarRef {
                blob_id: [0xA1; 32],
                key: [0xEE; 32],
                nonce: [0xA3; 12],
                size: 10,
                width: 10,
                height: 10,
            },
            video: None,
        });
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("238"), "{dbg}");
    }

    #[test]
    fn a_v3_envelope_is_refused() {
        assert_eq!(
            decode_profile(&raw(PROFILE_VERSION_V3, 0, b"", b"")),
            Err(ProfileError::UnknownVersion(PROFILE_VERSION_V3))
        );
    }

    /// Hand-build an envelope with arbitrary (possibly invalid) fields,
    /// bypassing `compose`'s validation — so decode's own gates are tested.
    fn raw(version: u8, at: u64, name: &[u8], username: &[u8]) -> Vec<u8> {
        let mut out = PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&at.to_be_bytes());
        out.push(u8::try_from(name.len()).unwrap());
        out.extend_from_slice(name);
        out.push(u8::try_from(username.len()).unwrap());
        out.extend_from_slice(username);
        // No account id, no certificate, no grants, no avatar.
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn insert_at(v: &mut Vec<u8>, at: usize, bytes: &[u8]) {
        let tail = v.split_off(at);
        v.extend_from_slice(bytes);
        v.extend_from_slice(&tail);
    }

    /// The byte offset of the account-id length byte in an envelope built
    /// by [`raw`] / `compose` with the given name and username lengths.
    fn account_id_len_offset(name_len: usize, username_len: usize) -> usize {
        PROFILE_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 1 + name_len + 1 + username_len
    }

    // --- wire format ---------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        for p in [
            profile(Some("Alice Petrova"), Some("alice"), 1_700_000_000_000),
            profile(None, Some("bob_42"), 0),
            profile(Some("Guest"), None, u64::MAX),
            profile(None, None, 7),
            profile(Some("Анна Иванова"), Some("anna"), 1),
        ] {
            let wire = compose_profile(&p).unwrap();
            assert_eq!(decode_profile(&wire).unwrap(), p);
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire =
            compose_profile(&profile(Some("Al"), Some("abc"), 0x0102_0304_0506_0708)).unwrap();
        let sep = PROFILE_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(sep, 17);
        assert_eq!(&wire[..sep], b"lakko/profile/v1\x00");
        assert_eq!(wire[sep], PROFILE_VERSION_V4);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 2);
        assert_eq!(&wire[sep + 10..sep + 12], b"Al");
        assert_eq!(wire[sep + 12], 3);
        assert_eq!(&wire[sep + 13..sep + 16], b"abc");
        assert_eq!(
            &wire[sep + 16..],
            &[0, 0, 0, 0],
            "no account id, no certificate, no grants, no avatar: four zero bytes"
        );
    }

    #[test]
    fn envelope_length_bounds_are_the_documented_construction() {
        assert_eq!(PROFILE_ENVELOPE_MIN_LEN, 32);
        assert_eq!(
            PROFILE_ENVELOPE_MAX_LEN,
            32 + 255 + 32 + 16 + 153 + 2 * 134 + 170
        );
        assert_eq!(PROFILE_ENVELOPE_MAX_LEN, 926);
        let empty = compose_profile(&profile(None, None, 0)).unwrap();
        assert_eq!(empty.len(), PROFILE_ENVELOPE_MIN_LEN);
        // A maximal envelope: 255 bytes of name in exactly 64 chars (63
        // four-byte scalars + one three-byte scalar) and 32 of username.
        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        assert_eq!(name.len(), 255);
        assert_eq!(name.chars().count(), DISPLAY_NAME_MAX_CHARS);
        let mut maximal = profile(Some(&name), Some(&"a".repeat(32)), 0);
        maximal.account_id = Some([7; ACCOUNT_ID_LEN]);
        maximal.username_certificate = Some(cert(9));
        maximal.grants = vec![grant(ReachClass::Group, 1), grant(ReachClass::Member, 2)];
        maximal.avatar = Some(full_avatar());
        let max = compose_profile(&maximal).unwrap();
        assert_eq!(max.len(), PROFILE_ENVELOPE_MAX_LEN);
    }

    // --- fail-closed decode --------------------------------------------------

    #[test]
    fn non_profile_plaintext_falls_through() {
        assert_eq!(try_unwrap_profile(b"hi alice"), Ok(None));
        assert_eq!(try_unwrap_profile(&[]), Ok(None));
        assert_eq!(decode_profile(b"hi alice"), Err(ProfileError::WrongDomain));
    }

    #[test]
    fn bare_separator_is_truncated_not_a_fall_through() {
        assert_eq!(
            try_unwrap_profile(PROFILE_ENVELOPE_DOMAIN_V1),
            Err(ProfileError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_profile(&profile(Some("Alice"), Some("alice"), 9)).unwrap();
        for cut in PROFILE_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_profile(&wire[..cut]),
                Err(ProfileError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut wire = compose_profile(&profile(Some("Alice"), None, 9)).unwrap();
        wire.push(0);
        assert_eq!(
            decode_profile(&wire),
            Err(ProfileError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn unknown_versions_are_refused() {
        for v in [0x00, 0x01, 0x02, 0x03, 0xFF] {
            assert_eq!(
                decode_profile(&raw(v, 0, b"", b"")),
                Err(ProfileError::UnknownVersion(v))
            );
        }
    }

    #[test]
    fn non_utf8_fields_are_refused() {
        assert_eq!(
            decode_profile(&raw(4, 0, &[0xFF, 0xFE], b"")),
            Err(ProfileError::DisplayName(DisplayNameError::NotUtf8))
        );
        assert_eq!(
            decode_profile(&raw(4, 0, b"", &[0xC3, 0x28, 0x61])),
            Err(ProfileError::Username(UsernameError::InvalidCharacters))
        );
    }

    #[test]
    fn an_out_of_bounds_field_rejects_the_whole_envelope() {
        // A perfectly good username does not rescue a bad name, and vice versa.
        assert_eq!(
            decode_profile(&raw(4, 0, "Alice\u{202E}".as_bytes(), b"alice")),
            Err(ProfileError::DisplayName(
                DisplayNameError::ControlOrFormatCharacter
            ))
        );
        assert_eq!(
            decode_profile(&raw(4, 0, b"Pavel", "p\u{430}vel".as_bytes())),
            Err(ProfileError::Username(UsernameError::InvalidCharacters))
        );
    }

    #[test]
    fn decode_refuses_what_compose_refuses() {
        // Each invalid profile: compose errs, and the same fields on the wire
        // (hand-built, bypassing compose) produce the same error from decode.
        let cases: &[(&str, &str)] = &[
            (" Alice", ""),
            ("Al\nice", ""),
            ("A\u{200B}lice", ""),
            ("x", "Alice"),
            ("x", "ab"),
            ("x", "ali-ce"),
        ];
        for &(name, username) in cases {
            let p = profile(
                Some(name),
                if username.is_empty() {
                    None
                } else {
                    Some(username)
                },
                0,
            );
            let compose_err = compose_profile(&p).unwrap_err();
            let decode_err =
                decode_profile(&raw(4, 0, name.as_bytes(), username.as_bytes())).unwrap_err();
            assert_eq!(compose_err, decode_err, "{name:?} / {username:?}");
        }
        // 65 chars cannot even be hand-built past the u8 length on some
        // inputs, so check that one via a sub-255-byte ASCII name.
        let long = "a".repeat(DISPLAY_NAME_MAX_CHARS + 1);
        assert_eq!(
            compose_profile(&profile(Some(&long), None, 0)),
            decode_profile(&raw(4, 0, long.as_bytes(), b"")).map(|_| Vec::new()),
        );
    }

    #[test]
    fn some_empty_fields_are_refused_by_compose() {
        assert_eq!(
            compose_profile(&profile(Some(""), None, 0)),
            Err(ProfileError::DisplayName(DisplayNameError::Empty))
        );
        assert_eq!(
            compose_profile(&profile(None, Some(""), 0)),
            Err(ProfileError::Username(UsernameError::Empty))
        );
    }

    // --- display-name bounds -------------------------------------------------

    #[test]
    fn display_name_length_boundaries_discriminate() {
        let at_chars = "a".repeat(DISPLAY_NAME_MAX_CHARS);
        assert_eq!(check_display_name(&at_chars), Ok(()));
        assert_eq!(
            check_display_name(&"a".repeat(DISPLAY_NAME_MAX_CHARS + 1)),
            Err(DisplayNameError::TooLong { max_chars: 64 })
        );
        // 63 four-byte scalars = 252 bytes: fits. 64 = 256 bytes: one over the
        // one-byte length, refused even though the char count is admissible.
        assert_eq!(check_display_name(&"\u{1F600}".repeat(63)), Ok(()));
        assert_eq!(
            check_display_name(&"\u{1F600}".repeat(64)),
            Err(DisplayNameError::TooManyBytes { max_bytes: 255 })
        );
    }

    #[test]
    fn display_name_refuses_controls_formats_and_line_breaks() {
        for (bad, why) in [
            ("Al\u{0}ice", DisplayNameError::ControlOrFormatCharacter),
            ("Al\u{1B}ice", DisplayNameError::ControlOrFormatCharacter),
            ("Al\u{7F}ice", DisplayNameError::ControlOrFormatCharacter),
            ("Al\u{202E}ice", DisplayNameError::ControlOrFormatCharacter), // RLO
            ("Al\u{2066}ice", DisplayNameError::ControlOrFormatCharacter), // LRI
            ("Al\u{200B}ice", DisplayNameError::ControlOrFormatCharacter), // ZWSP
            ("Al\u{E0067}ice", DisplayNameError::ControlOrFormatCharacter), // stray tag
            (
                "\u{1F3F4}\u{E0067}\u{E0062}",
                DisplayNameError::ControlOrFormatCharacter,
            ), // unterminated flag
            (
                "\u{1F3F4}\u{E007F}",
                DisplayNameError::ControlOrFormatCharacter,
            ), // flag with no tags
            ("Al\u{FEFF}ice", DisplayNameError::ControlOrFormatCharacter), // BOM
            ("Al\u{AD}ice", DisplayNameError::ControlOrFormatCharacter),   // soft hyphen
            ("Al\u{61C}ice", DisplayNameError::ControlOrFormatCharacter),  // ALM
            ("Al\u{E0001}ice", DisplayNameError::ControlOrFormatCharacter), // LANGUAGE TAG
            ("Al\nice", DisplayNameError::LineBreak),
            ("Al\rice", DisplayNameError::LineBreak),
            ("Al\u{85}ice", DisplayNameError::LineBreak),
            ("Al\u{2028}ice", DisplayNameError::LineBreak),
            ("Al\u{2029}ice", DisplayNameError::LineBreak),
        ] {
            assert_eq!(check_display_name(bad), Err(why), "{bad:?}");
        }
    }

    #[test]
    fn display_name_accepts_ordinary_scripts_and_emoji() {
        for ok in [
            "Alice Petrova",
            "Анна",
            "李小龙",
            "محمد",
            "Zoë",
            "O'Brien-Smith",
            "\u{1F600} Party",
            "a",
            // ZWJ emoji sequence: man, ZWJ, woman, ZWJ, girl.
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} Family",
            // Persian spelling that needs ZWNJ ("mi-khaham").
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
            // Flag of Scotland: black flag + tags "gbsct" + cancel tag.
            "Ian \u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
        ] {
            assert_eq!(check_display_name(ok), Ok(()), "{ok:?}");
        }
    }

    #[test]
    fn display_name_must_already_be_trimmed() {
        for bad in [" Alice", "Alice ", "\u{3000}Alice", "Alice\t"] {
            assert_eq!(
                check_display_name(bad),
                Err(DisplayNameError::NotTrimmed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn normalize_trims_and_maps_blank_to_none() {
        assert_eq!(
            normalize_display_name("  Alice  "),
            Ok(Some("Alice".to_string()))
        );
        assert_eq!(normalize_display_name(""), Ok(None));
        assert_eq!(normalize_display_name(" \t\u{3000} "), Ok(None));
        assert_eq!(
            normalize_display_name(" A\u{202E}B "),
            Err(DisplayNameError::ControlOrFormatCharacter)
        );
    }

    #[test]
    fn format_table_is_sorted_disjoint_and_hits_its_edges() {
        for w in FORMAT_CHAR_RANGES.windows(2) {
            assert!(w[0].1 < w[1].0, "{w:?}");
        }
        for &(lo, hi) in FORMAT_CHAR_RANGES {
            assert!(lo <= hi);
            for cp in [lo, hi] {
                let c = char::from_u32(cp).unwrap();
                assert!(is_format_char(c), "{cp:#x}");
            }
            if let Some(before) = lo.checked_sub(1).and_then(char::from_u32) {
                // The code point just below each range is outside `Cf` in
                // every range of the table (no two ranges are adjacent).
                assert!(!is_format_char(before), "{:#x}", lo - 1);
            }
        }
        assert!(!is_format_char('a'));
        assert!(!is_format_char(' '));
    }

    #[test]
    fn error_messages_never_echo_the_name() {
        let err = check_display_name("Secret\u{202E}Name").unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        let err = decode_profile(&raw(4, 0, "Secret\u{202E}".as_bytes(), b"")).unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        assert!(!format!("{err:?}").contains("Secret"));
    }

    #[test]
    fn account_id_round_trips_and_a_bad_length_is_refused() {
        let mut p = profile(Some("Alice"), Some("alice"), 7);
        p.account_id = Some([0xAB; ACCOUNT_ID_LEN]);
        let wire = compose_profile(&p).unwrap();
        assert_eq!(wire.len(), 32 + 5 + 5 + 16);
        assert_eq!(decode_profile(&wire).unwrap(), p);
        // No account id: a single zero length byte.
        let bare = compose_profile(&profile(None, None, 1)).unwrap();
        assert_eq!(bare.len(), PROFILE_ENVELOPE_MIN_LEN);
        assert_eq!(decode_profile(&bare).unwrap().account_id, None);
        let at = account_id_len_offset(0, 0);
        // A length byte that is neither 0 nor 16 is refused as a whole.
        let mut odd = bare.clone();
        odd[at] = 4;
        insert_at(&mut odd, at + 1, &[1, 2, 3, 4]);
        assert_eq!(
            decode_profile(&odd),
            Err(ProfileError::AccountIdLength { got: 4 })
        );
        // Claims 16 bytes, carries fewer: truncated.
        let mut short = bare[..=at].to_vec();
        short[at] = 16;
        short.extend_from_slice(&[0; 8]);
        assert_eq!(decode_profile(&short), Err(ProfileError::Truncated));
    }

    #[test]
    fn a_v1_envelope_is_refused_as_unknown_version() {
        let mut wire = compose_profile(&profile(Some("Alice"), None, 1)).unwrap();
        wire[PROFILE_ENVELOPE_DOMAIN_V1.len()] = PROFILE_VERSION_V1;
        assert_eq!(
            decode_profile(&wire),
            Err(ProfileError::UnknownVersion(0x01))
        );
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(
            ProfileError::AccountIdLength { got: 3 }.code(),
            "accountId.length"
        );
        assert_eq!(ProfileError::WrongDomain.code(), "wrongDomain");
        assert_eq!(ProfileError::Truncated.code(), "truncated");
        assert_eq!(
            ProfileError::DisplayName(DisplayNameError::TooLong { max_chars: 64 }).code(),
            "displayName.tooLong"
        );
        assert_eq!(
            ProfileError::Username(UsernameError::InvalidCharacters).code(),
            "username.invalidCharacters"
        );
    }

    // --- v3: certificate and grants (ADR-0060) ------------------------------

    #[test]
    fn certificate_and_grants_round_trip() {
        let mut p = profile(Some("Alice"), Some("alice"), 5);
        p.username_certificate = Some(cert(0x5A));
        for grants in [
            vec![],
            vec![grant(ReachClass::Group, 1)],
            vec![grant(ReachClass::Member, 2)],
            vec![grant(ReachClass::Channel, 3)],
            vec![grant(ReachClass::Group, 4), grant(ReachClass::Member, 5)],
            vec![grant(ReachClass::Member, 6), grant(ReachClass::Group, 7)],
            vec![grant(ReachClass::Channel, 8), grant(ReachClass::Member, 9)],
            vec![grant(ReachClass::Contact, 10)],
            vec![
                grant(ReachClass::Contact, 11),
                grant(ReachClass::Member, 12),
            ],
        ] {
            p.grants = grants;
            let wire = compose_profile(&p).unwrap();
            assert_eq!(
                wire.len(),
                32 + 5 + 5 + USERNAME_CERT_LEN + p.grants.len() * REACH_GRANT_LEN
            );
            assert_eq!(decode_profile(&wire).unwrap(), p);
        }
    }

    #[test]
    fn a_certificate_needs_a_username() {
        let mut p = profile(Some("Alice"), None, 5);
        p.username_certificate = Some(cert(1));
        assert_eq!(
            compose_profile(&p),
            Err(ProfileError::CertificateWithoutUsername)
        );
        // The same fields on the wire, hand-built past compose.
        let mut wire = raw(4, 5, b"Alice", b"");
        let at = account_id_len_offset(5, 0);
        wire[at + 1] = u8::try_from(USERNAME_CERT_LEN).unwrap();
        insert_at(&mut wire, at + 2, &cert(1));
        assert_eq!(
            decode_profile(&wire),
            Err(ProfileError::CertificateWithoutUsername)
        );
    }

    #[test]
    fn a_certificate_of_any_other_length_is_refused() {
        for len in [
            1usize,
            64,
            USERNAME_CERT_LEN - 1,
            USERNAME_CERT_LEN + 1,
            255,
        ] {
            let mut p = profile(None, Some("alice"), 5);
            p.username_certificate = Some(vec![0; len]);
            assert_eq!(
                compose_profile(&p),
                Err(ProfileError::CertificateLength { got: len })
            );
            let mut wire = raw(4, 5, b"", b"alice");
            let at = account_id_len_offset(0, 5);
            wire[at + 1] = u8::try_from(len).unwrap();
            insert_at(&mut wire, at + 2, &vec![0; len]);
            assert_eq!(
                decode_profile(&wire),
                Err(ProfileError::CertificateLength { got: len })
            );
        }
        // An empty `Some` is refused by compose (the wire has no spelling
        // for it: a zero length is `None`).
        let mut p = profile(None, Some("alice"), 5);
        p.username_certificate = Some(Vec::new());
        assert_eq!(
            compose_profile(&p),
            Err(ProfileError::CertificateLength { got: 0 })
        );
    }

    #[test]
    fn grant_bounds_are_refused_by_compose_and_decode_alike() {
        let cases: Vec<(Vec<ReachGrant>, ProfileError)> = vec![
            (
                vec![
                    grant(ReachClass::Group, 1),
                    grant(ReachClass::Member, 2),
                    grant(ReachClass::Channel, 3),
                ],
                ProfileError::TooManyGrants { count: 3 },
            ),
            (
                vec![grant(ReachClass::Group, 1), grant(ReachClass::Group, 2)],
                ProfileError::DuplicateGrantClass(ReachClass::Group.to_byte()),
            ),
            (
                vec![grant(ReachClass::Contact, 1), grant(ReachClass::Contact, 2)],
                ProfileError::DuplicateGrantClass(ReachClass::Contact.to_byte()),
            ),
            (
                vec![grant(ReachClass::Link, 1)],
                ProfileError::GrantClassNotAllowed(ReachClass::Link.to_byte()),
            ),
            (
                vec![grant(ReachClass::Reply, 1)],
                ProfileError::GrantClassNotAllowed(ReachClass::Reply.to_byte()),
            ),
            (
                vec![grant(ReachClass::Group, 1), grant(ReachClass::Fof, 1)],
                ProfileError::GrantClassNotAllowed(ReachClass::Fof.to_byte()),
            ),
        ];
        for (grants, why) in cases {
            let mut p = profile(Some("Alice"), None, 5);
            p.grants.clone_from(&grants);
            assert_eq!(compose_profile(&p), Err(why), "{grants:?}");
            // The same grants on the wire, hand-built past compose.
            let mut wire = raw(4, 5, b"Alice", b"");
            let count_at = account_id_len_offset(5, 0) + 2;
            wire[count_at] = u8::try_from(grants.len()).unwrap();
            wire.pop(); // the avatar length, re-pushed after the grants
            for g in &grants {
                wire.extend_from_slice(&g.encode());
            }
            wire.push(0);
            assert_eq!(decode_profile(&wire), Err(why), "{grants:?}");
        }
    }

    #[test]
    fn a_grant_with_an_unknown_class_byte_is_refused() {
        let mut wire = raw(4, 5, b"Alice", b"");
        let count_at = account_id_len_offset(5, 0) + 2;
        wire[count_at] = 1;
        let mut g = grant(ReachClass::Group, 1).encode();
        g[0] = 0x09;
        wire.pop();
        wire.extend_from_slice(&g);
        wire.push(0);
        assert_eq!(
            decode_profile(&wire),
            Err(ProfileError::Grant(ReachError::UnknownClass(0x09)))
        );
    }

    #[test]
    fn a_huge_grant_count_is_refused_before_reading_grants() {
        let mut wire = raw(4, 5, b"", b"");
        let count_at = account_id_len_offset(0, 0) + 2;
        wire[count_at] = 0xFF;
        assert_eq!(
            decode_profile(&wire),
            Err(ProfileError::TooManyGrants { count: 255 })
        );
    }

    #[test]
    fn every_truncation_point_of_a_full_v3_envelope_is_refused() {
        let mut p = profile(Some("Alice"), Some("alice"), 9);
        p.account_id = Some([3; ACCOUNT_ID_LEN]);
        p.username_certificate = Some(cert(4));
        p.grants = vec![grant(ReachClass::Group, 1), grant(ReachClass::Member, 2)];
        let wire = compose_profile(&p).unwrap();
        for cut in PROFILE_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_profile(&wire[..cut]),
                Err(ProfileError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire;
        long.push(0);
        assert_eq!(
            decode_profile(&long),
            Err(ProfileError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn debug_of_a_profile_never_prints_a_grant_seed() {
        let mut p = profile(Some("Alice"), None, 5);
        p.grants = vec![grant(ReachClass::Group, 0xEE)];
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("238"), "{dbg}");
        assert!(dbg.contains("redacted"));
    }

    /// Every single-byte substitution of a full v3 envelope: decode never
    /// panics, and whatever still decodes re-composes to the identical bytes.
    #[test]
    fn every_single_byte_mutation_decodes_canonically_or_is_refused() {
        let mut p = profile(Some("Al"), Some("abc"), 9);
        p.account_id = Some([3; ACCOUNT_ID_LEN]);
        p.username_certificate = Some(cert(4));
        p.grants = vec![grant(ReachClass::Group, 1), grant(ReachClass::Member, 2)];
        let wire = compose_profile(&p).unwrap();
        for at in 0..wire.len() {
            for byte in [
                0u8,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                16,
                0x61,
                0x7F,
                0x99,
                0xFF,
                wire[at] ^ 1,
            ] {
                let mut m = wire.clone();
                m[at] = byte;
                if let Ok(decoded) = decode_profile(&m) {
                    assert_eq!(compose_profile(&decoded).unwrap(), m);
                }
            }
        }
    }

    #[test]
    fn v3_error_codes_are_frozen() {
        assert_eq!(
            ProfileError::CertificateLength { got: 1 }.code(),
            "certificate.length"
        );
        assert_eq!(
            ProfileError::CertificateWithoutUsername.code(),
            "certificate.withoutUsername"
        );
        assert_eq!(
            ProfileError::TooManyGrants { count: 3 }.code(),
            "grants.tooMany"
        );
        assert_eq!(
            ProfileError::DuplicateGrantClass(4).code(),
            "grants.duplicateClass"
        );
        assert_eq!(
            ProfileError::GrantClassNotAllowed(1).code(),
            "grants.classNotAllowed"
        );
        assert_eq!(
            ProfileError::Grant(ReachError::UnknownClass(9)).code(),
            "grants.unknownClass"
        );
    }

    // --- order ---------------------------------------------------------------

    #[test]
    fn newer_is_strictly_greater() {
        assert!(profile_is_newer(2, 1));
        assert!(!profile_is_newer(1, 1), "equal is not newer");
        assert!(!profile_is_newer(0, 1));
        assert!(profile_is_newer(u64::MAX, u64::MAX - 1));
        assert!(!profile_is_newer(u64::MAX, u64::MAX));
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
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, PROFILE_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(PROFILE_ENVELOPE_DOMAIN_V1)
                    && !PROFILE_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn profile_does_not_cross_dispatch_as_a_sibling() {
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::committer_beacon::{compose_committer_beacon, try_unwrap_committer_beacon};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};
        use crate::push_routing::{compose_push_routing_secret, try_unwrap_push_routing_secret};

        let wire = compose_profile(&profile(Some("Alice"), Some("alice"), 1)).unwrap();
        assert!(try_unwrap_call_welcome(&wire).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&wire).unwrap().is_none());
        assert!(try_unwrap_media_state(&wire).unwrap().is_none());
        assert!(try_unwrap_push_routing_secret(&wire).unwrap().is_none());
        assert!(try_unwrap_committer_beacon(&wire).unwrap().is_none());

        for sibling in [
            wrap_call_welcome(&[0xAB; 256]),
            wrap_call_kp_forward(&[0xCD; 512]),
            compose_media_state(true, true),
            compose_push_routing_secret(7, &[0x11; 32]),
            compose_committer_beacon(),
        ] {
            assert_eq!(try_unwrap_profile(&sibling), Ok(None));
        }
    }
}
