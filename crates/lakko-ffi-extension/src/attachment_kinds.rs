// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim RECOGNISE surface for the ADR-0092 rows a push wake can land on that
//! are not chat: a sealed anonymous ballot row on a group journal, the
//! open-items and poll-ballots hand-overs (ordinary MLS application
//! messages), and the account's own poll-choice register on the DeviceGroup.
//!
//! ## Scope: recognise, nothing else (C-59)
//!
//! No compose, no open, no tally: the extension takes no decision and stores
//! nothing. A ballot row is not an MLS message at all, so the extension must
//! skip it before MLS processing, by its prefix.
//!
//! ## Why prefixes and not the owning crates
//!
//! `lakko-ballot` (the ballot row) and `lakko-sync` (the poll-choice
//! envelope) are not linked into the extension, to keep its dependency set
//! the one ADR-0041 measured. The two domains are copied here and pinned to
//! their owners by the tests below (dev-dependencies only), so a change in
//! either crate fails CI instead of silently un-suppressing a row. The
//! open-items and poll-ballots recognisers come from `lakko-contacts`, which
//! the extension already links.

use lakko_contacts::open_items::is_open_items_envelope as core_is_open_items;
use lakko_contacts::poll_ballots::{
    is_poll_ballots_envelope as core_is_poll_ballots, POLL_BALLOTS_ROW_PREFIX,
};

/// The poll-choice DeviceGroup envelope domain (`lakko-sync`'s
/// `POLL_CHOICE_ENVELOPE_DOMAIN_V1`), copied; pinned by a test.
const POLL_CHOICE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/poll-choice/v1\0";

/// Whether a journal row is a sealed anonymous ballot (`lakko/poll-ballot/v1\0`
/// prefix). Such a row is never fed to MLS processing and never notified.
#[uniffi::export]
#[must_use]
pub fn is_poll_ballot_row(row: Vec<u8>) -> bool {
    row.starts_with(POLL_BALLOTS_ROW_PREFIX)
}

/// Whether `plaintext` is an open-items hand-over (never notified).
#[uniffi::export]
#[must_use]
pub fn is_open_items_envelope(plaintext: Vec<u8>) -> bool {
    core_is_open_items(&plaintext)
}

/// Whether `plaintext` is a poll-ballots hand-over (never notified).
#[uniffi::export]
#[must_use]
pub fn is_poll_ballots_envelope(plaintext: Vec<u8>) -> bool {
    core_is_poll_ballots(&plaintext)
}

/// Whether `plaintext` is the account's own poll-choice envelope (a
/// DeviceGroup service envelope; never notified).
#[uniffi::export]
#[must_use]
pub fn is_poll_choice_envelope(plaintext: Vec<u8>) -> bool {
    plaintext.starts_with(POLL_CHOICE_ENVELOPE_DOMAIN_V1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_pins_the_prefixes_a_swift_prefilter_copies() {
        assert_eq!(POLL_BALLOTS_ROW_PREFIX, b"lakko/poll-ballot/v1\0");
        assert_eq!(
            POLL_CHOICE_ENVELOPE_DOMAIN_V1,
            b"lakko/sync/poll-choice/v1\0"
        );
        assert_eq!(
            lakko_contacts::open_items::OPEN_ITEMS_ENVELOPE_DOMAIN_V1,
            b"lakko/open-items/v1\0"
        );
        assert_eq!(
            lakko_contacts::poll_ballots::POLL_BALLOTS_ENVELOPE_DOMAIN_V1,
            b"lakko/poll-ballots/v1\0"
        );
    }

    #[test]
    fn the_copied_domains_equal_their_owners() {
        assert_eq!(
            POLL_BALLOTS_ROW_PREFIX,
            lakko_ballot::ballot::BALLOT_ROW_DOMAIN
        );
        assert_eq!(
            POLL_CHOICE_ENVELOPE_DOMAIN_V1,
            lakko_sync::synced::POLL_CHOICE_ENVELOPE_DOMAIN_V1
        );
    }

    #[test]
    fn recognisers_accept_their_own_rows_only() {
        assert!(is_poll_ballot_row(
            b"lakko/poll-ballot/v1\0\x01rest".to_vec()
        ));
        assert!(!is_poll_ballot_row(b"lakko/poll-ballots/v1\0".to_vec()));
        assert!(is_poll_choice_envelope(
            b"lakko/sync/poll-choice/v1\0\x02".to_vec()
        ));
        assert!(!is_open_items_envelope(b"{\"v\":1}".to_vec()));
        assert!(!is_poll_ballots_envelope(
            b"lakko/poll-ballot/v1\0".to_vec()
        ));
    }

    #[test]
    fn content_kinds_preview_and_ops_classify() {
        use crate::attachments::{parse_body_envelope, MessageBodyKind};
        let loc = lakko_attachments::Location {
            lat_e7: 1,
            lon_e7: 2,
            accuracy_m: 5,
            fix_at_ms: 0,
            name: Some("Cafe".into()),
        };
        let bytes = lakko_attachments::compose_content(
            &lakko_attachments::Content::Location(loc),
            Vec::new(),
            None,
            None,
        )
        .unwrap();
        let p = parse_body_envelope(bytes);
        assert_eq!(p.kind, MessageBodyKind::Location);
        assert_eq!(p.text, "Cafe");
        let vote = parse_body_envelope(lakko_attachments::compose_poll_vote(5, &[1]).unwrap());
        assert_eq!(vote.kind, MessageBodyKind::PollVote);
        assert_eq!(vote.target, Some(5));
    }
}
