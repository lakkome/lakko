// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The public record of a public group or channel ([ADR-0087] section 4).
//!
//! A public group or channel is published the way a person in Everyone is:
//! one record sealed under each search term
//! ([`crate::introduction`]'s record wire, keyed per term over the OPRF
//! outputs of the `@name` and the ADR-0060 name terms), carrying a payload of
//! its own:
//!
//! ```text
//! payload = version(1)=0x43 ‖ kind(1: 1 group, 2 channel) ‖ updated_at_ms(8, BE)
//!           ‖ id_len(1) ‖ id ‖ key(32) ‖ knock_version(2, BE)
//!           ‖ handle_len(1) ‖ handle ‖ name_len(1) ‖ name
//!           ‖ description_len(2, BE) ‖ description ‖ size_band(1)
//!           ‖ avatar_len(1: 0 | 84 | 170) ‖ avatar
//! ```
//!
//! Version `0x42` added the photo (ADR-0087 section 18.3), version `0x43`
//! the knock version (both flag days): a `0x41` or `0x42` payload is refused
//! like any other unknown first byte.
//!
//! - **The first byte** is the payload's kind as well as its version: a
//!   person's payload starts with its own version byte (`0x03`), so one
//!   record wire carries either, and a searcher that opened a record learns
//!   which ([`open_search_record`]). A group's payload can never open as an
//!   introduction (its version byte is refused there), so it cannot be
//!   relayed as one.
//! - **`id`** is the `group_id` (32 bytes) of a group, the `channel_id`
//!   (16 bytes) of a channel; any other length for the kind is refused.
//! - **`key`** is the group's knock public key ([`crate::knock`]) or the
//!   channel's read key.
//! - **`knock_version`** is the version of that knock key
//!   ([`crate::group_invite::GroupKnockKey::version`]): a searcher names it
//!   in the knock row it seals ([`crate::knock::seal_knock`]), since removing
//!   an admin moves the group to a new version and the admins open a row only
//!   under the version it names. A channel has no knock key, so its record
//!   carries `0` and any other value refuses the payload.
//! - **`handle`** is the canonical `@name`. ADR-0087 section 4's payload
//!   omits it; it is carried because a search by name returns a record the
//!   searcher did not find by `@name`, and the card shows the `@name`
//!   (UX section 5.1).
//! - **`name`**: a group's follows the display-name rule
//!   ([`crate::profile::check_display_name`]), a channel's the channel-name
//!   rule ([`crate::channel_text`]); either must be non-empty, since a public
//!   group needs a name (a title built from member names is never
//!   published). **`description`** follows the channel description rule
//!   for both kinds.
//! - **`size_band`**: `0` `<10`, `1` `10+`, `2` `100+`, `3` `1k+`, `4`
//!   `10k+`, `5` `100k+` (ADR-0059). A group's is the owner's statement; a
//!   channel's is unused (the channels service counts it).
//! - **`avatar`**: the photo, in the [`crate::avatar`] codec a person's
//!   profile uses (ADR-0087 section 18.3). `avatar_len = 0` means no photo.
//!   **The blob keys in it are public**: anyone who opens the record can
//!   fetch and decrypt the photo while it lives, as with the name; a group or
//!   channel going private re-uploads its photo under a fresh key (a client
//!   obligation, not this codec's).
//!
//! Whole-payload rejection: any field outside its bound refuses the payload.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::avatar::{
    decode_avatar_field, encode_avatar_field, Avatar, AvatarError, AVATAR_FIELD_MAX_LEN,
};
use crate::channel_text::{check_description, check_name, ChannelTextError, DESCRIPTION_MAX_BYTES};
use crate::introduction::{
    seal_record, IntroductionError, IntroductionPayload, IntroductionRecord, OPRF_OUTPUT_LEN,
    PUBLIC_SEALED_BOUNDS,
};
use crate::profile::{check_display_name, DisplayNameError, DISPLAY_NAME_MAX_BYTES};
use crate::username::{
    check_canonical_username, UsernameError, USERNAME_MAX_LEN, USERNAME_MIN_LEN,
};

/// The payload's first byte: version 3 of a group's or channel's payload
/// (version 1, `0x41`, carried no photo; version 2, `0x42`, no knock
/// version; both are refused). Distinct from every person payload version.
pub const HANDLE_RECORD_PAYLOAD_VERSION_V3: u8 = 0x43;

/// Length of the knock version in the payload (big-endian `u16`).
pub const HANDLE_RECORD_KNOCK_VERSION_LEN: usize = 2;

/// Length of a group id in the payload.
pub const HANDLE_RECORD_GROUP_ID_LEN: usize = 32;

/// Length of a channel id in the payload.
pub const HANDLE_RECORD_CHANNEL_ID_LEN: usize = 16;

/// Length of the knock key or read key.
pub const HANDLE_RECORD_KEY_LEN: usize = 32;

/// The highest size band (`100k+`).
pub const MAX_SIZE_BAND: u8 = 5;

/// Shortest payload: a channel id, a three-character handle, a one-byte
/// name, no description, no photo.
pub const HANDLE_RECORD_PAYLOAD_MIN_LEN: usize = 1
    + 1
    + 8
    + 1
    + HANDLE_RECORD_CHANNEL_ID_LEN
    + HANDLE_RECORD_KEY_LEN
    + HANDLE_RECORD_KNOCK_VERSION_LEN
    + 1
    + USERNAME_MIN_LEN
    + 1
    + 1
    + 2
    + 1
    + 1;

/// Longest payload: a group id, the longest handle, name and description,
/// and a photo with a video.
pub const HANDLE_RECORD_PAYLOAD_MAX_LEN: usize = 1
    + 1
    + 8
    + 1
    + HANDLE_RECORD_GROUP_ID_LEN
    + HANDLE_RECORD_KEY_LEN
    + HANDLE_RECORD_KNOCK_VERSION_LEN
    + 1
    + USERNAME_MAX_LEN
    + 1
    + DISPLAY_NAME_MAX_BYTES
    + 2
    + DESCRIPTION_MAX_BYTES
    + 1
    + AVATAR_FIELD_MAX_LEN;

/// Group or channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleRecordKind {
    /// A public group; `id` is its group id, `key` its knock key.
    Group,
    /// A public channel; `id` is its channel id, `key` its read key.
    Channel,
}

impl HandleRecordKind {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Group => 1,
            Self::Channel => 2,
        }
    }

    /// From the wire byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Group),
            2 => Some(Self::Channel),
            _ => None,
        }
    }

    /// The id length of this kind.
    #[must_use]
    pub const fn id_len(self) -> usize {
        match self {
            Self::Group => HANDLE_RECORD_GROUP_ID_LEN,
            Self::Channel => HANDLE_RECORD_CHANNEL_ID_LEN,
        }
    }
}

/// What a public group's or channel's record reveals to whoever holds a
/// matching OPRF output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleRecordPayload {
    /// Group or channel.
    pub kind: HandleRecordKind,
    /// Publisher-clock Unix milliseconds of this version.
    pub updated_at_ms: u64,
    /// The group id or channel id ([`HandleRecordKind::id_len`] bytes).
    pub id: Vec<u8>,
    /// The knock public key or read key.
    pub key: [u8; HANDLE_RECORD_KEY_LEN],
    /// The version of the group's knock key, named in a knock row sealed to
    /// it; always `0` for a channel.
    pub knock_version: u16,
    /// The canonical `@name`.
    pub handle: String,
    /// The name, non-empty.
    pub name: String,
    /// The description, possibly empty.
    pub description: String,
    /// The size band, `0..=5`.
    pub size_band: u8,
    /// The photo, or `None`. Its keys are public (see the module docs).
    pub avatar: Option<Avatar>,
}

/// Why a group's or channel's record payload was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandleRecordError {
    /// The payload's first byte is not [`HANDLE_RECORD_PAYLOAD_VERSION_V3`].
    #[error("unknown handle record payload version {0:#04x}")]
    UnknownVersion(u8),
    /// The kind byte is neither group nor channel.
    #[error("unknown handle record kind {0}")]
    UnknownKind(u8),
    /// A field is cut short, or bytes remain after the last.
    #[error("handle record payload is malformed")]
    BadLength,
    /// The id length does not match the kind.
    #[error("handle record id has the wrong length for its kind")]
    BadId,
    /// A channel record names a knock version other than `0`.
    #[error("a channel record carries knock version {0}, not 0")]
    ChannelKnockVersion(u16),
    /// The `@name` is not canonical.
    #[error("handle record name: {0}")]
    Handle(UsernameError),
    /// The name is empty.
    #[error("a public group or channel needs a name")]
    MissingName,
    /// A group name outside the display-name rule.
    #[error("handle record group name: {0}")]
    GroupName(DisplayNameError),
    /// A channel name or a description outside the channel-text rule.
    #[error("handle record text: {0}")]
    Text(ChannelTextError),
    /// The size band is above [`MAX_SIZE_BAND`].
    #[error("handle record size band {0} is out of range")]
    BadSizeBand(u8),
    /// The photo's length byte is not 0, 84 or 170, or the photo violates a
    /// bound.
    #[error("handle record photo: {0}")]
    Avatar(AvatarError),
    /// The record wire refused (structure, sealing, opening).
    #[error("handle record: {0}")]
    Record(IntroductionError),
}

impl HandleRecordError {
    /// Stable, language-neutral reason code.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::UnknownKind(_) => "unknownKind".to_string(),
            Self::BadLength => "badLength".to_string(),
            Self::BadId => "badId".to_string(),
            Self::ChannelKnockVersion(_) => "channelKnockVersion".to_string(),
            Self::Handle(e) => format!("handle.{}", e.code()),
            Self::MissingName => "missingName".to_string(),
            Self::GroupName(e) => format!("name.{}", e.code()),
            Self::Text(e) => format!("text.{}", e.code()),
            Self::BadSizeBand(_) => "badSizeBand".to_string(),
            Self::Avatar(e) => format!("avatar.{}", e.code()),
            Self::Record(e) => format!("record.{}", e.code()),
        }
    }
}

impl From<AvatarError> for HandleRecordError {
    fn from(e: AvatarError) -> Self {
        match e {
            AvatarError::Truncated => Self::BadLength,
            other => Self::Avatar(other),
        }
    }
}

impl From<IntroductionError> for HandleRecordError {
    fn from(e: IntroductionError) -> Self {
        Self::Record(e)
    }
}

impl HandleRecordPayload {
    /// Check every bound.
    ///
    /// # Errors
    ///
    /// The field errors of [`HandleRecordError`].
    pub fn validate(&self) -> CoreResult<(), HandleRecordError> {
        if self.id.len() != self.kind.id_len() {
            return Err(HandleRecordError::BadId);
        }
        if self.kind == HandleRecordKind::Channel && self.knock_version != 0 {
            return Err(HandleRecordError::ChannelKnockVersion(self.knock_version));
        }
        check_canonical_username(&self.handle).map_err(HandleRecordError::Handle)?;
        if self.name.is_empty() {
            return Err(HandleRecordError::MissingName);
        }
        match self.kind {
            HandleRecordKind::Group => {
                check_display_name(&self.name).map_err(HandleRecordError::GroupName)?;
            },
            HandleRecordKind::Channel => {
                check_name(&self.name).map_err(HandleRecordError::Text)?;
            },
        }
        check_description(&self.description).map_err(HandleRecordError::Text)?;
        if self.size_band > MAX_SIZE_BAND {
            return Err(HandleRecordError::BadSizeBand(self.size_band));
        }
        if let Some(avatar) = &self.avatar {
            avatar.validate().map_err(HandleRecordError::Avatar)?;
        }
        Ok(())
    }

    /// Encode the payload plaintext.
    ///
    /// # Errors
    ///
    /// What [`HandleRecordPayload::validate`] returns.
    pub fn encode(&self) -> CoreResult<Vec<u8>, HandleRecordError> {
        self.validate()?;
        let len8 = |n: usize| u8::try_from(n).map_err(|_| HandleRecordError::BadLength);
        let description_len =
            u16::try_from(self.description.len()).map_err(|_| HandleRecordError::BadLength)?;
        let mut out = Vec::with_capacity(HANDLE_RECORD_PAYLOAD_MAX_LEN);
        out.push(HANDLE_RECORD_PAYLOAD_VERSION_V3);
        out.push(self.kind.to_byte());
        out.extend_from_slice(&self.updated_at_ms.to_be_bytes());
        out.push(len8(self.id.len())?);
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.knock_version.to_be_bytes());
        out.push(len8(self.handle.len())?);
        out.extend_from_slice(self.handle.as_bytes());
        out.push(len8(self.name.len())?);
        out.extend_from_slice(self.name.as_bytes());
        out.extend_from_slice(&description_len.to_be_bytes());
        out.extend_from_slice(self.description.as_bytes());
        out.push(self.size_band);
        encode_avatar_field(self.avatar.as_ref(), &mut out);
        Ok(out)
    }

    /// Decode a payload plaintext, fail-closed.
    ///
    /// # Errors
    ///
    /// [`HandleRecordError::UnknownVersion`], [`HandleRecordError::UnknownKind`],
    /// [`HandleRecordError::BadLength`] and every field error.
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, HandleRecordError> {
        let mut cur = bytes;
        let version = take_u8(&mut cur)?;
        if version != HANDLE_RECORD_PAYLOAD_VERSION_V3 {
            return Err(HandleRecordError::UnknownVersion(version));
        }
        let kind_byte = take_u8(&mut cur)?;
        let kind = HandleRecordKind::from_byte(kind_byte)
            .ok_or(HandleRecordError::UnknownKind(kind_byte))?;
        let updated_at_ms = u64::from_be_bytes(take_array::<8>(&mut cur)?);
        let id_len = usize::from(take_u8(&mut cur)?);
        if id_len != kind.id_len() {
            return Err(HandleRecordError::BadId);
        }
        let id = take(&mut cur, id_len)?.to_vec();
        let key = take_array::<HANDLE_RECORD_KEY_LEN>(&mut cur)?;
        let knock_version = u16::from_be_bytes(take_array::<2>(&mut cur)?);
        if kind == HandleRecordKind::Channel && knock_version != 0 {
            return Err(HandleRecordError::ChannelKnockVersion(knock_version));
        }
        let handle_len = usize::from(take_u8(&mut cur)?);
        let handle = utf8(
            take(&mut cur, handle_len)?,
            HandleRecordError::Handle(UsernameError::InvalidCharacters),
        )?;
        let name_len = usize::from(take_u8(&mut cur)?);
        let name = utf8(
            take(&mut cur, name_len)?,
            HandleRecordError::GroupName(DisplayNameError::NotUtf8),
        )?;
        let description_len = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
        if description_len > DESCRIPTION_MAX_BYTES {
            return Err(HandleRecordError::Text(ChannelTextError::TooManyBytes {
                max_bytes: DESCRIPTION_MAX_BYTES,
            }));
        }
        let description = utf8(
            take(&mut cur, description_len)?,
            HandleRecordError::Text(ChannelTextError::NotUtf8),
        )?;
        let size_band = take_u8(&mut cur)?;
        let avatar = decode_avatar_field(&mut cur).map_err(HandleRecordError::from)?;
        if !cur.is_empty() {
            return Err(HandleRecordError::BadLength);
        }
        let payload = Self {
            kind,
            updated_at_ms,
            id,
            key,
            knock_version,
            handle,
            name,
            description,
            size_band,
            avatar,
        };
        payload.validate()?;
        Ok(payload)
    }
}

/// Build the public record of a public group or channel, findable by each
/// search term supplied: the OPRF output of its `@name` plus one per name
/// term.
///
/// # Errors
///
/// Every error of [`HandleRecordPayload::encode`], and the record errors
/// (no term, too many, a duplicate) as [`HandleRecordError::Record`].
pub fn compose_handle_record(
    payload: &HandleRecordPayload,
    term_oprfs: &[[u8; OPRF_OUTPUT_LEN]],
    issued_at_ms: u64,
) -> CoreResult<Vec<u8>, HandleRecordError> {
    let plaintext = payload.encode()?;
    Ok(seal_record(
        &plaintext,
        term_oprfs,
        issued_at_ms,
        PUBLIC_SEALED_BOUNDS,
    )?)
}

/// Open a public record (served by discovery) that must be a group's or a
/// channel's.
///
/// # Errors
///
/// The record errors as [`HandleRecordError::Record`], and every payload
/// error (a person's payload is [`HandleRecordError::UnknownVersion`]).
pub fn open_handle_record(
    record: &[u8],
    oprf_output: &[u8; OPRF_OUTPUT_LEN],
) -> CoreResult<Option<HandleRecordPayload>, HandleRecordError> {
    match IntroductionRecord::decode_public(record)?.open_plaintext(oprf_output)? {
        Some(plaintext) => HandleRecordPayload::decode(&plaintext).map(Some),
        None => Ok(None),
    }
}

/// What one search result opens to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRecord {
    /// A person in Everyone (never with a `fof` grant).
    Person(Box<IntroductionPayload>),
    /// A public group or channel (boxed: with its photo it is the larger
    /// variant by far).
    Conversation(Box<HandleRecordPayload>),
}

/// Open any public record a lookup or a name search returned: a person's
/// or a group's or channel's, by the payload's first byte. `Ok(None)` when
/// no entry matches the OPRF output.
///
/// # Errors
///
/// The record errors, the payload errors of the kind the first byte names,
/// and [`IntroductionError::UnexpectedFofGrant`] for a person's payload
/// with a grant.
pub fn open_search_record(
    record: &[u8],
    oprf_output: &[u8; OPRF_OUTPUT_LEN],
) -> CoreResult<Option<SearchRecord>, HandleRecordError> {
    let Some(plaintext) = IntroductionRecord::decode_public(record)?.open_plaintext(oprf_output)?
    else {
        return Ok(None);
    };
    if plaintext.first() == Some(&HANDLE_RECORD_PAYLOAD_VERSION_V3) {
        return HandleRecordPayload::decode(&plaintext)
            .map(|p| Some(SearchRecord::Conversation(Box::new(p))));
    }
    let person = IntroductionPayload::decode(&plaintext)?;
    if person.fof.is_some() {
        return Err(IntroductionError::UnexpectedFofGrant.into());
    }
    Ok(Some(SearchRecord::Person(Box::new(person))))
}

fn utf8(bytes: &[u8], err: HandleRecordError) -> CoreResult<String, HandleRecordError> {
    core::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| err)
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, HandleRecordError> {
    let (&first, rest) = cur.split_first().ok_or(HandleRecordError::BadLength)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], HandleRecordError> {
    if cur.len() < n {
        return Err(HandleRecordError::BadLength);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> CoreResult<[u8; N], HandleRecordError> {
    take(cur, N)?
        .try_into()
        .map_err(|_| HandleRecordError::BadLength)
}

/// The fuzz oracle behind `fuzz_api::handle_record_decode`: `bytes` as a
/// payload plaintext (which a fuzzer cannot reach through the AEAD), and as
/// a public record opened under a fixed OPRF output. `None` when both
/// refuse; `Some(true)` when what decoded re-encodes to the same bytes.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let _ = open_search_record(bytes, &[0x5A; OPRF_OUTPUT_LEN]);
    if let Ok(record) = IntroductionRecord::decode_public(bytes) {
        return Some(record.encode_with(PUBLIC_SEALED_BOUNDS).ok().as_deref() == Some(bytes));
    }
    let payload = HandleRecordPayload::decode(bytes).ok()?;
    Some(payload.encode().ok().as_deref() == Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introduction::{
        compose_public_record, open_introduction, open_public_record, PUBLIC_RECORD_MAX_LEN,
    };
    use crate::mailbox::MailboxBinding;
    use proptest::prelude::*;

    const HANDLE_OPRF: [u8; 64] = [0x11; 64];
    const TERM_OPRF: [u8; 64] = [0x22; 64];

    fn group() -> HandleRecordPayload {
        HandleRecordPayload {
            kind: HandleRecordKind::Group,
            updated_at_ms: 1_800_000_000_000,
            id: vec![0x33; 32],
            key: [0x44; 32],
            knock_version: 0x0102,
            handle: "hiking_club".to_string(),
            name: "Hiking club".to_string(),
            description: "Weekend walks around Berlin.".to_string(),
            size_band: 2,
            avatar: None,
        }
    }

    fn photo() -> Avatar {
        use crate::avatar::{AvatarRef, AvatarVideo};
        let reference = |byte: u8, side: u16, size: u32| AvatarRef {
            blob_id: [byte; 32],
            key: [byte.wrapping_add(1); 32],
            nonce: [byte.wrapping_add(2); 12],
            size,
            width: side,
            height: side,
        };
        Avatar {
            still: reference(0x10, 640, 40_000),
            video: Some(AvatarVideo {
                reference: reference(0x20, 480, 1_000_000),
                duration_ms: 5_000,
            }),
        }
    }

    #[test]
    fn a_group_record_opens_by_its_handle_and_its_terms() {
        let wire = compose_handle_record(&group(), &[HANDLE_OPRF, TERM_OPRF], 7).unwrap();
        for oprf in [HANDLE_OPRF, TERM_OPRF] {
            assert_eq!(open_handle_record(&wire, &oprf).unwrap(), Some(group()));
            assert_eq!(
                open_search_record(&wire, &oprf).unwrap(),
                Some(SearchRecord::Conversation(Box::new(group())))
            );
        }
        assert_eq!(open_handle_record(&wire, &[0x99; 64]).unwrap(), None);
    }

    #[test]
    fn a_group_record_never_opens_as_a_person_or_an_introduction() {
        let wire = compose_handle_record(&group(), &[HANDLE_OPRF], 7).unwrap();
        assert!(open_public_record(&wire, &HANDLE_OPRF).is_err());
        assert!(open_introduction(&wire, &HANDLE_OPRF).is_err());
    }

    #[test]
    fn a_person_record_opens_as_a_person_through_the_search_opener() {
        let seed = [0xA1; 32];
        let payload = IntroductionPayload {
            identity_pk: lakko_crypto::signing::SigningKey::from_seed(&seed)
                .verifying_key()
                .to_bytes(),
            account_id: [1; 16],
            display_name: Some("Anna".to_string()),
            username: None,
            username_certificate: None,
            mailbox_binding: MailboxBinding::for_account_seed(&seed),
            fof: None,
        };
        let wire = compose_public_record(&payload, &[HANDLE_OPRF], 1).unwrap();
        assert_eq!(
            open_search_record(&wire, &HANDLE_OPRF).unwrap(),
            Some(SearchRecord::Person(Box::new(payload)))
        );
        assert!(open_handle_record(&wire, &HANDLE_OPRF).is_err());
    }

    #[test]
    fn the_longest_record_fits_the_public_bound() {
        let mut p = group();
        p.handle = "a".repeat(32);
        p.name = "n".repeat(64);
        p.description = "\u{4e2d}".repeat(300);
        p.avatar = Some(photo());
        let terms: Vec<[u8; 64]> = (0..65u8).map(|i| [i; 64]).collect();
        let wire = compose_handle_record(&p, &terms, 1).unwrap();
        assert!(wire.len() <= PUBLIC_RECORD_MAX_LEN);
        assert_eq!(open_handle_record(&wire, &[64; 64]).unwrap(), Some(p));
    }

    #[test]
    fn bounds_are_enforced() {
        let mut p = group();
        p.name.clear();
        assert_eq!(p.encode(), Err(HandleRecordError::MissingName));
        let mut p = group();
        p.id = vec![0; 16];
        assert_eq!(p.encode(), Err(HandleRecordError::BadId));
        let mut p = group();
        p.handle = "Hiking".to_string();
        assert!(matches!(p.encode(), Err(HandleRecordError::Handle(_))));
        let mut p = group();
        p.size_band = 6;
        assert_eq!(p.encode(), Err(HandleRecordError::BadSizeBand(6)));
        let mut p = group();
        p.kind = HandleRecordKind::Channel;
        p.id = vec![0; 16];
        assert_eq!(
            p.encode(),
            Err(HandleRecordError::ChannelKnockVersion(0x0102))
        );
        p.knock_version = 0;
        assert!(p.encode().is_ok());

        let wire = group().encode().unwrap();
        assert_eq!(
            HandleRecordPayload::decode(&wire[..wire.len() - 1]),
            Err(HandleRecordError::BadLength)
        );
        let mut extra = wire.clone();
        extra.push(0);
        assert_eq!(
            HandleRecordPayload::decode(&extra),
            Err(HandleRecordError::BadLength)
        );
        let mut kind = wire;
        kind[1] = 3;
        assert_eq!(
            HandleRecordPayload::decode(&kind),
            Err(HandleRecordError::UnknownKind(3))
        );
    }

    #[test]
    fn the_photo_rides_after_the_size_band_and_is_bounded() {
        let mut p = group();
        p.avatar = Some(photo());
        let wire = p.encode().unwrap();
        assert_eq!(wire[0], 0x43);
        assert_eq!(wire.len(), group().encode().unwrap().len() + 170);
        assert_eq!(HandleRecordPayload::decode(&wire), Ok(p.clone()));
        let sealed = compose_handle_record(&p, &[HANDLE_OPRF], 3).unwrap();
        assert_eq!(open_handle_record(&sealed, &HANDLE_OPRF).unwrap(), Some(p));

        let mut not_square = group();
        let mut a = photo();
        a.still.height = 600;
        not_square.avatar = Some(a.clone());
        assert_eq!(
            not_square.encode(),
            Err(HandleRecordError::Avatar(AvatarError::NotSquare))
        );
        // The same bytes on the wire, past encode.
        let mut wire = group().encode().unwrap();
        wire.pop();
        encode_avatar_field(Some(&a), &mut wire);
        assert_eq!(
            HandleRecordPayload::decode(&wire),
            Err(HandleRecordError::Avatar(AvatarError::NotSquare))
        );
    }

    #[test]
    fn earlier_payload_versions_are_refused() {
        for old in [0x41, 0x42] {
            let mut wire = group().encode().unwrap();
            wire[0] = old;
            assert_eq!(
                HandleRecordPayload::decode(&wire),
                Err(HandleRecordError::UnknownVersion(old))
            );
        }
    }

    #[test]
    fn the_knock_version_rides_after_the_key_big_endian() {
        let wire = group().encode().unwrap();
        // version, kind, updated_at_ms, id_len, id, key.
        let at = 1 + 1 + 8 + 1 + HANDLE_RECORD_GROUP_ID_LEN + HANDLE_RECORD_KEY_LEN;
        assert_eq!(&wire[at..at + 2], &[0x01, 0x02]);
        let mut p = group();
        p.knock_version = u16::MAX;
        let wire = p.encode().unwrap();
        assert_eq!(HandleRecordPayload::decode(&wire), Ok(p.clone()));
        let sealed = compose_handle_record(&p, &[HANDLE_OPRF], 3).unwrap();
        assert_eq!(
            open_handle_record(&sealed, &HANDLE_OPRF)
                .unwrap()
                .map(|q| q.knock_version),
            Some(u16::MAX)
        );
    }

    #[test]
    fn a_channel_record_with_a_knock_version_is_refused_on_decode() {
        let mut channel = group();
        channel.kind = HandleRecordKind::Channel;
        channel.id = vec![0x55; HANDLE_RECORD_CHANNEL_ID_LEN];
        channel.knock_version = 0;
        let mut wire = channel.encode().unwrap();
        assert_eq!(HandleRecordPayload::decode(&wire), Ok(channel));
        let at = 1 + 1 + 8 + 1 + HANDLE_RECORD_CHANNEL_ID_LEN + HANDLE_RECORD_KEY_LEN;
        wire[at + 1] = 1;
        assert_eq!(
            HandleRecordPayload::decode(&wire),
            Err(HandleRecordError::ChannelKnockVersion(1))
        );
    }

    #[test]
    fn payload_bounds_are_the_documented_construction() {
        assert_eq!(HANDLE_RECORD_PAYLOAD_MIN_LEN, 71);
        assert_eq!(HANDLE_RECORD_PAYLOAD_MAX_LEN, 1564);
        assert_eq!(PUBLIC_RECORD_MAX_LEN, 7584);
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_is_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(p) = HandleRecordPayload::decode(&bytes) {
                prop_assert_eq!(p.encode().unwrap(), bytes);
            }
        }
    }
}
