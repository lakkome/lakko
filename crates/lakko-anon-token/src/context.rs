// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Token contexts and epochs.
//!
//! A token is bound to one **context** (the action it pays for) and one
//! **epoch** (the issuance period). Both enter the issuance-key derivation, so
//! a token minted for one (context, epoch) does not verify under any other,
//! and both enter the token input, so they are also covered by the VOPRF
//! output itself.

use crate::error::{AnonTokenError, Result};

/// Default epoch window: one day.
///
/// Chosen, not measured. A day is long enough that a person's handful of
/// lookups / knocks / invite opens (ADR-0054 decision 2) fall in few epochs,
/// and short enough that a stolen pool of tokens dies within two days (a token
/// minted in epoch `E` verifies in `E` and `E + 1`). Every service that issues
/// or verifies tokens must be configured with the SAME window: the epoch number
/// is `floor(unix_secs / window_secs)`, so a mismatch makes one side's current
/// epoch the other side's past or future and every token fails. Changing the
/// window renumbers the epochs and invalidates every outstanding token.
pub const DEFAULT_WINDOW_SECS: u64 = 86_400;

/// The action a token pays for.
///
/// One context per rationed action group of ADR-0054 decision 2, rather than
/// one per consuming service, so the issuer can ration each action on its own
/// quota (a knock is rarer than a lookup) while a service that consumes two
/// contexts still keeps one spent-set keyed on `(context, spend_id)`. The
/// wire value is the `u8` discriminant and is frozen: a value is never reused
/// and never renumbered, so a new context is appended with the next byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TokenContext {
    /// `services/discovery`: OPRF evaluation, username lookup, channel lookup.
    DiscoveryLookup = 1,
    /// `services/discovery`: channel-name registration.
    DiscoveryRegister = 2,
    /// `services/discovery`: invite / call-invite / group-invite consume.
    InviteConsume = 3,
    /// `services/ds`: the knock on a claimed group journal.
    DsKnock = 4,
    /// `services/push-gw`: device register / unregister, group subscribe /
    /// unsubscribe.
    PushRegister = 5,
    /// `services/channels`: registering as a channel subscriber (ADR-0059
    /// section 4). Makes a subscriber count cost accounts rather than
    /// nothing.
    ChannelSubscribe = 6,
    /// `services/channels`: one directory search (ADR-0059 section 5).
    DirectorySearch = 7,
    /// `services/storage`: claiming a conversation vault (ADR-0064
    /// section 1).
    VaultClaim = 8,
    /// `services/storage`: one token buys an hour's attachment upload pass
    /// (ADR-0065 section 5).
    BlobUpload = 9,
    /// `services/storage`: one token buys an hour's attachment read pass
    /// (ADR-0065 section 5).
    BlobRead = 10,
    /// `services/ds`: a write with no reach proof to a personal mailbox that
    /// is open (Everyone) or not yet claimed (ADR-0078 section 1). Under
    /// sealed sender nothing about such a writer is visible, so the token is
    /// what rations it.
    MailboxWrite = 11,
}

impl TokenContext {
    /// Every context, in wire order.
    pub const ALL: [Self; 11] = [
        Self::DiscoveryLookup,
        Self::DiscoveryRegister,
        Self::InviteConsume,
        Self::DsKnock,
        Self::PushRegister,
        Self::ChannelSubscribe,
        Self::DirectorySearch,
        Self::VaultClaim,
        Self::BlobUpload,
        Self::BlobRead,
        Self::MailboxWrite,
    ];

    /// The frozen wire byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse the wire byte.
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::UnknownContext`] for any byte that is not a context.
    pub const fn from_u8(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::DiscoveryLookup),
            2 => Ok(Self::DiscoveryRegister),
            3 => Ok(Self::InviteConsume),
            4 => Ok(Self::DsKnock),
            5 => Ok(Self::PushRegister),
            6 => Ok(Self::ChannelSubscribe),
            7 => Ok(Self::DirectorySearch),
            8 => Ok(Self::VaultClaim),
            9 => Ok(Self::BlobUpload),
            10 => Ok(Self::BlobRead),
            11 => Ok(Self::MailboxWrite),
            _ => Err(AnonTokenError::UnknownContext),
        }
    }

    /// The stable name used on the HTTP surface and in configuration.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DiscoveryLookup => "discovery-lookup",
            Self::DiscoveryRegister => "discovery-register",
            Self::InviteConsume => "invite-consume",
            Self::DsKnock => "ds-knock",
            Self::PushRegister => "push-register",
            Self::ChannelSubscribe => "channel-subscribe",
            Self::DirectorySearch => "directory-search",
            Self::VaultClaim => "vault-claim",
            Self::BlobUpload => "blob-upload",
            Self::BlobRead => "blob-read",
            Self::MailboxWrite => "mailbox-write",
        }
    }

    /// Parse the stable name (exact, case-sensitive).
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::UnknownContext`] for any other string.
    pub fn from_name(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.name() == name)
            .ok_or(AnonTokenError::UnknownContext)
    }
}

/// The epoch containing `unix_secs`: `floor(unix_secs / window_secs)`.
///
/// # Errors
///
/// [`AnonTokenError::InvalidWindow`] when `window_secs` is zero.
pub const fn epoch_at(unix_secs: u64, window_secs: u64) -> Result<u64> {
    if window_secs == 0 {
        return Err(AnonTokenError::InvalidWindow);
    }
    Ok(unix_secs / window_secs)
}

/// Whether a token epoch is accepted when the current epoch is `current`:
/// the current epoch and the one before it, nothing older, nothing newer.
#[must_use]
pub const fn epoch_accepted(token_epoch: u64, current: u64) -> bool {
    token_epoch == current || (current > 0 && token_epoch == current - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_bytes_and_names_round_trip() {
        for c in TokenContext::ALL {
            assert_eq!(TokenContext::from_u8(c.as_u8()), Ok(c));
            assert_eq!(TokenContext::from_name(c.name()), Ok(c));
        }
        assert_eq!(
            TokenContext::from_u8(0),
            Err(AnonTokenError::UnknownContext)
        );
        assert_eq!(
            TokenContext::from_u8(12),
            Err(AnonTokenError::UnknownContext)
        );
        assert_eq!(
            TokenContext::from_u8(u8::MAX),
            Err(AnonTokenError::UnknownContext)
        );
        assert_eq!(
            TokenContext::from_name("DS-KNOCK"),
            Err(AnonTokenError::UnknownContext)
        );
    }

    #[test]
    fn all_lists_every_context_once_in_wire_order() {
        // `ALL` is what `from_name`, the issuer's quota table and both FFI
        // surfaces enumerate: a context missing from it has no name lookup
        // and no quota. Exactly the bytes 1..=11 parse, and `ALL` is those
        // contexts in that order.
        let parsed: Vec<TokenContext> = (0..=u8::MAX)
            .filter_map(|b| TokenContext::from_u8(b).ok())
            .collect();
        assert_eq!(parsed, TokenContext::ALL);
        let bytes: Vec<u8> = TokenContext::ALL.iter().map(|c| c.as_u8()).collect();
        assert_eq!(bytes, (1..=11).collect::<Vec<u8>>());
        let mut names: Vec<&str> = TokenContext::ALL.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TokenContext::ALL.len());
    }

    #[test]
    fn wire_bytes_are_frozen() {
        assert_eq!(TokenContext::DiscoveryLookup.as_u8(), 1);
        assert_eq!(TokenContext::DiscoveryRegister.as_u8(), 2);
        assert_eq!(TokenContext::InviteConsume.as_u8(), 3);
        assert_eq!(TokenContext::DsKnock.as_u8(), 4);
        assert_eq!(TokenContext::PushRegister.as_u8(), 5);
        assert_eq!(TokenContext::ChannelSubscribe.as_u8(), 6);
        assert_eq!(TokenContext::DirectorySearch.as_u8(), 7);
        assert_eq!(TokenContext::VaultClaim.as_u8(), 8);
        assert_eq!(TokenContext::BlobUpload.as_u8(), 9);
        assert_eq!(TokenContext::BlobRead.as_u8(), 10);
        assert_eq!(TokenContext::MailboxWrite.as_u8(), 11);
        assert_eq!(TokenContext::DiscoveryLookup.name(), "discovery-lookup");
        assert_eq!(TokenContext::DiscoveryRegister.name(), "discovery-register");
        assert_eq!(TokenContext::InviteConsume.name(), "invite-consume");
        assert_eq!(TokenContext::DsKnock.name(), "ds-knock");
        assert_eq!(TokenContext::PushRegister.name(), "push-register");
        assert_eq!(TokenContext::ChannelSubscribe.name(), "channel-subscribe");
        assert_eq!(TokenContext::DirectorySearch.name(), "directory-search");
        assert_eq!(TokenContext::VaultClaim.name(), "vault-claim");
        assert_eq!(TokenContext::BlobUpload.name(), "blob-upload");
        assert_eq!(TokenContext::BlobRead.name(), "blob-read");
        assert_eq!(TokenContext::MailboxWrite.name(), "mailbox-write");
    }

    #[test]
    fn epoch_is_floor_division_and_refuses_a_zero_window() {
        assert_eq!(epoch_at(0, 86_400), Ok(0));
        assert_eq!(epoch_at(86_399, 86_400), Ok(0));
        assert_eq!(epoch_at(86_400, 86_400), Ok(1));
        assert_eq!(epoch_at(u64::MAX, 1), Ok(u64::MAX));
        assert_eq!(epoch_at(5, 0), Err(AnonTokenError::InvalidWindow));
    }

    #[test]
    fn only_current_and_previous_epoch_are_accepted() {
        assert!(epoch_accepted(10, 10));
        assert!(epoch_accepted(9, 10));
        assert!(!epoch_accepted(8, 10));
        assert!(!epoch_accepted(11, 10));
        assert!(epoch_accepted(0, 0));
        assert!(!epoch_accepted(u64::MAX, 0));
        assert!(epoch_accepted(u64::MAX, u64::MAX));
        assert!(epoch_accepted(u64::MAX - 1, u64::MAX));
    }
}
