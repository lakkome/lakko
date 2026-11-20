// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The report reasons ([ADR-0099] section 2.1, UX section 2.2): one closed
//! list, frozen bytes, shared by the report sheet, the report, the panel and
//! the suspended person's screen. The order of [`Reason::ALL`] is the order
//! the sheet shows.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use crate::ModerationError;

/// Why something was reported, and why a moderator acted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Reason {
    /// Spam or scam.
    Spam = 1,
    /// Harassment or threats.
    Harassment = 2,
    /// Hate.
    Hate = 3,
    /// Violence or graphic content.
    Violence = 4,
    /// Sexual content.
    Sexual = 5,
    /// A child's safety.
    ChildSafety = 6,
    /// Pretending to be someone else.
    Impersonation = 7,
    /// Something else.
    Other = 8,
}

impl Reason {
    /// Every reason, in the order the report sheet lists them.
    pub const ALL: [Self; 8] = [
        Self::Spam,
        Self::Harassment,
        Self::Hate,
        Self::Violence,
        Self::Sexual,
        Self::ChildSafety,
        Self::Impersonation,
        Self::Other,
    ];

    /// The wire byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte; an unknown byte is refused.
    ///
    /// # Errors
    ///
    /// [`ModerationError::UnknownValue`].
    pub const fn from_u8(byte: u8) -> Result<Self, ModerationError> {
        Ok(match byte {
            1 => Self::Spam,
            2 => Self::Harassment,
            3 => Self::Hate,
            4 => Self::Violence,
            5 => Self::Sexual,
            6 => Self::ChildSafety,
            7 => Self::Impersonation,
            8 => Self::Other,
            _ => return Err(ModerationError::UnknownValue("reason")),
        })
    }

    /// The stable name (catalog keys are `report.reason.<name>` with `-` as `_`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Harassment => "harassment",
            Self::Hate => "hate",
            Self::Violence => "violence",
            Self::Sexual => "sexual",
            Self::ChildSafety => "child-safety",
            Self::Impersonation => "impersonation",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_round_trips_and_nothing_else_decodes() {
        let decoded: Vec<Reason> = (0..=u8::MAX)
            .filter_map(|b| Reason::from_u8(b).ok())
            .collect();
        assert_eq!(decoded, Reason::ALL);
        for r in Reason::ALL {
            assert_eq!(Reason::from_u8(r.as_u8()), Ok(r));
        }
    }

    #[test]
    fn frozen_bytes() {
        let bytes: Vec<u8> = Reason::ALL.iter().map(|r| r.as_u8()).collect();
        assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
