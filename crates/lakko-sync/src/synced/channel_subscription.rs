// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's channel subscriptions, carried between its own devices
//! (maintainer decision of 2026-11-13; ADR-0050 section 2.9 and ADR-0064
//! section 7 as amended that day).
//!
//! A channel is not an MLS group the account's devices are all in: the
//! channel service knows a subscriber only by a subscriber key, and before
//! this register a second device of the account had no way to learn which
//! channels the account reads or which key the service knows it by. The
//! register names every channel the account is subscribed to, the ONE
//! subscriber key the account holds for it (one per account and channel,
//! shared by its devices, ADR-0064 section 7: two devices holding one key is
//! safe because a subscriber key is not ratchet state), and the little a
//! reader needs beside it.
//!
//! ## The register
//!
//! `channel id -> subscribed { role, subscriber secret, read key, publisher
//! group id, name, handle } | left`. One register per channel the account
//! ever subscribed to; folded by the ordinary rule ([`super::state`]): the
//! greater stamp wins, and on an exact tie the larger canonical bytes, where
//! the state byte follows the key and "subscribed" is the larger byte, so a
//! tie never drops a subscription (the conversation-end rule: a tie must not
//! delete a conversation). A leave is kept as a tombstone, so a device that
//! was offline across it cannot bring the subscription back with an older
//! stamp; a re-subscribe after a leave is an ordinary later write and wins.
//! Unlike a link revocation a leave is NOT final.
//!
//! - `role`: what the account is in the channel. A second device of an owner
//!   or a co-publisher is a READER for now (maintainer, 2026-11-13): it lists
//!   the channel and reads its posts with the shared subscriber key, and
//!   posts, approves, rotates and admits nothing. The role tells it which
//!   controls to hide.
//! - `subscriber_secret`: the 32-byte Ed25519 seed of the account's
//!   subscriber key for the channel (the X25519 seed-envelope key is
//!   derived from it). SECRET: whoever holds it reads the channel.
//! - `read_key`: the channel's 32-byte read key (ADR-0087) when held, else
//!   none. SECRET.
//! - `publisher_group_id`: for a co-publisher, the 32-byte PublisherGroup id
//!   it joined (not derivable: it came from the `/g/` link); none for every
//!   other role (an owner's is derived from the account seed).
//! - `owner_account_key`: for a co-publisher, the channel owner's 32-byte
//!   account key, recorded by the device that joined from the issuer of the
//!   `/g/` capability it verified (ADR-0090 section 2.3). It is the
//!   AUTHORITY a co-publisher's other devices use: a co-publisher device
//!   accepts a PublisherGroup Welcome only from a device of this key
//!   (`PublisherGroup::join_from_welcome` takes it), never from what the
//!   Welcome says about itself. Required for a co-publisher; absent for an
//!   owner (the owner's key is the account's own key, derived from the
//!   account seed, and recording it could only disagree with it) and for a
//!   subscriber.
//! - `name`, `handle`: what the channel list falls back to before the
//!   channel's own metadata record is read; each may be empty.
//!
//! What it deliberately does NOT carry: epoch seeds (they change on every
//! rotation, so carrying them would cost a DeviceGroup message per channel
//! per rotation; the pairing state bundle carries the newest ones once, and
//! the channel service retains the newest four envelopes per key), publisher
//! signing seeds and publish counters (a publishing device mints its own,
//! G-51), and PublisherGroup MLS state (one leaf on two devices is ADR-0050
//! section 2.9a's reason 3).
//!
//! [`CHANNEL_SUBSCRIPTIONS_MAX_CLIENT`] is a client-side limit on live
//! subscriptions, not a decode bound. Tombstones are never dropped (dropping
//! one lets a late envelope resurrect the subscription).
//!
//! ## Wire
//!
//! ```text
//! domain(35) "lakko/sync/channel-subscription/v1\0"
//! version(1) = 0x02            (0x01 refused: it carried no owner key)
//! count(1)                      1..=16
//! count x {
//!     hlc(8, BE)                the register's ORIGINAL stamp
//!     channel_id(16)
//!     state(1)                  1 left, 2 subscribed
//!     -- only when subscribed --
//!     role(1)                   1 subscriber, 2 owner, 3 co-publisher
//!     subscriber_secret(32)
//!     read_key_len(1)           0 or 32
//!     read_key
//!     publisher_group_id_len(1) 32 for a co-publisher, else 0
//!     publisher_group_id
//!     owner_account_key_len(1)  32 for a co-publisher, else 0
//!     owner_account_key
//!     name_len(1)               0..=255, a display name when not empty
//!     name
//!     handle_len(1)             0 or 3..=32, a canonical name when not empty
//!     handle
//! }
//! ```
//!
//! Entries are sorted by channel id, strictly increasing; nothing may
//! trail; the encoding is canonical and every violation refuses the whole
//! envelope. It is not in the growth snapshot (whose wire codec lives in
//! `lakko-mls`): a device re-sends
//! [`super::SyncedState::channel_subscription_envelopes`] on DeviceGroup
//! growth, as it does the link records, and [`super::SyncedState::to_bytes`]
//! persists it, so the backup carries it.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account (inside a
//! DeviceGroup application message) or from a backup. A hostile device of
//! the account already holds every subscriber key it could name here. The
//! decoder is bounded and total; the secrets never reach `Debug`.

use thiserror::Error;

use super::entry::{take_n, take_u64, take_u8, BoundsError, CodecError, Stamped};

/// Length of a channel id.
pub const CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN: usize = 16;

/// Length of the subscriber secret (an Ed25519 seed).
pub const CHANNEL_SUBSCRIPTION_SECRET_LEN: usize = 32;

/// Length of a channel read key (ADR-0087).
pub const CHANNEL_SUBSCRIPTION_READ_KEY_LEN: usize = 32;

/// Length of a PublisherGroup id (ADR-0078 phase 6).
pub const CHANNEL_SUBSCRIPTION_GROUP_ID_LEN: usize = 32;

/// The most bytes a stored channel name takes (the display-name bound).
pub const CHANNEL_SUBSCRIPTION_NAME_MAX_BYTES: usize = 255;

/// The most bytes a stored handle takes (the canonical-name bound).
pub const CHANNEL_SUBSCRIPTION_HANDLE_MAX_BYTES: usize = 32;

/// The most live subscriptions a client writes. Chosen, not measured. The
/// fold never refuses an entry for it; a removal is always written.
pub const CHANNEL_SUBSCRIPTIONS_MAX_CLIENT: usize = 1000;

/// Domain separator of the channel-subscription envelope, trailing NUL
/// included. Frozen.
pub const CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/channel-subscription/v1\0";

/// The version byte this build writes and accepts.
pub const CHANNEL_SUBSCRIPTION_ENVELOPE_VERSION_V1: u8 = 0x02;

/// Length of the channel owner's account key a co-publisher records.
pub const CHANNEL_SUBSCRIPTION_OWNER_KEY_LEN: usize = 32;

/// Most entries one envelope carries.
pub const CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_ENTRIES: usize = 16;

const STATE_LEFT: u8 = 1;
const STATE_SUBSCRIBED: u8 = 2;

/// The longest entry: hlc, id, state, role, secret, and the four
/// length-prefixed fields at their bounds.
const ENTRY_MAX_LEN: usize = 8
    + CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN
    + 1
    + 1
    + CHANNEL_SUBSCRIPTION_SECRET_LEN
    + 1
    + CHANNEL_SUBSCRIPTION_READ_KEY_LEN
    + 1
    + CHANNEL_SUBSCRIPTION_GROUP_ID_LEN
    + 1
    + CHANNEL_SUBSCRIPTION_OWNER_KEY_LEN
    + 1
    + CHANNEL_SUBSCRIPTION_NAME_MAX_BYTES
    + 1
    + CHANNEL_SUBSCRIPTION_HANDLE_MAX_BYTES;

/// Shortest valid envelope: the header and one tombstone.
pub const CHANNEL_SUBSCRIPTION_ENVELOPE_MIN_LEN: usize = CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1
    .len()
    + 1
    + 1
    + 8
    + CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN
    + 1;

/// Longest valid envelope.
pub const CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN: usize = CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1
    .len()
    + 1
    + 1
    + CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_ENTRIES * ENTRY_MAX_LEN;

/// What the account is in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChannelSubscriptionRole {
    /// A reader.
    Subscriber,
    /// The channel's owner (its PublisherGroup id derives from the account
    /// seed).
    Owner,
    /// A co-publisher (ADR-0028 OQ3, "publisher" on screen).
    CoPublisher,
}

impl ChannelSubscriptionRole {
    /// The wire byte. Frozen.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Subscriber => 1,
            Self::Owner => 2,
            Self::CoPublisher => 3,
        }
    }

    /// Read the wire byte.
    ///
    /// # Errors
    ///
    /// [`BoundsError::ChannelSubscriptionRole`] for any other byte.
    pub const fn from_wire(b: u8) -> Result<Self, BoundsError> {
        match b {
            1 => Ok(Self::Subscriber),
            2 => Ok(Self::Owner),
            3 => Ok(Self::CoPublisher),
            _ => Err(BoundsError::ChannelSubscriptionRole),
        }
    }
}

/// A live subscription.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelSubscribed {
    /// What the account is in the channel.
    pub role: ChannelSubscriptionRole,
    /// The Ed25519 seed of the account's subscriber key for the channel.
    /// SECRET.
    pub subscriber_secret: [u8; CHANNEL_SUBSCRIPTION_SECRET_LEN],
    /// The channel's read key when held. SECRET.
    pub read_key: Option<[u8; CHANNEL_SUBSCRIPTION_READ_KEY_LEN]>,
    /// The PublisherGroup id a co-publisher joined; `None` for every other
    /// role.
    pub publisher_group_id: Option<[u8; CHANNEL_SUBSCRIPTION_GROUP_ID_LEN]>,
    /// The channel owner's account key a co-publisher recorded from the
    /// verified `/g/` capability issuer; `None` for every other role. A
    /// co-publisher device accepts a PublisherGroup Welcome only from a
    /// device of this key.
    pub owner_account_key: Option<[u8; CHANNEL_SUBSCRIPTION_OWNER_KEY_LEN]>,
    /// The channel's name as this account last knew it, or empty.
    pub name: String,
    /// The channel's `@name` without the `@`, or empty.
    pub handle: String,
}

impl core::fmt::Debug for ChannelSubscribed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The subscriber secret reads the channel and the read key opens it:
        // both are left out.
        f.debug_struct("ChannelSubscribed")
            .field("role", &self.role)
            .field("has_read_key", &self.read_key.is_some())
            .field("has_publisher_group_id", &self.publisher_group_id.is_some())
            .field("has_owner_account_key", &self.owner_account_key.is_some())
            .field("name_len", &self.name.len())
            .field("handle_len", &self.handle.len())
            .finish_non_exhaustive()
    }
}

impl ChannelSubscribed {
    /// Check the bounds a decoder checks.
    ///
    /// # Errors
    ///
    /// [`BoundsError::ChannelSubscriptionPublisherGroup`] when a
    /// PublisherGroup id is present for a role other than co-publisher, or
    /// absent for a co-publisher; [`BoundsError::ChannelSubscriptionName`]
    /// for a name that is neither empty nor a display name;
    /// [`BoundsError::ChannelSubscriptionHandle`] for a handle that is
    /// neither empty nor a canonical name.
    pub fn validate(&self) -> Result<(), BoundsError> {
        let co_publisher = self.role == ChannelSubscriptionRole::CoPublisher;
        if co_publisher != self.publisher_group_id.is_some() {
            return Err(BoundsError::ChannelSubscriptionPublisherGroup);
        }
        if co_publisher != self.owner_account_key.is_some() {
            return Err(BoundsError::ChannelSubscriptionOwnerKey);
        }
        if !self.name.is_empty()
            && (self.name.len() > CHANNEL_SUBSCRIPTION_NAME_MAX_BYTES
                || lakko_contacts::profile::check_display_name(&self.name).is_err())
        {
            return Err(BoundsError::ChannelSubscriptionName);
        }
        if !self.handle.is_empty()
            && (self.handle.len() > CHANNEL_SUBSCRIPTION_HANDLE_MAX_BYTES
                || lakko_contacts::username::check_canonical_username(&self.handle).is_err())
        {
            return Err(BoundsError::ChannelSubscriptionHandle);
        }
        Ok(())
    }
}

/// One channel-subscription register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSubscriptionEntry {
    /// The channel.
    pub channel_id: [u8; CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN],
    /// The subscription, or `None` for the tombstone of a left channel.
    pub subscribed: Option<ChannelSubscribed>,
}

impl ChannelSubscriptionEntry {
    /// The tombstone of a left channel.
    #[must_use]
    pub const fn left(channel_id: [u8; CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN]) -> Self {
        Self {
            channel_id,
            subscribed: None,
        }
    }

    /// Check the bounds a decoder checks.
    ///
    /// # Errors
    ///
    /// See [`ChannelSubscribed::validate`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        self.subscribed
            .as_ref()
            .map_or(Ok(()), ChannelSubscribed::validate)
    }

    /// `true` while the account is subscribed.
    #[must_use]
    pub const fn is_subscribed(&self) -> bool {
        self.subscribed.is_some()
    }

    /// Append the body to `out`: the key, the state byte, then the fields of
    /// a live subscription. Call only on a validated entry.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_id);
        let Some(s) = &self.subscribed else {
            out.push(STATE_LEFT);
            return;
        };
        out.push(STATE_SUBSCRIBED);
        out.push(s.role.to_wire());
        out.extend_from_slice(&s.subscriber_secret);
        put_opt(out, s.read_key.as_ref().map(<[u8; 32]>::as_slice));
        put_opt(out, s.publisher_group_id.as_ref().map(<[u8; 32]>::as_slice));
        put_opt(out, s.owner_account_key.as_ref().map(<[u8; 32]>::as_slice));
        put_opt(out, Some(s.name.as_bytes()));
        put_opt(out, Some(s.handle.as_bytes()));
    }

    /// Read a body written by [`Self::encode_body`], then validate it. Every
    /// length is checked against its bound before the bytes are taken.
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let mut channel_id = [0u8; CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(take_n(cur, CHANNEL_SUBSCRIPTION_CHANNEL_ID_LEN)?);
        let subscribed = match take_u8(cur)? {
            STATE_LEFT => None,
            STATE_SUBSCRIBED => {
                let role = ChannelSubscriptionRole::from_wire(take_u8(cur)?)?;
                let mut subscriber_secret = [0u8; CHANNEL_SUBSCRIPTION_SECRET_LEN];
                subscriber_secret.copy_from_slice(take_n(cur, CHANNEL_SUBSCRIPTION_SECRET_LEN)?);
                let read_key = take_opt32(cur, BoundsError::ChannelSubscriptionReadKey)?;
                let publisher_group_id =
                    take_opt32(cur, BoundsError::ChannelSubscriptionPublisherGroup)?;
                let owner_account_key = take_opt32(cur, BoundsError::ChannelSubscriptionOwnerKey)?;
                let name = take_text(
                    cur,
                    CHANNEL_SUBSCRIPTION_NAME_MAX_BYTES,
                    BoundsError::ChannelSubscriptionName,
                )?;
                let handle = take_text(
                    cur,
                    CHANNEL_SUBSCRIPTION_HANDLE_MAX_BYTES,
                    BoundsError::ChannelSubscriptionHandle,
                )?;
                Some(ChannelSubscribed {
                    role,
                    subscriber_secret,
                    read_key,
                    publisher_group_id,
                    owner_account_key,
                    name,
                    handle,
                })
            },
            _ => return Err(BoundsError::ChannelSubscriptionState.into()),
        };
        let entry = Self {
            channel_id,
            subscribed,
        };
        entry.validate()?;
        Ok(entry)
    }
}

/// A one-byte length then the bytes. Validated fields are at most 255 bytes.
fn put_opt(out: &mut Vec<u8>, bytes: Option<&[u8]>) {
    let bytes = bytes.unwrap_or_default();
    out.push(u8::try_from(bytes.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(bytes);
}

fn take_opt32(cur: &mut &[u8], err: BoundsError) -> Result<Option<[u8; 32]>, CodecError> {
    match usize::from(take_u8(cur)?) {
        0 => Ok(None),
        32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(take_n(cur, 32)?);
            Ok(Some(a))
        },
        _ => Err(err.into()),
    }
}

fn take_text(cur: &mut &[u8], max: usize, err: BoundsError) -> Result<String, CodecError> {
    let n = usize::from(take_u8(cur)?);
    if n > max {
        return Err(err.into());
    }
    String::from_utf8(take_n(cur, n)?.to_vec()).map_err(|_| CodecError::NotUtf8)
}

/// Why a channel-subscription envelope was refused. A reason, never the
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelSubscriptionEnvelopeError {
    /// The bytes do not open with [`CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1`].
    #[error("not a channel-subscription envelope")]
    WrongDomain,
    /// The version byte is not [`CHANNEL_SUBSCRIPTION_ENVELOPE_VERSION_V1`].
    #[error("unknown channel-subscription envelope version")]
    UnknownVersion,
    /// The count is zero or above [`CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_ENTRIES`].
    #[error("channel-subscription envelope count is out of bounds")]
    Count,
    /// The bytes ended early, or an entry is malformed (an unknown state or
    /// role byte, a length out of bounds, not UTF-8, a field that breaks its
    /// rule).
    #[error("channel-subscription envelope is truncated or malformed")]
    Truncated,
    /// Entries were not strictly increasing by channel id.
    #[error("channel-subscription envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in channel-subscription envelope")]
    Trailing,
}

impl ChannelSubscriptionEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Count => "count",
            Self::Truncated => "truncated",
            Self::NotCanonical => "notCanonical",
            Self::Trailing => "trailing",
        }
    }
}

/// `true` when `bytes` open with the channel-subscription envelope domain:
/// the cheap routing question. Says nothing about validity.
#[must_use]
pub fn is_channel_subscription_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=16 stamped registers, sorted by channel id.
///
/// # Errors
///
/// [`ChannelSubscriptionEnvelopeError::Count`] for none or too many entries,
/// [`ChannelSubscriptionEnvelopeError::NotCanonical`] for two entries of one
/// channel, [`ChannelSubscriptionEnvelopeError::Truncated`] for an entry
/// that breaks a bound.
pub fn compose_channel_subscription_envelope(
    entries: &[Stamped<ChannelSubscriptionEntry>],
) -> Result<Vec<u8>, ChannelSubscriptionEnvelopeError> {
    if entries.is_empty() || entries.len() > CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelSubscriptionEnvelopeError::Count);
    }
    if entries.iter().any(|e| e.value.validate().is_err()) {
        return Err(ChannelSubscriptionEnvelopeError::Truncated);
    }
    let count = u8::try_from(entries.len()).map_err(|_| ChannelSubscriptionEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<ChannelSubscriptionEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.channel_id);
    if sorted
        .windows(2)
        .any(|w| w[0].value.channel_id == w[1].value.channel_id)
    {
        return Err(ChannelSubscriptionEnvelopeError::NotCanonical);
    }
    let mut out = Vec::new();
    out.extend_from_slice(CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_SUBSCRIPTION_ENVELOPE_VERSION_V1);
    out.push(count);
    for e in sorted {
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`ChannelSubscriptionEnvelopeError`]; the domain is checked first,
/// then the version, then the count, then each entry, then trailing bytes.
pub fn decode_channel_subscription_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<ChannelSubscriptionEntry>>, ChannelSubscriptionEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1)
        .ok_or(ChannelSubscriptionEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| ChannelSubscriptionEnvelopeError::Truncated)?;
    if version != CHANNEL_SUBSCRIPTION_ENVELOPE_VERSION_V1 {
        return Err(ChannelSubscriptionEnvelopeError::UnknownVersion);
    }
    let count =
        usize::from(take_u8(&mut cur).map_err(|_| ChannelSubscriptionEnvelopeError::Truncated)?);
    if count == 0 || count > CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelSubscriptionEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<ChannelSubscriptionEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| ChannelSubscriptionEnvelopeError::Truncated)?;
        let value = ChannelSubscriptionEntry::take_body(&mut cur)
            .map_err(|_| ChannelSubscriptionEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.channel_id >= value.channel_id {
                return Err(ChannelSubscriptionEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(ChannelSubscriptionEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn live(
        channel: u8,
        role: ChannelSubscriptionRole,
        hlc: u64,
    ) -> Stamped<ChannelSubscriptionEntry> {
        Stamped {
            hlc,
            value: ChannelSubscriptionEntry {
                channel_id: [channel; 16],
                subscribed: Some(ChannelSubscribed {
                    role,
                    subscriber_secret: [0x5E; 32],
                    read_key: Some([0x4B; 32]),
                    publisher_group_id: (role == ChannelSubscriptionRole::CoPublisher)
                        .then_some([0x9A; 32]),
                    owner_account_key: (role == ChannelSubscriptionRole::CoPublisher)
                        .then_some([0x0E; 32]),
                    name: "News".to_string(),
                    handle: "news".to_string(),
                }),
            },
        }
    }

    fn left(channel: u8, hlc: u64) -> Stamped<ChannelSubscriptionEntry> {
        Stamped {
            hlc,
            value: ChannelSubscriptionEntry::left([channel; 16]),
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1.len(), 35);
        assert_eq!(CHANNEL_SUBSCRIPTION_ENVELOPE_MIN_LEN, 35 + 2 + 8 + 16 + 1);
        let wire = compose_channel_subscription_envelope(&[left(1, 9)]).unwrap_or_default();
        assert_eq!(wire.len(), CHANNEL_SUBSCRIPTION_ENVELOPE_MIN_LEN);
        assert_eq!(wire[35], 0x02);
        assert_eq!(wire[36], 1);
        assert_eq!(&wire[37..45], &9u64.to_be_bytes());
        assert_eq!(&wire[45..61], &[1u8; 16]);
        assert_eq!(wire[61], STATE_LEFT);

        let on = compose_channel_subscription_envelope(&[live(
            2,
            ChannelSubscriptionRole::Subscriber,
            7,
        )])
        .unwrap_or_default();
        assert_eq!(on[61], STATE_SUBSCRIBED);
        assert_eq!(on[62], 1);
        assert_eq!(&on[63..95], &[0x5E; 32]);
        assert_eq!(on[95], 32);
        assert_eq!(&on[96..128], &[0x4B; 32]);
        assert_eq!(on[128], 0);
        assert_eq!(on[129], 0);
        assert_eq!(on[130], 4);
        assert_eq!(&on[131..135], b"News");
        assert_eq!(on[135], 4);
        assert_eq!(&on[136..140], b"news");
        assert_eq!(on.len(), 140);
        let co = compose_channel_subscription_envelope(&[live(
            2,
            ChannelSubscriptionRole::CoPublisher,
            7,
        )])
        .unwrap_or_default();
        assert_eq!(co[128], 32);
        assert_eq!(&co[129..161], &[0x9A; 32]);
        assert_eq!(co[161], 32);
        assert_eq!(&co[162..194], &[0x0E; 32]);
        assert!(is_channel_subscription_envelope(&on));
        assert!(!is_channel_subscription_envelope(
            b"lakko/sync/link-record/v1\0"
        ));
        assert_eq!(CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN, 35 + 2 + 16 * 446);
    }

    #[test]
    fn the_secrets_stay_out_of_debug() {
        let e = live(3, ChannelSubscriptionRole::Owner, 1);
        let text = format!("{e:?}");
        assert!(!text.contains("94") && !text.contains("75"), "{text}");
        assert!(text.contains("has_read_key: true"));
    }

    #[test]
    fn a_publisher_group_id_belongs_to_a_co_publisher_alone() {
        let mut co = live(1, ChannelSubscriptionRole::CoPublisher, 1).value;
        assert!(co.validate().is_ok());
        if let Some(s) = co.subscribed.as_mut() {
            s.publisher_group_id = None;
        }
        assert_eq!(
            co.validate(),
            Err(BoundsError::ChannelSubscriptionPublisherGroup)
        );
        let mut owner = live(1, ChannelSubscriptionRole::Owner, 1).value;
        if let Some(s) = owner.subscribed.as_mut() {
            s.publisher_group_id = Some([1; 32]);
        }
        assert_eq!(
            owner.validate(),
            Err(BoundsError::ChannelSubscriptionPublisherGroup)
        );
    }

    /// ADR-0090 section 2.3: a co-publisher records the owner's account key
    /// (required); an owner and a subscriber record none.
    #[test]
    fn an_owner_key_belongs_to_a_co_publisher_alone() {
        let mut co = live(1, ChannelSubscriptionRole::CoPublisher, 1).value;
        if let Some(s) = co.subscribed.as_mut() {
            s.owner_account_key = None;
        }
        assert_eq!(co.validate(), Err(BoundsError::ChannelSubscriptionOwnerKey));
        for role in [
            ChannelSubscriptionRole::Owner,
            ChannelSubscriptionRole::Subscriber,
        ] {
            let mut e = live(1, role, 1).value;
            assert!(e.validate().is_ok());
            if let Some(s) = e.subscribed.as_mut() {
                s.owner_account_key = Some([1; 32]);
            }
            assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionOwnerKey));
        }
        // A key-less co-publisher entry on the wire is refused.
        let wire = compose_channel_subscription_envelope(&[live(
            4,
            ChannelSubscriptionRole::CoPublisher,
            1,
        )])
        .unwrap_or_default();
        let mut damaged = wire[..161].to_vec();
        damaged.push(0);
        damaged.extend_from_slice(&wire[194..]);
        assert!(decode_channel_subscription_envelope(&damaged).is_err());
        let text = format!("{:?}", live(4, ChannelSubscriptionRole::CoPublisher, 1));
        assert!(text.contains("has_owner_account_key: true"), "{text}");
    }

    #[test]
    fn names_and_handles_follow_their_rules() {
        let mut e = live(1, ChannelSubscriptionRole::Subscriber, 1).value;
        let set = |e: &mut ChannelSubscriptionEntry, name: &str, handle: &str| {
            if let Some(s) = e.subscribed.as_mut() {
                s.name = name.to_string();
                s.handle = handle.to_string();
            }
        };
        set(&mut e, "", "");
        assert!(e.validate().is_ok());
        set(&mut e, " padded", "");
        assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionName));
        set(&mut e, "a\nb", "");
        assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionName));
        set(&mut e, "Ok", "No");
        assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionHandle));
        set(&mut e, "Ok", "ab");
        assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionHandle));
        set(&mut e, "Ok", &"a".repeat(33));
        assert_eq!(e.validate(), Err(BoundsError::ChannelSubscriptionHandle));
        set(&mut e, "Ok", "the_news_42");
        assert!(e.validate().is_ok());
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_and_bad_counts() {
        let wire = compose_channel_subscription_envelope(&[
            left(3, 1),
            live(1, ChannelSubscriptionRole::Owner, 2),
            live(2, ChannelSubscriptionRole::CoPublisher, 3),
        ])
        .unwrap_or_default();
        assert_eq!(
            decode_channel_subscription_envelope(&wire),
            Ok(vec![
                live(1, ChannelSubscriptionRole::Owner, 2),
                live(2, ChannelSubscriptionRole::CoPublisher, 3),
                left(3, 1),
            ])
        );
        assert_eq!(
            compose_channel_subscription_envelope(&[left(1, 1), left(1, 2)]),
            Err(ChannelSubscriptionEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_channel_subscription_envelope(&[]),
            Err(ChannelSubscriptionEnvelopeError::Count)
        );
        let many: Vec<_> = (0..17).map(|c| left(c, 1)).collect();
        assert_eq!(
            compose_channel_subscription_envelope(&many),
            Err(ChannelSubscriptionEnvelopeError::Count)
        );
        let mut bad = live(1, ChannelSubscriptionRole::Subscriber, 1);
        if let Some(s) = bad.value.subscribed.as_mut() {
            s.handle = "X".to_string();
        }
        assert_eq!(
            compose_channel_subscription_envelope(&[bad]),
            Err(ChannelSubscriptionEnvelopeError::Truncated)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire = compose_channel_subscription_envelope(&[
            live(1, ChannelSubscriptionRole::Subscriber, 1),
            left(2, 2),
        ])
        .unwrap_or_default();
        for cut in 0..wire.len() {
            assert!(
                decode_channel_subscription_envelope(&wire[..cut]).is_err(),
                "{cut}"
            );
        }
        let mut version = wire.clone();
        version[35] = 1;
        assert_eq!(
            decode_channel_subscription_envelope(&version),
            Err(ChannelSubscriptionEnvelopeError::UnknownVersion)
        );
        let mut count = wire.clone();
        count[36] = 0;
        assert_eq!(
            decode_channel_subscription_envelope(&count),
            Err(ChannelSubscriptionEnvelopeError::Count)
        );
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_channel_subscription_envelope(&trailing),
            Err(ChannelSubscriptionEnvelopeError::Trailing)
        );
        for (at, value) in [
            (61, 3u8),  // state
            (62, 4),    // role
            (95, 31),   // read key length
            (128, 32),  // a group id for a subscriber
            (129, 32),  // an owner key for a subscriber
            (130, 255), // name length runs past the bytes
            (135, 33),  // handle length past its bound
        ] {
            let mut damaged = wire.clone();
            damaged[at] = value;
            assert!(
                decode_channel_subscription_envelope(&damaged).is_err(),
                "{at}"
            );
        }
        let mut name = wire.clone();
        name[131] = 0xFF;
        assert_eq!(
            decode_channel_subscription_envelope(&name),
            Err(ChannelSubscriptionEnvelopeError::Truncated)
        );
        // Swap the two entries: not canonical.
        let first_len = 8 + 16 + 1 + 1 + 32 + 33 + 1 + 1 + 5 + 5;
        let mut swapped = wire[..37].to_vec();
        swapped.extend_from_slice(&wire[37 + first_len..]);
        swapped.extend_from_slice(&wire[37..37 + first_len]);
        assert_eq!(
            decode_channel_subscription_envelope(&swapped),
            Err(ChannelSubscriptionEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_channel_subscription_envelope(b"lakko/sync/link-record/v1\0\x01"),
            Err(ChannelSubscriptionEnvelopeError::WrongDomain)
        );
    }

    fn arb_entry() -> impl Strategy<Value = ChannelSubscriptionEntry> {
        (
            any::<[u8; 16]>(),
            proptest::option::of((
                0u8..3,
                any::<[u8; 32]>(),
                proptest::option::of(any::<[u8; 32]>()),
                any::<[u8; 32]>(),
                any::<[u8; 32]>(),
                "[A-Za-z]{0,20}",
                "[a-z0-9_]{3,32}|",
            )),
        )
            .prop_map(|(channel_id, s)| ChannelSubscriptionEntry {
                channel_id,
                subscribed: s.map(|(role, secret, read_key, group, owner, name, handle)| {
                    let role = match role {
                        0 => ChannelSubscriptionRole::Subscriber,
                        1 => ChannelSubscriptionRole::Owner,
                        _ => ChannelSubscriptionRole::CoPublisher,
                    };
                    ChannelSubscribed {
                        role,
                        subscriber_secret: secret,
                        read_key,
                        publisher_group_id: (role == ChannelSubscriptionRole::CoPublisher)
                            .then_some(group),
                        owner_account_key: (role == ChannelSubscriptionRole::CoPublisher)
                            .then_some(owner),
                        name,
                        handle,
                    }
                }),
            })
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..700)) {
            let mut input = CHANNEL_SUBSCRIPTION_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_channel_subscription_envelope(&input) {
                prop_assert!(input.len() <= CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN);
                prop_assert_eq!(compose_channel_subscription_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(entries in proptest::collection::btree_map(any::<[u8; 16]>(), (arb_entry(), any::<u64>()), 1..=16)) {
            let mut stamped: Vec<Stamped<ChannelSubscriptionEntry>> = entries
                .into_iter()
                .map(|(id, (mut e, hlc))| {
                    e.channel_id = id;
                    Stamped { hlc, value: e }
                })
                .collect();
            let wire = compose_channel_subscription_envelope(&stamped).unwrap_or_default();
            prop_assert!(wire.len() <= CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN);
            stamped.sort_by_key(|e| e.value.channel_id);
            prop_assert_eq!(decode_channel_subscription_envelope(&wire), Ok(stamped));
        }
    }
}
