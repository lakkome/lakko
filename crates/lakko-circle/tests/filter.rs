// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The retrieval filter (ADR-0086 section 2): it never misses a stored term,
//! and how often it answers for a term it does not hold, measured.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use lakko_circle::filter::{seal_filter, wire_lengths};
use lakko_circle::{
    filter_header, open_filter, DirectoryWriter, EpochKey, FilterEntry, MAX_ROWS_PER_TERM,
};
use lakko_crypto::hash::blake3;
use proptest::prelude::*;

fn key() -> EpochKey {
    DirectoryWriter::create(&[0x77; 32]).unwrap().epoch_key()
}

fn tag(n: u64) -> [u8; 32] {
    blake3(&n.to_be_bytes())
}

/// A directory of `people`, each with `terms` terms of their own.
fn entries(people: u32, terms: u64) -> Vec<FilterEntry> {
    let mut out = Vec::new();
    for row in 0..people {
        for term in 0..terms {
            out.push(FilterEntry {
                tag: tag(u64::from(row) * 100 + term),
                row,
            });
        }
    }
    out
}

const FALSE_POSITIVE_TRIES: u64 = 2_000_000;

fn false_positive_rate(people: u32, terms: u64) -> (f64, u32, usize, usize) {
    let k = key();
    let stored = entries(people, terms);
    let (wire, stats) = seal_filter(&k, 1, &stored, people).unwrap();
    let opened = open_filter(&k, &wire).unwrap();
    let mut hits = 0u64;
    for i in 0..FALSE_POSITIVE_TRIES {
        if !opened.lookup(&tag(1 << 40 | i)).is_empty() {
            hits += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let rate = hits as f64 / FALSE_POSITIVE_TRIES as f64;
    (rate, opened.check_bits(), stats.stored_entries, wire.len())
}

#[test]
fn the_false_positive_rate_and_the_size_measured() {
    // Run with --nocapture to read the numbers. The bound is the arithmetic:
    // a term that is not stored matches its check with probability 2^-check,
    // and then needs a row index in range.
    for (people, terms) in [(150u32, 9u64), (1_024, 9), (10_000, 9)] {
        let (rate, check_bits, stored, wire_len) = false_positive_rate(people, terms);
        #[allow(clippy::cast_precision_loss)]
        let per_term = wire_len as f64 / stored as f64;
        println!(
            "{people} rows, {stored} terms: {wire_len} bytes, {per_term:.2} bytes per term, \
             {check_bits} check bits, false positive rate {rate:.6} per filter"
        );
        let expected = 1.0 / f64::from(1u32 << check_bits);
        assert!(rate < expected * 1.3, "rate {rate} expected {expected}");
    }
}

#[test]
fn a_full_size_directory_builds() {
    // 65,536 rows of 9 terms is the most the tree holds.
    let k = key();
    let stored = entries(65_536, 9);
    let (wire, stats) = seal_filter(&k, 1, &stored, 65_536).unwrap();
    assert_eq!(stats.stored_entries, stored.len());
    assert!(wire.len() <= lakko_circle::MAX_FILTER_BYTES);
    let opened = open_filter(&k, &wire).unwrap();
    for entry in stored.iter().step_by(997) {
        assert!(opened.lookup(&entry.tag).contains(&entry.row));
    }
    #[allow(clippy::cast_precision_loss)]
    let per_term = wire.len() as f64 / stored.len() as f64;
    println!(
        "65536 rows: {} bytes, {per_term:.2} bytes per term",
        wire.len()
    );
}

#[test]
fn an_empty_filter_builds_and_answers_nothing_stored() {
    let k = key();
    let (wire, _) = seal_filter(&k, 1, &[], 0).unwrap();
    assert_eq!(wire.len(), wire_lengths()[0]);
    let opened = open_filter(&k, &wire).unwrap();
    assert!(opened.lookup(&tag(1)).is_empty());
}

#[test]
fn a_term_on_more_rows_than_the_cap_keeps_the_cap() {
    let k = key();
    let term = tag(5);
    let many: Vec<FilterEntry> = (0..30).map(|row| FilterEntry { tag: term, row }).collect();
    let (wire, stats) = seal_filter(&k, 1, &many, 30).unwrap();
    assert_eq!(stats.stored_entries, MAX_ROWS_PER_TERM);
    assert_eq!(stats.dropped_entries, 10);
    let rows = open_filter(&k, &wire).unwrap().lookup(&term);
    assert_eq!(rows, (0..20).collect::<Vec<u32>>());
}

#[test]
fn a_row_past_the_count_is_refused() {
    let k = key();
    let entry = FilterEntry {
        tag: tag(1),
        row: 3,
    };
    assert_eq!(
        seal_filter(&k, 1, &[entry], 3).map(|_| ()),
        Err(lakko_circle::CircleError::RowOutOfRange)
    );
}

#[test]
fn a_filter_opens_under_its_epoch_only() {
    let mut writer = DirectoryWriter::create(&[0x78; 32]).unwrap();
    writer.take_write().unwrap();
    let grant = writer.add_member().unwrap();
    let k = writer.epoch_key();
    let (wire, _) = seal_filter(&k, 1, &entries(4, 3), 4).unwrap();
    assert_eq!(filter_header(&wire).unwrap().epoch, k.epoch());
    writer.remove_members(&[grant.leaf]).unwrap();
    assert_eq!(
        open_filter(&writer.epoch_key(), &wire).map(|_| ()),
        Err(lakko_circle::CircleError::WrongEpoch)
    );
    let mut flipped = wire;
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert_eq!(
        open_filter(&k, &flipped).map(|_| ()),
        Err(lakko_circle::CircleError::Authentication)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every stored (term, row) pair comes back, for any mix of terms shared
    /// across rows, up to the per-term cap.
    #[test]
    fn a_filter_never_misses_a_stored_term(
        pairs in proptest::collection::vec((0u64..200, 0u32..500), 0..3000),
    ) {
        let k = key();
        let stored: Vec<FilterEntry> = pairs
            .iter()
            .map(|(term, row)| FilterEntry { tag: tag(*term), row: *row })
            .collect();
        let (wire, _) = seal_filter(&k, 7, &stored, 500).unwrap();
        let opened = open_filter(&k, &wire).unwrap();
        let mut by_term: BTreeMap<u64, BTreeSet<u32>> = BTreeMap::new();
        for (term, row) in &pairs {
            by_term.entry(*term).or_default().insert(*row);
        }
        for (term, rows) in by_term {
            let found: BTreeSet<u32> = opened.lookup(&tag(term)).into_iter().collect();
            let kept: BTreeSet<u32> = rows.iter().copied().take(MAX_ROWS_PER_TERM).collect();
            prop_assert!(kept.is_subset(&found));
        }
    }

    /// Whatever the input, a build either succeeds or refuses, and a filter's
    /// wire length is one of the buckets.
    #[test]
    fn a_filter_is_always_a_bucket(people in 0u32..400, terms in 1u64..12) {
        let k = key();
        let (wire, _) = seal_filter(&k, 1, &entries(people, terms), people).unwrap();
        prop_assert!(wire_lengths().contains(&wire.len()));
    }
}
