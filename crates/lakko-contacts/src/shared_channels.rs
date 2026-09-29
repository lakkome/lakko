// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shared-channels envelope — the listed channels one member follows,
//! shared with a group they opted in for
//! ([ADR-0059 §6](../../../docs/adr/0059-channel-discover.md)).
//!
//! ## What travels, and where
//!
//! An MLS application message on the group's own journal, classified by
//! [`crate::incoming_envelope`] so it is never rendered as a chat bubble and
//! suppressed by the three background decrypt paths like every sibling
//! service envelope. The server sees ciphertext only (invariants #1, #2, #5):
//! who follows what never reaches it.
//!
//! ## What the envelope does NOT establish
//!
//! * **It speaks only for its sender.** Any member may send one; the
//!   receiver stores the newest list per member, keyed on the member's
//!   **account key resolved at receipt** (C-68), never on anything in the
//!   payload — the envelope carries no identity of its own.
//! * **The order** is [`shared_channels_is_newer`]: strictly greater stamp
//!   wins per member, equal is not newer, consulted against the current
//!   stored stamp under the per-group lock (C-62 sub-invariant 5).
//! * **Nothing here is verified**: a handle and a name are the sender's
//!   claim. A client resolves the handle through the OPRF lookup before it
//!   subscribes (ADR-0059 §3), exactly as for a directory card.
//! * An **empty list** is admissible and means "I no longer share"; it is
//!   what a sender publishes when the switch goes off.
//! * Which channels are included (only those whose own metadata says
//!   `listed`) is the sender's discipline; a receiver cannot check it.
//!
//! ## Wire shape
//!
//! ```text
//!   SHARED_CHANNELS_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || count
//!   └──────── 25 bytes, ASCII ───────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └ 1 B ┘
//!
//!     || count x [ channel_id(16) || handle_len(1) || handle || name_len(1) || name ]
//!
//!                     "lakko/shared-channels/v1\x00"
//! ```
//!
//! `count` is at most [`SHARED_CHANNELS_MAX`]. Total length is between
//! [`SHARED_CHANNELS_ENVELOPE_MIN_LEN`] and
//! [`SHARED_CHANNELS_ENVELOPE_MAX_LEN`].
//!
//! ## Bounds
//!
//! * `handle`: the canonical username rule
//!   ([`crate::username::check_canonical_username`]).
//! * `name`: the ADR-0051 channel-name rule ([`crate::channel_text`]), the
//!   same one the channel's own metadata record uses, so a name the channel
//!   stated is always a name a member can share.
//! * no `channel_id` twice.
//!
//! An envelope violating any bound is **rejected as a whole**;
//! **`compose_shared_channels` refuses exactly what `decode_shared_channels`
//! refuses**, both running [`SharedChannels::validate`]. Every accepted
//! encoding re-composes to the identical bytes.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::channel_text::{check_name, ChannelTextError, NAME_MAX_BYTES};
use crate::username::{check_canonical_username, UsernameError, USERNAME_MAX_LEN};

/// 25-byte domain separator. **Frozen**: [`crate::incoming_envelope`]
/// dispatches on this exact prefix; the in-payload version byte moves.
pub const SHARED_CHANNELS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/shared-channels/v1\x00";

/// Schema-version byte carried first after the separator.
pub const SHARED_CHANNELS_VERSION_V1: u8 = 0x01;

/// Channel id length.
pub const SHARED_CHANNEL_ID_LEN: usize = 16;

/// Most channels one envelope may carry (ADR-0059 §6).
pub const SHARED_CHANNELS_MAX: usize = 32;

/// Bytes after the separator every envelope carries: version,
/// `updated_at_ms`, `count`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1;

/// Fixed bytes per entry: `channel_id`, `handle_len`, `name_len`.
const ENTRY_FIXED_LEN: usize = SHARED_CHANNEL_ID_LEN + 1 + 1;

/// Shortest well-formed envelope: an empty list.
pub const SHARED_CHANNELS_ENVELOPE_MIN_LEN: usize =
    SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Longest well-formed envelope: 32 maximal entries.
pub const SHARED_CHANNELS_ENVELOPE_MAX_LEN: usize = SHARED_CHANNELS_ENVELOPE_MIN_LEN
    + SHARED_CHANNELS_MAX * (ENTRY_FIXED_LEN + USERNAME_MAX_LEN + NAME_MAX_BYTES);

/// One channel a member shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedChannel {
    /// The channel's 16-byte id.
    pub channel_id: [u8; SHARED_CHANNEL_ID_LEN],
    /// The channel's handle, canonical.
    pub handle: String,
    /// The channel name, as the channel's metadata stated it.
    pub name: String,
}

/// The list of channels one member shares with one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedChannels {
    /// Sender-clock UNIX milliseconds; orders lists of one member.
    pub updated_at_ms: u64,
    /// At most [`SHARED_CHANNELS_MAX`] channels, no id twice. Empty means
    /// "no longer sharing".
    pub channels: Vec<SharedChannel>,
}

impl SharedChannels {
    /// Check every bound. The single rule compose and decode both run.
    ///
    /// # Errors
    ///
    /// The first violation, in list order.
    pub fn validate(&self) -> CoreResult<(), SharedChannelsError> {
        if self.channels.len() > SHARED_CHANNELS_MAX {
            return Err(SharedChannelsError::TooManyChannels {
                count: self.channels.len(),
            });
        }
        for (i, channel) in self.channels.iter().enumerate() {
            check_canonical_username(&channel.handle).map_err(SharedChannelsError::Handle)?;
            check_name(&channel.name).map_err(SharedChannelsError::Name)?;
            if self.channels[..i]
                .iter()
                .any(|c| c.channel_id == channel.channel_id)
            {
                return Err(SharedChannelsError::DuplicateChannel);
            }
        }
        Ok(())
    }
}

/// Why a shared-channels envelope was refused. Reason codes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SharedChannelsError {
    /// The plaintext does not carry the domain separator. Only from
    /// [`decode_shared_channels`]; [`try_unwrap_shared_channels`] falls
    /// through with `Ok(None)`.
    #[error("plaintext does not carry the shared-channels domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("shared-channels envelope is truncated")]
    Truncated,
    /// Bytes remain after the last entry.
    #[error("shared-channels envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the last entry.
        count: usize,
    },
    /// The version byte is not [`SHARED_CHANNELS_VERSION_V1`].
    #[error("shared-channels envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// More than [`SHARED_CHANNELS_MAX`] channels.
    #[error("shared-channels envelope carries {count} channels")]
    TooManyChannels {
        /// The declared or supplied count.
        count: usize,
    },
    /// A handle is not canonical.
    #[error("invalid shared channel handle: {0}")]
    Handle(UsernameError),
    /// A name violates the channel-name rule.
    #[error("invalid shared channel name: {0}")]
    Name(ChannelTextError),
    /// A channel id appears twice.
    #[error("shared-channels envelope names a channel twice")]
    DuplicateChannel,
}

impl SharedChannelsError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::TooManyChannels { .. } => "tooManyChannels".to_string(),
            Self::Handle(e) => format!("handle.{}", e.code()),
            Self::Name(e) => format!("name.{}", e.code()),
            Self::DuplicateChannel => "duplicateChannel".to_string(),
        }
    }
}

/// `true` when a list stamped `incoming_updated_at_ms` must replace the list
/// stored for the **same member**. Strictly greater; equal is not newer.
#[must_use]
pub const fn shared_channels_is_newer(
    incoming_updated_at_ms: u64,
    stored_updated_at_ms: u64,
) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a shared-channels envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// # Errors
///
/// Exactly the errors [`SharedChannels::validate`] returns.
pub fn compose_shared_channels(list: &SharedChannels) -> CoreResult<Vec<u8>, SharedChannelsError> {
    list.validate()?;
    let count =
        u8::try_from(list.channels.len()).map_err(|_| SharedChannelsError::TooManyChannels {
            count: list.channels.len(),
        })?;
    let mut out = Vec::with_capacity(
        SHARED_CHANNELS_ENVELOPE_MIN_LEN
            + list
                .channels
                .iter()
                .map(|c| ENTRY_FIXED_LEN + c.handle.len() + c.name.len())
                .sum::<usize>(),
    );
    out.extend_from_slice(SHARED_CHANNELS_ENVELOPE_DOMAIN_V1);
    out.push(SHARED_CHANNELS_VERSION_V1);
    out.extend_from_slice(&list.updated_at_ms.to_be_bytes());
    out.push(count);
    for channel in &list.channels {
        // `validate` bounded both lengths; re-checked rather than cast.
        let handle_len = u8::try_from(channel.handle.len()).map_err(|_| {
            SharedChannelsError::Handle(UsernameError::TooLong {
                max: USERNAME_MAX_LEN,
            })
        })?;
        let name_len = u8::try_from(channel.name.len()).map_err(|_| {
            SharedChannelsError::Name(ChannelTextError::TooManyBytes {
                max_bytes: NAME_MAX_BYTES,
            })
        })?;
        out.extend_from_slice(&channel.channel_id);
        out.push(handle_len);
        out.extend_from_slice(channel.handle.as_bytes());
        out.push(name_len);
        out.extend_from_slice(channel.name.as_bytes());
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(list))` — the separator matched and the envelope is valid.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed; the caller
///   drops it (fail-closed), never renders it.
///
/// The version is checked before anything else, and `count` is checked
/// against [`SHARED_CHANNELS_MAX`] before any entry is read, so a hostile
/// count allocates nothing.
///
/// # Errors
///
/// Any [`SharedChannelsError`] except `WrongDomain`.
pub fn try_unwrap_shared_channels(
    plaintext: &[u8],
) -> CoreResult<Option<SharedChannels>, SharedChannelsError> {
    let Some(mut cur) = plaintext.strip_prefix(SHARED_CHANNELS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let version = take_u8(&mut cur)?;
    if version != SHARED_CHANNELS_VERSION_V1 {
        return Err(SharedChannelsError::UnknownVersion(version));
    }
    let updated_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| SharedChannelsError::Truncated)?;
    let count = usize::from(take_u8(&mut cur)?);
    if count > SHARED_CHANNELS_MAX {
        return Err(SharedChannelsError::TooManyChannels { count });
    }
    let mut channels = Vec::with_capacity(count);
    for _ in 0..count {
        let channel_id: [u8; SHARED_CHANNEL_ID_LEN] = take(&mut cur, SHARED_CHANNEL_ID_LEN)?
            .try_into()
            .map_err(|_| SharedChannelsError::Truncated)?;
        let handle_len = usize::from(take_u8(&mut cur)?);
        let handle = take(&mut cur, handle_len)?;
        let name_len = usize::from(take_u8(&mut cur)?);
        let name = take(&mut cur, name_len)?;
        let handle = core::str::from_utf8(handle)
            .map_err(|_| SharedChannelsError::Handle(UsernameError::InvalidCharacters))?;
        let name = core::str::from_utf8(name)
            .map_err(|_| SharedChannelsError::Name(ChannelTextError::NotUtf8))?;
        channels.push(SharedChannel {
            channel_id,
            handle: handle.to_string(),
            name: name.to_string(),
        });
    }
    if !cur.is_empty() {
        return Err(SharedChannelsError::TrailingBytes { count: cur.len() });
    }
    let list = SharedChannels {
        updated_at_ms: u64::from_be_bytes(updated_at_bytes),
        channels,
    };
    list.validate()?;
    Ok(Some(list))
}

/// Strict standalone decoder: a plaintext that is not this envelope is a
/// hard error.
///
/// # Errors
///
/// [`SharedChannelsError::WrongDomain`], or any error
/// [`try_unwrap_shared_channels`] returns.
pub fn decode_shared_channels(bytes: &[u8]) -> CoreResult<SharedChannels, SharedChannelsError> {
    try_unwrap_shared_channels(bytes)?.ok_or(SharedChannelsError::WrongDomain)
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, SharedChannelsError> {
    let (&first, rest) = cur.split_first().ok_or(SharedChannelsError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], SharedChannelsError> {
    if cur.len() < n {
        return Err(SharedChannelsError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: u8, handle: &str, name: &str) -> SharedChannel {
        SharedChannel {
            channel_id: [id; SHARED_CHANNEL_ID_LEN],
            handle: handle.to_string(),
            name: name.to_string(),
        }
    }

    fn list(channels: Vec<SharedChannel>) -> SharedChannels {
        SharedChannels {
            updated_at_ms: 1_700_000_000_000,
            channels,
        }
    }

    #[test]
    fn compose_then_decode_round_trips() {
        for l in [
            list(Vec::new()),
            list(vec![channel(1, "lakko_news", "Lakko News")]),
            list(vec![
                channel(1, "abc", "A"),
                channel(2, "cooking_daily", "\u{1F373} Cooking"),
            ]),
        ] {
            let wire = compose_shared_channels(&l).unwrap();
            assert_eq!(decode_shared_channels(&wire).unwrap(), l);
            assert_eq!(try_unwrap_shared_channels(&wire).unwrap(), Some(l));
        }
    }

    #[test]
    fn layout_and_bounds_are_pinned() {
        assert_eq!(SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.len(), 25);
        assert_eq!(SHARED_CHANNELS_ENVELOPE_MIN_LEN, 35);
        assert_eq!(SHARED_CHANNELS_ENVELOPE_MAX_LEN, 35 + 32 * (18 + 32 + 255));
        let mut l = list(vec![channel(0xAB, "abc", "N")]);
        l.updated_at_ms = 0x0102_0304_0506_0708;
        let wire = compose_shared_channels(&l).unwrap();
        let sep = 25;
        assert_eq!(&wire[..sep], b"lakko/shared-channels/v1\x00");
        assert_eq!(wire[sep], 0x01);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 1);
        assert_eq!(&wire[sep + 10..sep + 26], &[0xAB; 16]);
        assert_eq!(wire[sep + 26], 3);
        assert_eq!(&wire[sep + 27..sep + 30], b"abc");
        assert_eq!(wire[sep + 30], 1);
        assert_eq!(&wire[sep + 31..], b"N");

        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        let handle = "a".repeat(32);
        let max = list(
            (0..32u8)
                .map(|i| channel(i, &handle, &name))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            compose_shared_channels(&max).unwrap().len(),
            SHARED_CHANNELS_ENVELOPE_MAX_LEN
        );
        let empty = compose_shared_channels(&list(Vec::new())).unwrap();
        assert_eq!(empty.len(), SHARED_CHANNELS_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_a_bare_separator_is_an_error() {
        assert_eq!(try_unwrap_shared_channels(b"hello").unwrap(), None);
        assert_eq!(
            decode_shared_channels(b"hello"),
            Err(SharedChannelsError::WrongDomain)
        );
        assert_eq!(
            try_unwrap_shared_channels(SHARED_CHANNELS_ENVELOPE_DOMAIN_V1),
            Err(SharedChannelsError::Truncated)
        );
    }

    #[test]
    fn every_truncation_and_trailing_byte_is_refused() {
        let wire =
            compose_shared_channels(&list(vec![channel(1, "abc", "A"), channel(2, "def", "B")]))
                .unwrap();
        for cut in SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_shared_channels(&wire[..cut]),
                Err(SharedChannelsError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire;
        long.push(0);
        assert_eq!(
            try_unwrap_shared_channels(&long),
            Err(SharedChannelsError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn version_and_count_are_checked_before_any_entry() {
        let mut v2 = SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(
            try_unwrap_shared_channels(&v2),
            Err(SharedChannelsError::UnknownVersion(0x02))
        );
        let mut many = SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.to_vec();
        many.push(0x01);
        many.extend_from_slice(&0u64.to_be_bytes());
        many.push(33);
        assert_eq!(
            try_unwrap_shared_channels(&many),
            Err(SharedChannelsError::TooManyChannels { count: 33 })
        );
    }

    #[test]
    fn decode_refuses_what_compose_refuses() {
        for (bad, code) in [
            (list(vec![channel(1, "ab", "A")]), "handle.tooShort"),
            (
                list(vec![channel(1, "Abc", "A")]),
                "handle.invalidCharacters",
            ),
            (list(vec![channel(1, "abc", "")]), "name.empty"),
            (
                list(vec![channel(1, "abc", "A\u{202E}")]),
                "name.controlOrFormatCharacter",
            ),
            (
                list(vec![channel(1, "abc", "A"), channel(1, "def", "B")]),
                "duplicateChannel",
            ),
        ] {
            let err = compose_shared_channels(&bad).unwrap_err();
            assert_eq!(err.code(), code);
            let mut raw = SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.to_vec();
            raw.push(1);
            raw.extend_from_slice(&bad.updated_at_ms.to_be_bytes());
            raw.push(u8::try_from(bad.channels.len()).unwrap());
            for c in &bad.channels {
                raw.extend_from_slice(&c.channel_id);
                raw.push(u8::try_from(c.handle.len()).unwrap());
                raw.extend_from_slice(c.handle.as_bytes());
                raw.push(u8::try_from(c.name.len()).unwrap());
                raw.extend_from_slice(c.name.as_bytes());
            }
            assert_eq!(try_unwrap_shared_channels(&raw).unwrap_err(), err);
        }
        let too_many = list((0..33u8).map(|i| channel(i, "abc", "A")).collect());
        assert_eq!(
            compose_shared_channels(&too_many).unwrap_err(),
            SharedChannelsError::TooManyChannels { count: 33 }
        );
    }

    #[test]
    fn newer_is_strictly_greater() {
        assert!(shared_channels_is_newer(2, 1));
        assert!(!shared_channels_is_newer(1, 1));
        assert!(!shared_channels_is_newer(0, 1));
    }

    #[test]
    fn error_messages_never_echo_the_payload() {
        let err =
            compose_shared_channels(&list(vec![channel(1, "abc", "Secret\u{202E}")])).unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        assert!(!format!("{err:?}").contains("Secret"));
    }

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
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, SHARED_CHANNELS_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(SHARED_CHANNELS_ENVELOPE_DOMAIN_V1)
                    && !SHARED_CHANNELS_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }
}
