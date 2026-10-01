// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Directory listing — the public copy of a channel's metadata the owner
//! chooses to publish ([ADR-0059 §3](../../../docs/adr/0059-channel-discover.md)),
//! plus the fixed topic list and the language rule both the listing and the
//! encrypted metadata record ([`crate::meta`]) use.
//!
//! ## What a listing is, and what it is not
//!
//! A listing is **plaintext on the server by the owner's choice**: handle,
//! name, description, topic and language of one channel. It carries no
//! post, no key and no reader's data (invariants #1, #2). It is signed by
//! the channel's owner key ([`crate::owner`]) through the owner request
//! proof when it is uploaded; the codec here carries no signature of its own,
//! because the server checks the request, not the record, and a reader
//! trusts nothing in a listing until it has resolved the handle through the
//! normal OPRF lookup to the same `channel_id` (ADR-0059 §3).
//!
//! ## Wire shape
//!
//! ```text
//!   CHANNEL_LISTING_DOMAIN_V1 || version || channel_id || handle_len || handle
//!   └──── 25 bytes, ASCII ───┘ └─ 0x01 ─┘ └── 16 B ──┘ └── 1 B ───┘ └ 3..32 ┘
//!
//!     || name_len || name || description_len || description || topic
//!        └─ 1 B ─┘ └ .. ┘ └───── 2 B, BE ─────┘ └──── .. ────┘ └ 1 B ┘
//!
//!     || language_len || language || updated_at_ms
//!        └─── 1 B ────┘ └ 2..3 B ─┘ └── 8 B, BE ──┘
//!
//!                     "lakko/channel-listing/v1\x00"
//! ```
//!
//! Total length is between [`CHANNEL_LISTING_MIN_LEN`] and
//! [`CHANNEL_LISTING_MAX_LEN`].
//!
//! ## Bounds
//!
//! * `handle`: the canonical username rule,
//!   `lakko_contacts::username::check_canonical_username` (3 to 32 of
//!   `[a-z0-9_]`), defined once so a listing can never carry a handle the
//!   discovery blinder would refuse.
//! * `name` / `description`: the ADR-0051 channel rules
//!   (`lakko_contacts::channel_text`), the same ones the metadata record
//!   uses.
//! * `topic`: any byte; unknown ids read as "Other" ([`topic_name`]). Kept
//!   verbatim so the encoding stays canonical.
//! * `language`: **required** here, [`check_language`]. The metadata record
//!   admits an unset (empty) language; a listing does not, because the
//!   directory buckets by `(topic, language)`.
//!
//! A listing violating any bound is **rejected as a whole**, and
//! **`compose_channel_listing` refuses exactly what `decode_channel_listing`
//! refuses** — both run [`ChannelListing::validate`]. Every accepted
//! encoding re-composes to the identical bytes.
//!
//! ## Order
//!
//! [`listing_is_newer`]: strictly greater `updated_at_ms` wins, equal is not
//! newer. The server refuses a listing that is not newer than the stored one.

use core::result::Result as CoreResult;

use lakko_contacts::channel_text::{
    check_description, check_name, ChannelTextError, DESCRIPTION_MAX_BYTES, NAME_MAX_BYTES,
};
use lakko_contacts::username::{check_canonical_username, UsernameError, USERNAME_MAX_LEN};
use thiserror::Error;

use crate::CHANNEL_ID_LEN;

/// 25-byte domain separator a listing starts with. **Frozen**; the
/// in-payload version byte is the knob that moves.
pub const CHANNEL_LISTING_DOMAIN_V1: &[u8] = b"lakko/channel-listing/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_LISTING_VERSION_V1: u8 = 0x01;

/// Shortest admissible language code.
pub const LANGUAGE_MIN_LEN: usize = 2;
/// Longest admissible language code.
pub const LANGUAGE_MAX_LEN: usize = 3;

/// Bytes after the separator every listing carries: version, `channel_id`,
/// `handle_len`, `name_len`, `description_len`, `topic`, `language_len`,
/// `updated_at_ms`.
const FIXED_PAYLOAD_LEN: usize = 1 + CHANNEL_ID_LEN + 1 + 1 + 2 + 1 + 1 + 8;

/// Shortest well-formed listing: a 3-byte handle, a 1-byte name, no
/// description, a 2-letter language.
pub const CHANNEL_LISTING_MIN_LEN: usize =
    CHANNEL_LISTING_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 3 + 1 + LANGUAGE_MIN_LEN;

/// Longest well-formed listing.
pub const CHANNEL_LISTING_MAX_LEN: usize = CHANNEL_LISTING_DOMAIN_V1.len()
    + FIXED_PAYLOAD_LEN
    + USERNAME_MAX_LEN
    + NAME_MAX_BYTES
    + DESCRIPTION_MAX_BYTES
    + LANGUAGE_MAX_LEN;

/// The topic id every unknown byte reads as.
pub const TOPIC_OTHER: u8 = 0;

/// The fixed topic list (ADR-0059 §5): stable `u8` ids and English names the
/// clients localise.
///
/// **Twenty-four entries is a chosen number, not a measured one**, and "Other"
/// (id 0) is one of them. The ids are frozen: an entry may be added at the
/// end, never renumbered or reused, because a listing and a metadata record
/// carry the id and the directory buckets by it.
pub const TOPICS: [(u8, &str); 24] = [
    (0, "Other"),
    (1, "News"),
    (2, "Technology"),
    (3, "Science"),
    (4, "Business"),
    (5, "Finance"),
    (6, "Politics"),
    (7, "Sports"),
    (8, "Gaming"),
    (9, "Music"),
    (10, "Film and TV"),
    (11, "Books"),
    (12, "Art and Design"),
    (13, "Photography"),
    (14, "Food"),
    (15, "Travel"),
    (16, "Health"),
    (17, "Education"),
    (18, "Humour"),
    (19, "Lifestyle"),
    (20, "Nature"),
    (21, "Religion"),
    (22, "Local"),
    (23, "Software Development"),
];

/// `true` when `id` names an entry of [`TOPICS`].
#[must_use]
pub fn is_known_topic(id: u8) -> bool {
    TOPICS.iter().any(|&(t, _)| t == id)
}

/// The English name of topic `id`; "Other" for an id this build does not
/// know.
#[must_use]
pub fn topic_name(id: u8) -> &'static str {
    TOPICS
        .iter()
        .find(|&&(t, _)| t == id)
        .map_or(TOPICS[0].1, |&(_, name)| name)
}

/// Why a language code was refused. A reason code; never the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LanguageError {
    /// Not 2 or 3 characters long.
    #[error("language code is not 2 or 3 letters long")]
    BadLength,
    /// Contains a byte that is not an ASCII lowercase letter.
    #[error("language code is not ASCII lowercase letters")]
    NotLowercaseAscii,
}

impl LanguageError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::NotLowercaseAscii => "notLowercaseAscii",
        }
    }
}

/// Check an ISO 639 language code: 2 or 3 ASCII lowercase letters. No table
/// of assigned codes is consulted (a syntactic rule, like the handle's).
///
/// # Errors
///
/// [`LanguageError::BadLength`] or [`LanguageError::NotLowercaseAscii`].
pub fn check_language(language: &str) -> CoreResult<(), LanguageError> {
    if !(LANGUAGE_MIN_LEN..=LANGUAGE_MAX_LEN).contains(&language.len()) {
        return Err(LanguageError::BadLength);
    }
    if !language.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err(LanguageError::NotLowercaseAscii);
    }
    Ok(())
}

/// One directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelListing {
    /// The channel's 16-byte id.
    pub channel_id: [u8; CHANNEL_ID_LEN],
    /// The channel's handle, canonical (3-32 of `[a-z0-9_]`).
    pub handle: String,
    /// The channel name (the ADR-0051 name rule).
    pub name: String,
    /// The channel description (the ADR-0051 description rule).
    pub description: String,
    /// A [`TOPICS`] id; unknown ids read as "Other".
    pub topic: u8,
    /// ISO 639 language code, required.
    pub language: String,
    /// Owner-clock UNIX milliseconds; orders listings of one channel.
    pub updated_at_ms: u64,
}

impl ChannelListing {
    /// Check every bound. The single rule both compose and decode run.
    ///
    /// # Errors
    ///
    /// The first field that violates its rule, in wire order.
    pub fn validate(&self) -> CoreResult<(), ChannelListingError> {
        check_canonical_username(&self.handle).map_err(ChannelListingError::Handle)?;
        check_name(&self.name).map_err(ChannelListingError::Name)?;
        check_description(&self.description).map_err(ChannelListingError::Description)?;
        check_language(&self.language).map_err(ChannelListingError::Language)?;
        Ok(())
    }
}

/// Why a listing was refused. Reason codes only; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelListingError {
    /// The bytes do not start with [`CHANNEL_LISTING_DOMAIN_V1`].
    #[error("bytes do not carry the channel-listing domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("channel listing is truncated")]
    Truncated,
    /// Bytes remain after `updated_at_ms`.
    #[error("channel listing has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the last field.
        count: usize,
    },
    /// The version byte is not [`CHANNEL_LISTING_VERSION_V1`].
    #[error("channel listing carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The handle is not a canonical username.
    #[error("invalid channel handle: {0}")]
    Handle(UsernameError),
    /// The name violates a bound.
    #[error("invalid channel name: {0}")]
    Name(ChannelTextError),
    /// The description violates a bound.
    #[error("invalid channel description: {0}")]
    Description(ChannelTextError),
    /// The language is not an ISO 639 code.
    #[error("invalid channel language: {0}")]
    Language(LanguageError),
}

impl ChannelListingError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen. Field
    /// errors are namespaced (`handle.tooShort`, `name.empty`, ...).
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::Handle(e) => format!("handle.{}", e.code()),
            Self::Name(e) => format!("name.{}", e.code()),
            Self::Description(e) => format!("description.{}", e.code()),
            Self::Language(e) => format!("language.{}", e.code()),
        }
    }
}

/// `true` when a listing stamped `incoming_updated_at_ms` must replace the
/// stored one for the same channel. Strictly greater; equal is not newer.
#[must_use]
pub const fn listing_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a listing.
///
/// # Errors
///
/// Exactly the errors [`ChannelListing::validate`] returns.
pub fn compose_channel_listing(
    listing: &ChannelListing,
) -> CoreResult<Vec<u8>, ChannelListingError> {
    listing.validate()?;
    let handle = listing.handle.as_bytes();
    let name = listing.name.as_bytes();
    let description = listing.description.as_bytes();
    let language = listing.language.as_bytes();
    // `validate` bounded every length; the conversions are re-checked rather
    // than cast so a future bound change cannot truncate silently.
    let handle_len = u8::try_from(handle.len()).map_err(|_| {
        ChannelListingError::Handle(UsernameError::TooLong {
            max: USERNAME_MAX_LEN,
        })
    })?;
    let name_len = u8::try_from(name.len()).map_err(|_| {
        ChannelListingError::Name(ChannelTextError::TooManyBytes {
            max_bytes: NAME_MAX_BYTES,
        })
    })?;
    let description_len = u16::try_from(description.len()).map_err(|_| {
        ChannelListingError::Description(ChannelTextError::TooManyBytes {
            max_bytes: DESCRIPTION_MAX_BYTES,
        })
    })?;
    let language_len = u8::try_from(language.len())
        .map_err(|_| ChannelListingError::Language(LanguageError::BadLength))?;

    let mut out = Vec::with_capacity(
        CHANNEL_LISTING_DOMAIN_V1.len()
            + FIXED_PAYLOAD_LEN
            + handle.len()
            + name.len()
            + description.len()
            + language.len(),
    );
    out.extend_from_slice(CHANNEL_LISTING_DOMAIN_V1);
    out.push(CHANNEL_LISTING_VERSION_V1);
    out.extend_from_slice(&listing.channel_id);
    out.push(handle_len);
    out.extend_from_slice(handle);
    out.push(name_len);
    out.extend_from_slice(name);
    out.extend_from_slice(&description_len.to_be_bytes());
    out.extend_from_slice(description);
    out.push(listing.topic);
    out.push(language_len);
    out.extend_from_slice(language);
    out.extend_from_slice(&listing.updated_at_ms.to_be_bytes());
    Ok(out)
}

/// Decode a listing. Bounded and fail-closed: every length prefix is checked
/// against the remaining bytes before anything is allocated, and the decoded
/// listing must pass [`ChannelListing::validate`].
///
/// # Errors
///
/// Any [`ChannelListingError`].
pub fn decode_channel_listing(bytes: &[u8]) -> CoreResult<ChannelListing, ChannelListingError> {
    let mut cur = bytes
        .strip_prefix(CHANNEL_LISTING_DOMAIN_V1)
        .ok_or(ChannelListingError::WrongDomain)?;

    let version = take_u8(&mut cur)?;
    if version != CHANNEL_LISTING_VERSION_V1 {
        return Err(ChannelListingError::UnknownVersion(version));
    }
    let channel_id: [u8; CHANNEL_ID_LEN] = take(&mut cur, CHANNEL_ID_LEN)?
        .try_into()
        .map_err(|_| ChannelListingError::Truncated)?;
    let handle_len = usize::from(take_u8(&mut cur)?);
    let handle = take(&mut cur, handle_len)?;
    let name_len = usize::from(take_u8(&mut cur)?);
    let name = take(&mut cur, name_len)?;
    let description_len_bytes: [u8; 2] = take(&mut cur, 2)?
        .try_into()
        .map_err(|_| ChannelListingError::Truncated)?;
    let description = take(
        &mut cur,
        usize::from(u16::from_be_bytes(description_len_bytes)),
    )?;
    let topic = take_u8(&mut cur)?;
    let language_len = usize::from(take_u8(&mut cur)?);
    let language = take(&mut cur, language_len)?;
    let updated_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| ChannelListingError::Truncated)?;
    if !cur.is_empty() {
        return Err(ChannelListingError::TrailingBytes { count: cur.len() });
    }

    let handle = core::str::from_utf8(handle)
        .map_err(|_| ChannelListingError::Handle(UsernameError::InvalidCharacters))?;
    let name = core::str::from_utf8(name)
        .map_err(|_| ChannelListingError::Name(ChannelTextError::NotUtf8))?;
    let description = core::str::from_utf8(description)
        .map_err(|_| ChannelListingError::Description(ChannelTextError::NotUtf8))?;
    let language = core::str::from_utf8(language)
        .map_err(|_| ChannelListingError::Language(LanguageError::NotLowercaseAscii))?;

    let listing = ChannelListing {
        channel_id,
        handle: handle.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        topic,
        language: language.to_string(),
        updated_at_ms: u64::from_be_bytes(updated_at_bytes),
    };
    listing.validate()?;
    Ok(listing)
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, ChannelListingError> {
    let (&first, rest) = cur.split_first().ok_or(ChannelListingError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], ChannelListingError> {
    if cur.len() < n {
        return Err(ChannelListingError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> ChannelListing {
        ChannelListing {
            channel_id: [0x11; CHANNEL_ID_LEN],
            handle: "lakko_news".to_string(),
            name: "Lakko News".to_string(),
            description: "Releases\nand outages".to_string(),
            topic: 2,
            language: "en".to_string(),
            updated_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn compose_then_decode_round_trips() {
        let l = listing();
        let wire = compose_channel_listing(&l).unwrap();
        assert_eq!(decode_channel_listing(&wire).unwrap(), l);
        let mut unknown_topic = l;
        unknown_topic.topic = 200;
        unknown_topic.language = "fil".to_string();
        unknown_topic.description = String::new();
        let wire = compose_channel_listing(&unknown_topic).unwrap();
        assert_eq!(decode_channel_listing(&wire).unwrap(), unknown_topic);
    }

    #[test]
    fn layout_and_bounds_are_pinned() {
        assert_eq!(CHANNEL_LISTING_DOMAIN_V1.len(), 25);
        assert_eq!(CHANNEL_LISTING_MIN_LEN, 25 + 31 + 3 + 1 + 2);
        assert_eq!(CHANNEL_LISTING_MAX_LEN, 25 + 31 + 32 + 255 + 1024 + 3);
        let min = ChannelListing {
            channel_id: [0; 16],
            handle: "abc".to_string(),
            name: "a".to_string(),
            description: String::new(),
            topic: 0,
            language: "en".to_string(),
            updated_at_ms: 0x0102_0304_0506_0708,
        };
        let wire = compose_channel_listing(&min).unwrap();
        assert_eq!(wire.len(), CHANNEL_LISTING_MIN_LEN);
        let sep = 25;
        assert_eq!(&wire[..sep], b"lakko/channel-listing/v1\x00");
        assert_eq!(wire[sep], 0x01);
        assert_eq!(&wire[sep + 1..sep + 17], &[0; 16]);
        assert_eq!(wire[sep + 17], 3);
        assert_eq!(&wire[sep + 18..sep + 21], b"abc");
        assert_eq!(wire[sep + 21], 1);
        assert_eq!(wire[sep + 22], b'a');
        assert_eq!(&wire[sep + 23..sep + 25], &[0, 0]);
        assert_eq!(wire[sep + 25], 0);
        assert_eq!(wire[sep + 26], 2);
        assert_eq!(&wire[sep + 27..sep + 29], b"en");
        assert_eq!(&wire[sep + 29..], &[1, 2, 3, 4, 5, 6, 7, 8]);

        let max = ChannelListing {
            channel_id: [0; 16],
            handle: "a".repeat(32),
            name: format!("{}\u{20AC}", "\u{1F600}".repeat(63)),
            description: "\u{1F600}".repeat(256),
            topic: 0,
            language: "eng".to_string(),
            updated_at_ms: 0,
        };
        assert_eq!(
            compose_channel_listing(&max).unwrap().len(),
            CHANNEL_LISTING_MAX_LEN
        );
    }

    #[test]
    fn every_truncation_and_trailing_byte_is_refused() {
        let wire = compose_channel_listing(&listing()).unwrap();
        for cut in CHANNEL_LISTING_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                decode_channel_listing(&wire[..cut]),
                Err(ChannelListingError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire;
        long.push(0);
        assert_eq!(
            decode_channel_listing(&long),
            Err(ChannelListingError::TrailingBytes { count: 1 })
        );
        assert_eq!(
            decode_channel_listing(b"hello"),
            Err(ChannelListingError::WrongDomain)
        );
    }

    #[test]
    fn unknown_version_is_refused() {
        let mut wire = compose_channel_listing(&listing()).unwrap();
        wire[CHANNEL_LISTING_DOMAIN_V1.len()] = 2;
        assert_eq!(
            decode_channel_listing(&wire),
            Err(ChannelListingError::UnknownVersion(2))
        );
    }

    #[test]
    fn decode_refuses_what_compose_refuses() {
        let cases: Vec<(ChannelListing, &str)> = vec![
            (
                ChannelListing {
                    handle: "ab".to_string(),
                    ..listing()
                },
                "handle.tooShort",
            ),
            (
                ChannelListing {
                    handle: "Lakko".to_string(),
                    ..listing()
                },
                "handle.invalidCharacters",
            ),
            (
                ChannelListing {
                    name: String::new(),
                    ..listing()
                },
                "name.empty",
            ),
            (
                ChannelListing {
                    description: "x\u{202E}".to_string(),
                    ..listing()
                },
                "description.controlOrFormatCharacter",
            ),
            (
                ChannelListing {
                    language: String::new(),
                    ..listing()
                },
                "language.badLength",
            ),
            (
                ChannelListing {
                    language: "EN".to_string(),
                    ..listing()
                },
                "language.notLowercaseAscii",
            ),
        ];
        for (bad, code) in cases {
            let err = compose_channel_listing(&bad).unwrap_err();
            assert_eq!(err.code(), code);
            // Hand-encode bypassing validation: decode must refuse the same.
            let mut raw = CHANNEL_LISTING_DOMAIN_V1.to_vec();
            raw.push(1);
            raw.extend_from_slice(&bad.channel_id);
            raw.push(u8::try_from(bad.handle.len()).unwrap());
            raw.extend_from_slice(bad.handle.as_bytes());
            raw.push(u8::try_from(bad.name.len()).unwrap());
            raw.extend_from_slice(bad.name.as_bytes());
            raw.extend_from_slice(&u16::try_from(bad.description.len()).unwrap().to_be_bytes());
            raw.extend_from_slice(bad.description.as_bytes());
            raw.push(bad.topic);
            raw.push(u8::try_from(bad.language.len()).unwrap());
            raw.extend_from_slice(bad.language.as_bytes());
            raw.extend_from_slice(&bad.updated_at_ms.to_be_bytes());
            assert_eq!(decode_channel_listing(&raw).unwrap_err(), err);
        }
    }

    #[test]
    fn topics_are_24_stable_ids_and_unknown_reads_as_other() {
        assert_eq!(TOPICS.len(), 24);
        for (i, &(id, name)) in TOPICS.iter().enumerate() {
            assert_eq!(usize::from(id), i, "ids are dense and in order");
            assert!(!name.is_empty());
            assert!(is_known_topic(id));
            assert_eq!(topic_name(id), name);
        }
        assert_eq!(TOPICS[usize::from(TOPIC_OTHER)].1, "Other");
        assert!(!is_known_topic(24));
        assert_eq!(topic_name(24), "Other");
        assert_eq!(topic_name(255), "Other");
    }

    #[test]
    fn language_rule() {
        assert_eq!(check_language("en"), Ok(()));
        assert_eq!(check_language("fil"), Ok(()));
        assert_eq!(check_language("e"), Err(LanguageError::BadLength));
        assert_eq!(check_language("engl"), Err(LanguageError::BadLength));
        assert_eq!(check_language("e1"), Err(LanguageError::NotLowercaseAscii));
        assert_eq!(check_language("éé"), Err(LanguageError::BadLength));
        assert_eq!(check_language("En"), Err(LanguageError::NotLowercaseAscii));
    }

    #[test]
    fn newer_is_strictly_greater() {
        assert!(listing_is_newer(2, 1));
        assert!(!listing_is_newer(1, 1));
        assert!(!listing_is_newer(0, 1));
    }

    #[test]
    fn domain_is_distinct_from_the_metadata_record() {
        let meta = crate::meta::CHANNEL_META_DOMAIN_V1;
        assert!(!meta.starts_with(CHANNEL_LISTING_DOMAIN_V1));
        assert!(!CHANNEL_LISTING_DOMAIN_V1.starts_with(meta));
    }
}
