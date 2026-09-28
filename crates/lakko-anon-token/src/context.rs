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
/// wire value is the `u8` discriminant and is frozen: a value is never reused.
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
}

impl TokenContext {
    /// Every context, in wire order.
    pub const ALL: [Self; 5] = [
        Self::DiscoveryLookup,
        Self::DiscoveryRegister,
        Self::InviteConsume,
        Self::DsKnock,
        Self::PushRegister,
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
            TokenContext::from_u8(6),
            Err(AnonTokenError::UnknownContext)
        );
        assert_eq!(
            TokenContext::from_name("DS-KNOCK"),
            Err(AnonTokenError::UnknownContext)
        );
    }

    #[test]
    fn wire_bytes_are_frozen() {
        assert_eq!(TokenContext::DiscoveryLookup.as_u8(), 1);
        assert_eq!(TokenContext::DiscoveryRegister.as_u8(), 2);
        assert_eq!(TokenContext::InviteConsume.as_u8(), 3);
        assert_eq!(TokenContext::DsKnock.as_u8(), 4);
        assert_eq!(TokenContext::PushRegister.as_u8(), 5);
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
