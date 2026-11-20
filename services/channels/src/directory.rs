// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channel directory's pure rules ([ADR-0059] sections 4-5): subscriber
//! bands, the bucket score, and the search query rule.
//!
//! Stores return every candidate of a bucket or a search with its counts;
//! this module orders them and cuts the page. Nothing here reads a clock or
//! a store, so the order is identical for every caller and testable alone.
//!
//! # The score (chosen, not measured)
//!
//! A listing's score is the sum of three terms, each over public signals
//! only:
//!
//! - **size**: the subscriber band index (0 for `<10` up to 5 for `100k+`)
//!   times [`SIZE_POINTS_PER_BAND`];
//! - **growth**: the band index of the registrations of the last
//!   [`GROWTH_WINDOW_MS`] times [`GROWTH_POINTS_PER_BAND`], so a channel
//!   gaining 100 subscribers a week ranks near one twice its size that gains
//!   none;
//! - **freshness**: [`FRESH_BONUS_MAX`] on the day a channel is first listed,
//!   decaying linearly to 0 over [`FRESH_WINDOW_MS`] (the maintainer's answer
//!   to open question 3), so a new channel is seen before it has subscribers
//!   to earn a place. It is measured from the first listing, which survives a
//!   withdrawal, so withdrawing and relisting does not renew it.
//!
//! Ties break by first listing time (older first) and then by channel id, so
//! the order is total. The exact count is never an input to the order beyond
//! its band, which keeps the page from ranking two channels of one band by a
//! count the page does not publish.
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use std::cmp::Ordering;

use crate::storage::ChannelId;

/// Listings per bucket page (ADR-0059 section 5, chosen).
pub const BUCKET_PAGE_LEN: usize = 100;
/// Results per search (ADR-0059 section 5, chosen).
pub const SEARCH_PAGE_LEN: usize = 24;
/// The growth signal's window: registrations of the last 7 days.
pub const GROWTH_WINDOW_MS: u64 = 7 * 24 * 3600 * 1000;
/// How long a freshly listed channel keeps a bonus (chosen).
pub const FRESH_WINDOW_MS: u64 = 14 * 24 * 3600 * 1000;
/// Points per subscriber band (chosen).
pub const SIZE_POINTS_PER_BAND: u64 = 100;
/// Points per band of 7-day registrations (chosen).
pub const GROWTH_POINTS_PER_BAND: u64 = 60;
/// The fresh-listing bonus at the moment of first listing (chosen; worth a
/// band and a half of size).
pub const FRESH_BONUS_MAX: u64 = 150;
/// Shortest search query, in characters after trimming.
pub const SEARCH_MIN_CHARS: usize = 2;
/// Longest search query, in characters after trimming.
pub const SEARCH_MAX_CHARS: usize = 64;

/// The published subscriber bands, lowest first.
pub const BANDS: [&str; 6] = ["<10", "10+", "100+", "1k+", "10k+", "100k+"];

/// The index into [`BANDS`] of `count`.
#[must_use]
pub const fn band_index(count: u64) -> usize {
    match count {
        0..=9 => 0,
        10..=99 => 1,
        100..=999 => 2,
        1_000..=9_999 => 3,
        10_000..=99_999 => 4,
        _ => 5,
    }
}

/// The published band of `count`.
#[must_use]
pub const fn band(count: u64) -> &'static str {
    BANDS[band_index(count)]
}

/// One listing with the counts the score needs, as a store returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// The channel.
    pub channel: ChannelId,
    /// The canonical listing bytes, served verbatim.
    pub listing: Vec<u8>,
    /// Registered subscribers now.
    pub subscribers: u64,
    /// Registrations in the last [`GROWTH_WINDOW_MS`].
    pub recent_registrations: u64,
    /// When the channel was first listed, unix milliseconds.
    pub first_listed_at_ms: u64,
}

/// The fresh-listing bonus of a channel first listed at `first_listed_at_ms`
/// seen at `now_ms`.
#[must_use]
pub fn fresh_bonus(first_listed_at_ms: u64, now_ms: u64) -> u64 {
    let age = now_ms.saturating_sub(first_listed_at_ms);
    if age >= FRESH_WINDOW_MS {
        return 0;
    }
    // Linear decay; u128 so the product cannot overflow.
    let remaining = u128::from(FRESH_WINDOW_MS - age);
    let bonus = u128::from(FRESH_BONUS_MAX) * remaining / u128::from(FRESH_WINDOW_MS);
    u64::try_from(bonus).unwrap_or(FRESH_BONUS_MAX)
}

/// The score of `entry` at `now_ms` (module docs).
#[must_use]
pub fn score(entry: &DirectoryEntry, now_ms: u64) -> u64 {
    let size = band_index(entry.subscribers) as u64 * SIZE_POINTS_PER_BAND;
    let growth = band_index(entry.recent_registrations) as u64 * GROWTH_POINTS_PER_BAND;
    size + growth + fresh_bonus(entry.first_listed_at_ms, now_ms)
}

/// Order `entries` by the score at `now_ms` and keep the first `limit`.
#[must_use]
pub fn rank(mut entries: Vec<DirectoryEntry>, now_ms: u64, limit: usize) -> Vec<DirectoryEntry> {
    entries.sort_by(|a, b| compare(a, b, now_ms));
    entries.truncate(limit);
    entries
}

fn compare(a: &DirectoryEntry, b: &DirectoryEntry, now_ms: u64) -> Ordering {
    score(b, now_ms)
        .cmp(&score(a, now_ms))
        .then(a.first_listed_at_ms.cmp(&b.first_listed_at_ms))
        .then(a.channel.cmp(&b.channel))
}

/// Why a search query was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchQueryError {
    /// Shorter than [`SEARCH_MIN_CHARS`] after trimming.
    TooShort,
    /// Longer than [`SEARCH_MAX_CHARS`].
    TooLong,
    /// Carries a control character.
    ControlCharacter,
}

/// Normalise a search query: trim, bound, refuse control characters, and
/// lowercase. The result is what both stores match case-insensitively as a
/// substring of a listing's name, handle or description.
///
/// # Errors
///
/// A [`SearchQueryError`].
pub fn normalize_search_query(raw: &str) -> Result<String, SearchQueryError> {
    let q = raw.trim();
    let chars = q.chars().count();
    if chars < SEARCH_MIN_CHARS {
        return Err(SearchQueryError::TooShort);
    }
    if chars > SEARCH_MAX_CHARS {
        return Err(SearchQueryError::TooLong);
    }
    if q.chars().any(char::is_control) {
        return Err(SearchQueryError::ControlCharacter);
    }
    Ok(q.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 3600 * 1000;

    fn entry(id: u8, subscribers: u64, recent: u64, listed_at: u64) -> DirectoryEntry {
        DirectoryEntry {
            channel: ChannelId::from_bytes(&[id; 16]).unwrap(),
            listing: vec![id],
            subscribers,
            recent_registrations: recent,
            first_listed_at_ms: listed_at,
        }
    }

    #[test]
    fn bands_have_their_published_edges() {
        let cases = [
            (0, "<10"),
            (9, "<10"),
            (10, "10+"),
            (99, "10+"),
            (100, "100+"),
            (999, "100+"),
            (1_000, "1k+"),
            (9_999, "1k+"),
            (10_000, "10k+"),
            (99_999, "10k+"),
            (100_000, "100k+"),
            (u64::MAX, "100k+"),
        ];
        for (count, expected) in cases {
            assert_eq!(band(count), expected, "{count}");
        }
    }

    #[test]
    fn the_fresh_bonus_decays_to_zero_over_its_window() {
        let t0 = 1_000 * DAY;
        assert_eq!(fresh_bonus(t0, t0), FRESH_BONUS_MAX);
        assert_eq!(
            fresh_bonus(t0, t0 + FRESH_WINDOW_MS / 2),
            FRESH_BONUS_MAX / 2
        );
        assert_eq!(fresh_bonus(t0, t0 + FRESH_WINDOW_MS), 0);
        assert_eq!(fresh_bonus(t0, t0 + 100 * DAY), 0);
        // A clock behind the listing time reads as age 0, never more.
        assert_eq!(fresh_bonus(t0, t0 - DAY), FRESH_BONUS_MAX);
    }

    #[test]
    fn size_then_growth_then_freshness_order_the_page() {
        let now = 1_000 * DAY;
        let old = now - 60 * DAY;
        let big = entry(1, 5_000, 0, old);
        let small_growing = entry(2, 150, 150, old);
        let small = entry(3, 150, 0, old);
        let fresh = entry(4, 0, 0, now);
        let ranked = rank(
            vec![
                small.clone(),
                fresh.clone(),
                big.clone(),
                small_growing.clone(),
            ],
            now,
            10,
        );
        // big: 300; small_growing: 200 + 120 = 320; small: 200; fresh: 150.
        assert_eq!(ranked, vec![small_growing, big, small, fresh]);
    }

    #[test]
    fn ties_break_by_listing_age_then_channel_id_and_the_page_is_cut() {
        let now = 1_000 * DAY;
        let old = now - 60 * DAY;
        let a = entry(9, 20, 0, old - DAY);
        let b = entry(1, 20, 0, old);
        let c = entry(2, 20, 0, old);
        let ranked = rank(vec![c.clone(), b.clone(), a.clone()], now, 2);
        assert_eq!(ranked, vec![a, b]);
    }

    #[test]
    fn the_order_does_not_depend_on_the_input_order() {
        let now = 1_000 * DAY;
        let set: Vec<_> = (0u8..20)
            .map(|i| {
                entry(
                    i,
                    u64::from(i) * 37,
                    u64::from(i % 3) * 11,
                    now - u64::from(i) * DAY,
                )
            })
            .collect();
        let mut reversed = set.clone();
        reversed.reverse();
        assert_eq!(rank(set, now, 100), rank(reversed, now, 100));
    }

    #[test]
    fn search_queries_are_trimmed_bounded_and_lowercased() {
        assert_eq!(
            normalize_search_query("  Rust News "),
            Ok("rust news".into())
        );
        assert_eq!(
            normalize_search_query(" a "),
            Err(SearchQueryError::TooShort)
        );
        assert_eq!(
            normalize_search_query(&"x".repeat(SEARCH_MAX_CHARS + 1)),
            Err(SearchQueryError::TooLong)
        );
        assert_eq!(
            normalize_search_query("ab\ncd"),
            Err(SearchQueryError::ControlCharacter)
        );
    }
}
