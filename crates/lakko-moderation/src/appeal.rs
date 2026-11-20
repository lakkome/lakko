// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A suspended person's appeal ([ADR-0098] section 4, UX section 4).
//!
//! An appeal is a short text sealed to the moderator list with
//! [`Purpose::Appeal`], so only the listed moderators open it and a report's
//! seal never opens as an appeal (the purpose enters every key derivation).
//! Auth stores the sealed bytes against the suspension and never opens them.
//!
//! ```text
//! appeal = seal::seal_for_moderators(Appeal, APPEAL_AAD, text, list_version, recipients)
//! text   = UTF-8, trimmed, 1..=APPEAL_MAX_CHARS scalars, no control
//!          character but line feed and tab
//! ```
//!
//! The sealed bytes are exactly a [`crate::seal::split`]-able part: there is
//! no outer framing, so auth checks the shape with `split` alone.
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use lakko_crypto::agreement::SecretKey;

use crate::moderators::ModeratorList;
use crate::seal::{self, Purpose, MODERATOR_ID_LEN};
use crate::ModerationError;

/// The outer AAD of every appeal. Frozen.
pub const APPEAL_AAD: &[u8] = b"lakko/moderation/appeal/v1\0";
/// The longest appeal in Unicode scalar values (UX section 4).
pub const APPEAL_MAX_CHARS: usize = 1000;
/// The longest appeal in bytes (four bytes a scalar at most).
pub const APPEAL_MAX_BYTES: usize = APPEAL_MAX_CHARS * 4;

fn check_text(text: &str) -> Result<(), ModerationError> {
    let count = text.chars().count();
    if count == 0 {
        return Err(ModerationError::Invalid("appeal empty"));
    }
    if count > APPEAL_MAX_CHARS || text.len() > APPEAL_MAX_BYTES {
        return Err(ModerationError::Invalid("appeal too long"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ModerationError::Invalid("appeal control character"));
    }
    Ok(())
}

/// Seal an appeal to every moderator `list` names and has not revoked, at
/// the list's version. The text is trimmed first.
///
/// The caller verifies `list` under the moderation root before calling
/// (the FFI surfaces do).
///
/// # Errors
///
/// [`ModerationError::Invalid`] for an empty or over-long text, a control
/// character, or a list with no current moderator; the seal's own refusals.
pub fn seal_appeal(text: &str, list: &ModeratorList) -> Result<Vec<u8>, ModerationError> {
    let text = text.trim();
    check_text(text)?;
    seal::seal_for_moderators(
        Purpose::Appeal,
        APPEAL_AAD,
        text.as_bytes(),
        list.list_version,
        &list.recipients(),
    )
}

/// Open an appeal as the moderator `moderator_id` holding `secret` (the
/// panel, and tests).
///
/// # Errors
///
/// [`ModerationError::Malformed`] for bad framing,
/// [`ModerationError::CannotOpen`] when the appeal is not wrapped to this
/// moderator or does not authenticate, [`ModerationError::Malformed`] for a
/// text that breaks the appeal rules.
pub fn open_appeal(
    sealed: &[u8],
    moderator_id: &[u8; MODERATOR_ID_LEN],
    secret: &SecretKey,
) -> Result<String, ModerationError> {
    let bytes =
        seal::open_for_moderator(Purpose::Appeal, APPEAL_AAD, sealed, moderator_id, secret)?;
    let text = String::from_utf8(bytes).map_err(|_| ModerationError::Malformed("appeal utf-8"))?;
    if text.trim() != text {
        return Err(ModerationError::Malformed("appeal not trimmed"));
    }
    check_text(&text).map_err(|_| ModerationError::Malformed("appeal text"))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moderators::{ModeratorEntry, ModeratorKey};
    use crate::seal::moderator_report_secret;
    use proptest::prelude::*;

    fn entry(id: u8, revoked_at_ms: u64) -> (ModeratorEntry, SecretKey) {
        let secret = moderator_report_secret(&[id; 32]).unwrap();
        (
            ModeratorEntry {
                moderator_id: [id; 16],
                name: format!("m{id}"),
                key: ModeratorKey::Ed25519([id; 32]),
                credential_id: vec![id],
                x25519_public: secret.public_key().to_bytes(),
                added_at_ms: 0,
                revoked_at_ms,
            },
            secret,
        )
    }

    fn list(entries: Vec<ModeratorEntry>) -> ModeratorList {
        ModeratorList {
            list_version: 7,
            issued_at_ms: 0,
            rp_id: "moderation.example".into(),
            origin: "https://moderation.example".into(),
            entries,
        }
    }

    #[test]
    fn an_appeal_opens_for_every_current_moderator_and_is_trimmed() {
        let (a, sa) = entry(1, 0);
        let (b, sb) = entry(2, 0);
        let l = list(vec![a, b]);
        let sealed = seal_appeal("  I did not do this.\nPlease look again.  ", &l).unwrap();
        let parts = seal::split(&sealed).unwrap();
        assert_eq!(parts.list_version, 7);
        assert_eq!(parts.moderator_ids, vec![[1; 16], [2; 16]]);
        for (id, s) in [([1; 16], &sa), ([2; 16], &sb)] {
            assert_eq!(
                open_appeal(&sealed, &id, s).unwrap(),
                "I did not do this.\nPlease look again."
            );
        }
    }

    #[test]
    fn a_revoked_moderator_is_not_a_recipient() {
        let (a, _) = entry(1, 0);
        let (b, sb) = entry(2, 5);
        let sealed = seal_appeal("please", &list(vec![a, b])).unwrap();
        assert_eq!(
            open_appeal(&sealed, &[2; 16], &sb).unwrap_err(),
            ModerationError::CannotOpen
        );
    }

    #[test]
    fn text_rules() {
        let (a, _) = entry(1, 0);
        let l = list(vec![a]);
        for bad in ["", "   \n ", "a\u{0007}b"] {
            assert!(seal_appeal(bad, &l).is_err(), "{bad:?}");
        }
        assert!(seal_appeal(&"é".repeat(APPEAL_MAX_CHARS), &l).is_ok());
        assert!(seal_appeal(&"x".repeat(APPEAL_MAX_CHARS + 1), &l).is_err());
        // Trimming happens before counting.
        assert!(seal_appeal(&format!("  {}  ", "x".repeat(APPEAL_MAX_CHARS)), &l).is_ok());
    }

    #[test]
    fn a_list_with_no_current_moderator_is_refused() {
        let (a, _) = entry(1, 3);
        assert!(seal_appeal("please", &list(vec![a])).is_err());
    }

    #[test]
    fn an_appeal_never_opens_as_a_report_and_a_report_never_as_an_appeal() {
        let (a, sa) = entry(1, 0);
        let l = list(vec![a.clone()]);
        let appeal = seal_appeal("please", &l).unwrap();
        assert_eq!(
            seal::open_for_moderator(Purpose::Report, APPEAL_AAD, &appeal, &[1; 16], &sa)
                .unwrap_err(),
            ModerationError::CannotOpen
        );
        let report_like =
            seal::seal_for_moderators(Purpose::Report, APPEAL_AAD, b"please", 7, &l.recipients())
                .unwrap();
        assert_eq!(
            open_appeal(&report_like, &[1; 16], &sa).unwrap_err(),
            ModerationError::CannotOpen
        );
    }

    #[test]
    fn the_largest_appeal_fits_the_server_bound() {
        // Auth accepts at most 16 KiB; the largest appeal to the largest list
        // must fit.
        let entries: Vec<_> = (1..=64u8).map(|i| entry(i, 0).0).collect();
        let l = list(entries);
        let sealed = seal_appeal(&"\u{10FFFF}".repeat(APPEAL_MAX_CHARS), &l).unwrap();
        assert!(sealed.len() <= 16 * 1024, "{}", sealed.len());
    }

    proptest! {
        #[test]
        fn any_valid_text_round_trips(text in "[a-zA-Z0-9 .,\n]{1,200}") {
            let (a, sa) = entry(1, 0);
            let l = list(vec![a]);
            match seal_appeal(&text, &l) {
                Ok(sealed) => {
                    prop_assert_eq!(open_appeal(&sealed, &[1; 16], &sa).unwrap(), text.trim());
                }
                Err(_) => prop_assert!(text.trim().is_empty()),
            }
        }
    }
}
