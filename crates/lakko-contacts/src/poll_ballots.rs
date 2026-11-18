// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Old ballot rows for a newcomer — [ADR-0090] section 3.9.
//!
//! A newcomer to a group fetches an anonymous poll's ballot rows from the
//! group journal itself, from the poll's row onward. A poll open longer than
//! the journal retention (30 days, ADR-0024) has ballots the journal no
//! longer holds; the committer of the admission appends its stored copies of
//! those rows (the sealed bytes, which the newcomer opens and verifies under
//! the poll secret like any ballot) in these envelopes.
//!
//! ## Wire format
//!
//! ```text
//! envelope = "lakko/poll-ballots/v1\0"(22) || 0x01 || poll_ref(i64 BE)
//!            || count(u16 BE: 1..) || (len(u16 BE: 1..=4096) || row)*
//! ```
//!
//! Every `row` starts with the ballot row domain `"lakko/poll-ballot/v1\0"`
//! (`lakko_ballot::ballot::BALLOT_ROW_DOMAIN`) and the rows are strictly
//! ascending bytewise (so no row twice). The whole envelope is at most
//! [`POLL_BALLOTS_ENVELOPE_MAX_LEN`] bytes. The rows stay opaque here; the
//! newcomer opens each with `lakko_ballot::open_and_verify`.
//!
//! [ADR-0090]: ../../docs/adr/0090-attachment-kinds.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::open_items::OPEN_ITEMS_ENVELOPE_MAX_LEN;

/// 22-byte domain separator. Frozen.
pub const POLL_BALLOTS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/poll-ballots/v1\x00";

/// Schema-version byte.
pub const POLL_BALLOTS_VERSION_V1: u8 = 0x01;

/// The domain every carried row starts with
/// (`lakko_ballot::ballot::BALLOT_ROW_DOMAIN`).
pub const POLL_BALLOTS_ROW_PREFIX: &[u8] = b"lakko/poll-ballot/v1\x00";

/// Longest carried row: 4096 bytes. Chosen, above `lakko-ballot`'s
/// `MAX_ROW_LEN` (pinned by a test in `lakko-open-items`).
pub const POLL_BALLOTS_MAX_ROW_LEN: usize = 4096;

/// Longest envelope (the open-items bound).
pub const POLL_BALLOTS_ENVELOPE_MAX_LEN: usize = OPEN_ITEMS_ENVELOPE_MAX_LEN;

/// Shortest envelope: one row of just the row domain.
pub const POLL_BALLOTS_ENVELOPE_MIN_LEN: usize =
    POLL_BALLOTS_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 2 + 2 + POLL_BALLOTS_ROW_PREFIX.len();

/// The DS journal retention (ADR-0024): rows older than this may be gone.
pub const JOURNAL_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// How much earlier than the retention a committer starts handing a ballot
/// over: one day. Chosen; a ballot handed over that the journal still holds
/// is a harmless duplicate, one that is neither is lost to the newcomer.
pub const BALLOT_HANDOVER_MARGIN_MS: i64 = 24 * 60 * 60 * 1000;

const FIXED_LEN: usize = POLL_BALLOTS_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 2;

/// A decoded envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct PollBallots {
    /// DS `messageId` of the poll row.
    pub poll_ref: i64,
    /// The sealed ballot rows, strictly ascending.
    pub rows: Vec<Vec<u8>>,
}

impl core::fmt::Debug for PollBallots {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PollBallots")
            .field("poll_ref", &self.poll_ref)
            .field("rows", &self.rows.len())
            .finish()
    }
}

/// Why an envelope was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PollBallotsError {
    /// Not the domain. Only from [`decode_poll_ballots`].
    #[error("plaintext does not carry the poll-ballots domain separator")]
    WrongDomain,
    /// The version byte is not [`POLL_BALLOTS_VERSION_V1`].
    #[error("poll-ballots envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The bytes ended before the layout did, or bytes follow it.
    #[error("poll-ballots envelope does not have its layout's length")]
    BadLength,
    /// Longer than [`POLL_BALLOTS_ENVELOPE_MAX_LEN`].
    #[error("poll-ballots envelope is too long")]
    TooLong,
    /// No row.
    #[error("poll-ballots envelope carries no row")]
    Empty,
    /// A row empty, too long, or not a ballot row.
    #[error("poll-ballots row is malformed")]
    Row,
    /// Rows not strictly ascending.
    #[error("poll-ballots rows are not strictly ascending")]
    RowOrder,
}

impl PollBallotsError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadLength => "badLength",
            Self::TooLong => "tooLong",
            Self::Empty => "empty",
            Self::Row => "row",
            Self::RowOrder => "rowOrder",
        }
    }
}

type Result<T> = CoreResult<T, PollBallotsError>;

fn row_ok(row: &[u8]) -> bool {
    row.len() <= POLL_BALLOTS_MAX_ROW_LEN && row.starts_with(POLL_BALLOTS_ROW_PREFIX)
}

/// Compose an envelope. [`decode_poll_ballots`] accepts exactly what this
/// produces.
///
/// # Errors
///
/// [`PollBallotsError::Empty`], [`PollBallotsError::Row`],
/// [`PollBallotsError::RowOrder`], [`PollBallotsError::TooLong`].
pub fn compose_poll_ballots(ballots: &PollBallots) -> Result<Vec<u8>> {
    if ballots.rows.is_empty() {
        return Err(PollBallotsError::Empty);
    }
    if !ballots.rows.iter().all(|r| row_ok(r)) {
        return Err(PollBallotsError::Row);
    }
    if !ballots.rows.windows(2).all(|w| w[0] < w[1]) {
        return Err(PollBallotsError::RowOrder);
    }
    let count = u16::try_from(ballots.rows.len()).map_err(|_| PollBallotsError::TooLong)?;
    let len = FIXED_LEN + ballots.rows.iter().map(|r| 2 + r.len()).sum::<usize>();
    if len > POLL_BALLOTS_ENVELOPE_MAX_LEN {
        return Err(PollBallotsError::TooLong);
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(POLL_BALLOTS_ENVELOPE_DOMAIN_V1);
    out.push(POLL_BALLOTS_VERSION_V1);
    out.extend_from_slice(&ballots.poll_ref.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    for row in &ballots.rows {
        // Each row is at most POLL_BALLOTS_MAX_ROW_LEN, which fits a u16.
        out.extend_from_slice(&u16::try_from(row.len()).unwrap_or(u16::MAX).to_be_bytes());
        out.extend_from_slice(row);
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// Every [`PollBallotsError`] but [`PollBallotsError::WrongDomain`]; the
/// version is checked before any length.
pub fn try_unwrap_poll_ballots(plaintext: &[u8]) -> Result<Option<PollBallots>> {
    let Some(payload) = plaintext.strip_prefix(POLL_BALLOTS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(PollBallotsError::BadLength)?;
    if version != POLL_BALLOTS_VERSION_V1 {
        return Err(PollBallotsError::UnknownVersion(version));
    }
    if plaintext.len() > POLL_BALLOTS_ENVELOPE_MAX_LEN {
        return Err(PollBallotsError::TooLong);
    }
    if rest.len() < 10 {
        return Err(PollBallotsError::BadLength);
    }
    let (ref_bytes, rest) = rest.split_at(8);
    let mut a = [0u8; 8];
    a.copy_from_slice(ref_bytes);
    let poll_ref = i64::from_be_bytes(a);
    let (count_bytes, mut rest) = rest.split_at(2);
    let count = usize::from(u16::from_be_bytes([count_bytes[0], count_bytes[1]]));
    if count == 0 {
        return Err(PollBallotsError::Empty);
    }
    // Every row takes at least its two length bytes.
    if count.saturating_mul(2) > rest.len() {
        return Err(PollBallotsError::BadLength);
    }
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(count);
    for _ in 0..count {
        if rest.len() < 2 {
            return Err(PollBallotsError::BadLength);
        }
        let (len_bytes, tail) = rest.split_at(2);
        let len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
        if len > POLL_BALLOTS_MAX_ROW_LEN {
            return Err(PollBallotsError::Row);
        }
        if tail.len() < len {
            return Err(PollBallotsError::BadLength);
        }
        let (row, tail) = tail.split_at(len);
        if !row_ok(row) {
            return Err(PollBallotsError::Row);
        }
        if rows.last().is_some_and(|prev| prev.as_slice() >= row) {
            return Err(PollBallotsError::RowOrder);
        }
        rows.push(row.to_vec());
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(PollBallotsError::BadLength);
    }
    Ok(Some(PollBallots { poll_ref, rows }))
}

/// Strict standalone decoder.
///
/// # Errors
///
/// [`PollBallotsError::WrongDomain`], else what [`try_unwrap_poll_ballots`]
/// returns.
pub fn decode_poll_ballots(bytes: &[u8]) -> Result<PollBallots> {
    try_unwrap_poll_ballots(bytes)?.ok_or(PollBallotsError::WrongDomain)
}

/// Whether `plaintext` carries the poll-ballots domain (the notification
/// extension's suppress test).
#[must_use]
pub fn is_poll_ballots_envelope(plaintext: &[u8]) -> bool {
    plaintext.starts_with(POLL_BALLOTS_ENVELOPE_DOMAIN_V1)
}

/// Whether the committer hands over a stored ballot row whose DS
/// `created_at` is `created_at_ms`: when the journal may no longer hold it,
/// that is when it is older than [`JOURNAL_RETENTION_MS`] less
/// [`BALLOT_HANDOVER_MARGIN_MS`].
#[must_use]
pub const fn ballot_needs_handover(created_at_ms: i64, now_ms: i64) -> bool {
    created_at_ms <= now_ms.saturating_sub(JOURNAL_RETENTION_MS - BALLOT_HANDOVER_MARGIN_MS)
}

/// The committer's old-ballot envelopes for one poll: `rows` sorted, a row
/// given twice kept once, a row that is not a ballot row or is too long
/// dropped, packed into as few envelopes as fit. No rows gives no envelope.
#[must_use]
pub fn plan_poll_ballots_rows(poll_ref: i64, rows: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut sorted: Vec<&Vec<u8>> = rows.iter().filter(|r| row_ok(r)).collect();
    sorted.sort();
    sorted.dedup();
    let mut out = Vec::new();
    let mut current: Vec<Vec<u8>> = Vec::new();
    let mut current_len = FIXED_LEN;
    for row in sorted {
        let n = 2 + row.len();
        if !current.is_empty()
            && (current_len + n > POLL_BALLOTS_ENVELOPE_MAX_LEN
                || current.len() == usize::from(u16::MAX))
        {
            if let Ok(bytes) = compose_poll_ballots(&PollBallots {
                poll_ref,
                rows: core::mem::take(&mut current),
            }) {
                out.push(bytes);
            }
            current_len = FIXED_LEN;
        }
        current.push(row.clone());
        current_len += n;
    }
    if !current.is_empty() {
        if let Ok(bytes) = compose_poll_ballots(&PollBallots {
            poll_ref,
            rows: current,
        }) {
            out.push(bytes);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn row(tag: u8, len: usize) -> Vec<u8> {
        let mut r = POLL_BALLOTS_ROW_PREFIX.to_vec();
        r.resize(len.max(r.len()), tag);
        r
    }

    #[test]
    fn round_trips_and_is_canonical() {
        let b = PollBallots {
            poll_ref: 7,
            rows: vec![row(1, 2366), row(2, 830)],
        };
        let wire = compose_poll_ballots(&b).unwrap();
        assert_eq!(decode_poll_ballots(&wire).unwrap(), b);
        assert!(is_poll_ballots_envelope(&wire));
        assert!(wire.len() >= POLL_BALLOTS_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn refuses_what_it_should() {
        let b = PollBallots {
            poll_ref: 7,
            rows: vec![row(2, 100), row(1, 100)],
        };
        assert_eq!(compose_poll_ballots(&b), Err(PollBallotsError::RowOrder));
        let dup = PollBallots {
            poll_ref: 7,
            rows: vec![row(1, 100), row(1, 100)],
        };
        assert_eq!(compose_poll_ballots(&dup), Err(PollBallotsError::RowOrder));
        let foreign = PollBallots {
            poll_ref: 7,
            rows: vec![b"lakko/other/v1\0xx".to_vec()],
        };
        assert_eq!(compose_poll_ballots(&foreign), Err(PollBallotsError::Row));
        let empty = PollBallots {
            poll_ref: 7,
            rows: vec![],
        };
        assert_eq!(compose_poll_ballots(&empty), Err(PollBallotsError::Empty));
        let mut wire = compose_poll_ballots(&PollBallots {
            poll_ref: 1,
            rows: vec![row(1, 50)],
        })
        .unwrap();
        wire[POLL_BALLOTS_ENVELOPE_DOMAIN_V1.len()] = 2;
        assert_eq!(
            decode_poll_ballots(&wire),
            Err(PollBallotsError::UnknownVersion(2))
        );
        assert_eq!(
            decode_poll_ballots(b"lakko/open-items/v1\0\x01\0"),
            Err(PollBallotsError::WrongDomain)
        );
    }

    #[test]
    fn handover_cutoff() {
        let now = 100 * 24 * 60 * 60 * 1000;
        assert!(!ballot_needs_handover(now - 28 * 24 * 60 * 60 * 1000, now));
        assert!(ballot_needs_handover(now - 29 * 24 * 60 * 60 * 1000, now));
        assert!(ballot_needs_handover(i64::MIN, i64::MIN));
    }

    #[test]
    fn plan_splits_and_dedups() {
        let rows: Vec<Vec<u8>> = (0..800u16)
            .map(|i| {
                let mut r = row(0, 2366);
                r[30..32].copy_from_slice(&i.to_be_bytes());
                r
            })
            .chain([row(0, 2366), b"not a ballot".to_vec()])
            .collect();
        let envelopes = plan_poll_ballots_rows(9, &rows);
        assert!(envelopes.len() >= 2);
        let mut total = 0;
        for e in &envelopes {
            assert!(e.len() <= POLL_BALLOTS_ENVELOPE_MAX_LEN);
            let d = decode_poll_ballots(e).unwrap();
            assert_eq!(d.poll_ref, 9);
            total += d.rows.len();
        }
        // 800 distinct plus row(0, 2366), which equals the i == 0 row.
        assert_eq!(total, 800);
        assert!(plan_poll_ballots_rows(9, &[]).is_empty());
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        for d in [
            POLL_BALLOTS_ROW_PREFIX,
            crate::open_items::OPEN_ITEMS_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(
                !d.starts_with(POLL_BALLOTS_ENVELOPE_DOMAIN_V1)
                    && !POLL_BALLOTS_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let mut input = POLL_BALLOTS_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(b) = decode_poll_ballots(&input) {
                prop_assert_eq!(compose_poll_ballots(&b).unwrap(), input);
            }
        }
    }
}
