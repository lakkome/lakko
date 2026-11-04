// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The static retrieval filter of a directory (ADR-0086 section 2).
//!
//! A contact's phone holds this filter and tests a search term against it
//! with no network. It maps a term to the rows that carry it, and to nothing
//! else: there is no list of terms inside, only slots that a key XORs into
//! its value.
//!
//! ## Construction: a ribbon retrieval structure
//!
//! Each entry names `W` consecutive slots starting at a hashed position and a
//! hashed subset of them; the XOR of that subset must equal the entry's
//! value. That is a linear system over GF(2), solved on the fly by the Ribbon
//! construction (Dillinger and Walzer, 2021): insertion is Gaussian
//! elimination restricted to a band, back substitution fills the slots.
//! Slots no equation fixes are filled pseudo-randomly, so an absent term
//! reads a uniform value.
//!
//! The ADR leaves the family open between binary fuse and ribbon. Ribbon was
//! taken because it holds its overhead at the small sizes a directory has:
//! the binary fuse sizing formula asks for about 35% extra slots at 1,350
//! entries, which alone is a fourth of the budget of 3 bytes a term, where
//! the band here needs the few percent [`required_slots`] states.
//!
//! ## An entry
//!
//! A term can sit on several rows (two contacts named Anna). A retrieval
//! structure holds one value per key, so the key is `(tag, ordinal)`: the
//! rows of one tag are ordinals 0, 1, 2 ..., and a lookup walks them until
//! one does not match. At most [`MAX_ROWS_PER_TERM`] rows are kept per term,
//! the most a search reads; a term on more rows than that is one the search
//! asks more letters for anyway.
//!
//! ```text
//! h     = BLAKE3-keyed(hash_key, filter_version(8) || attempt(1) || tag(32) || ordinal(1))
//! start = h[0..8]  as a u64, scaled into 0 ..= slots - W
//! coeff = h[8..24] with bit 0 set
//! check = h[24..27]
//! value = (check << index_bits | row), 24 bits
//! ```
//!
//! `index_bits` is the width of a row index for this filter's row count, so a
//! small directory has a longer check and fewer false positives. A value is
//! 24 bits: at 16 index bits (the most) 8 bits of check remain.
//!
//! ## Wire
//!
//! ```text
//! wire  := format(1) = 0x01 || epoch(8) || filter_version(8) || nonce(12) || ciphertext
//! plain := attempt(1) || index_bits(1) || row_count(4) || slots(3 x n)
//! aad   := "lakko/circle/filter/v1\0" || directory_id || epoch(8) || filter_version(8)
//! ```
//!
//! The wire length is one of [`wire_lengths`]: the bucket a size falls in is
//! all the server sees of how many terms a directory holds. `epoch` is in the
//! clear so a reader knows which key opens it: a filter is sealed under the
//! epoch current when it was written, and a removal does not rewrite it.
//!
//! ## Hostile input
//!
//! [`open_filter`] runs on bytes the Delivery Service returns. Length, format
//! and bucket shape are checked before anything is decrypted, every field of
//! the plaintext before it is used, and nothing panics (fuzz target
//! `filter_open`).

use std::collections::BTreeMap;

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::{blake3_keyed, blake3_keyed_xof};
use lakko_crypto::{random, SecretBytes};

use crate::epoch::EpochKey;
use crate::error::CircleError;
use crate::keys::{DIRECTORY_ID_LEN, KEY_LEN};

/// Length of a term tag (`lakko_contacts::fof_tag`).
pub const TAG_LEN_BYTES: usize = 32;
/// Bytes per slot.
pub const SLOT_BYTES: usize = 3;
/// Bits per value.
const VALUE_BITS: u32 = 24;
/// Band width: how many consecutive slots one entry touches.
const BAND: usize = 128;
/// The band as a machine word.
type Band = u128;
/// Most rows one term maps to: the most rows a search reads (ADR-0086 section
/// 4, chosen there, not measured).
pub const MAX_ROWS_PER_TERM: usize = 20;
/// Most rows a directory holds.
pub const MAX_ROWS: u32 = 1 << 16;
/// Largest wire length of a filter (4 MiB). Chosen, not measured.
pub const MAX_FILTER_BYTES: usize = 4 * 1024 * 1024;
/// Smallest bucket (1 KiB). Chosen, not measured.
pub const MIN_FILTER_BYTES: usize = 1024;

/// Format byte.
const FORMAT_V1: u8 = 0x01;
const HEADER_LEN: usize = 1 + 8 + 8 + NONCE_LEN;
const PLAIN_HEADER_LEN: usize = 1 + 1 + 4;
const AAD_DOMAIN: &[u8] = b"lakko/circle/filter/v1\0";
/// Attempts per bucket before the next bucket is tried.
const ATTEMPTS: u8 = 16;

/// Bytes a sealed filter adds around its slots.
const OVERHEAD: usize = HEADER_LEN + TAG_LEN + PLAIN_HEADER_LEN;

/// The wire length a bucket of `bucket` bytes gives: the largest length of the
/// form overhead + 3n that fits.
const fn wire_len_for(bucket: usize) -> usize {
    bucket - (bucket - OVERHEAD) % SLOT_BYTES
}

/// Every wire length a filter may have, ascending: four steps per doubling
/// from [`MIN_FILTER_BYTES`] to [`MAX_FILTER_BYTES`], 49 lengths, each at most
/// 1.25 times the last. Chosen, not measured: a coarser ladder says less about
/// a directory's size and wastes more of the 3 bytes a term.
#[must_use]
pub fn wire_lengths() -> Vec<usize> {
    let mut out = Vec::with_capacity(49);
    let mut base = MIN_FILTER_BYTES;
    while base <= MAX_FILTER_BYTES {
        for step in 0..4 {
            let bucket = base + base / 4 * step;
            if bucket <= MAX_FILTER_BYTES {
                out.push(wire_len_for(bucket));
            }
        }
        base *= 2;
    }
    out
}

/// Slots needed for `entries` entries. Measured, see the `sizes` test: at a
/// band of 128 the construction succeeds within [`ATTEMPTS`] tries at this
/// load across the sizes a directory reaches.
#[must_use]
pub const fn required_slots(entries: usize) -> usize {
    entries + entries / 25 + BAND
}

/// One stored fact: this term tag is on this row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterEntry {
    /// The term tag.
    pub tag: [u8; TAG_LEN_BYTES],
    /// The row that carries the term.
    pub row: u32,
}

/// What building a filter did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterStats {
    /// Entries stored, after dropping repeats and rows past the per-term cap.
    pub stored_entries: usize,
    /// Entries dropped because a term sat on more than [`MAX_ROWS_PER_TERM`]
    /// rows.
    pub dropped_entries: usize,
    /// Slots in the filter.
    pub slots: usize,
    /// Length of the sealed filter.
    pub wire_len: usize,
    /// Which attempt the construction succeeded on.
    pub attempt: u8,
}

/// The width of a row index for `row_count` rows.
const fn index_bits_for(row_count: u32) -> u32 {
    if row_count <= 1 {
        1
    } else {
        32 - (row_count - 1).leading_zeros()
    }
}

struct Probe {
    start: usize,
    coeff: Band,
    check: u32,
}

fn probe(
    hash_key: &SecretBytes<KEY_LEN>,
    filter_version: u64,
    attempt: u8,
    tag: &[u8; TAG_LEN_BYTES],
    ordinal: u8,
    slots: usize,
) -> Probe {
    let mut input = [0u8; 8 + 1 + TAG_LEN_BYTES + 1];
    input[..8].copy_from_slice(&filter_version.to_be_bytes());
    input[8] = attempt;
    input[9..9 + TAG_LEN_BYTES].copy_from_slice(tag);
    input[9 + TAG_LEN_BYTES] = ordinal;
    let h = blake3_keyed(hash_key.as_bytes(), &input);
    let mut start_bytes = [0u8; 8];
    start_bytes.copy_from_slice(&h[..8]);
    let positions = (slots - BAND + 1) as u128;
    let start = (u128::from(u64::from_be_bytes(start_bytes)) * positions) >> 64;
    let mut coeff_bytes = [0u8; 16];
    coeff_bytes.copy_from_slice(&h[8..24]);
    let coeff = Band::from_be_bytes(coeff_bytes) | 1;
    let check = u32::from(h[24]) << 16 | u32::from(h[25]) << 8 | u32::from(h[26]);
    #[allow(clippy::cast_possible_truncation)] // below `slots`, a usize
    let start = start as usize;
    Probe {
        start,
        coeff,
        check,
    }
}

const fn value_of(check: u32, row: u32, index_bits: u32) -> u32 {
    let mask = (1u32 << VALUE_BITS) - 1;
    ((check << index_bits) | row) & mask
}

/// A term tag, the ordinal of the row under it, and the row.
type KeyedEntry<'a> = (&'a [u8; TAG_LEN_BYTES], u8, u32);
/// The entries to store, and how many the per-term cap dropped.
type Keyed<'a> = (Vec<KeyedEntry<'a>>, usize);

/// Solve the band for `entries`. `None` when the system has no solution with
/// these hashes, which a new attempt fixes.
fn solve(
    hash_key: &SecretBytes<KEY_LEN>,
    filter_version: u64,
    attempt: u8,
    entries: &[KeyedEntry<'_>],
    slots: usize,
    index_bits: u32,
) -> Option<Vec<u8>> {
    let mut coeffs: Vec<Band> = vec![0; slots];
    let mut results: Vec<u32> = vec![0; slots];
    for (tag, ordinal, row) in entries {
        let p = probe(hash_key, filter_version, attempt, tag, *ordinal, slots);
        let mut at = p.start;
        let mut coeff = p.coeff;
        let mut result = value_of(p.check, *row, index_bits);
        loop {
            let held = coeffs[at];
            if held == 0 {
                coeffs[at] = coeff;
                results[at] = result;
                break;
            }
            coeff ^= held;
            result ^= results[at];
            if coeff == 0 {
                if result == 0 {
                    break;
                }
                return None;
            }
            let shift = coeff.trailing_zeros();
            at += shift as usize;
            coeff >>= shift;
        }
    }
    // Slots no equation fixes get pseudo-random values, so a term that is
    // not stored reads a uniform value whichever slots it lands on.
    let free = blake3_keyed_xof(
        hash_key.as_bytes(),
        &[&filter_version.to_be_bytes()[..], &[attempt]].concat(),
        slots * SLOT_BYTES,
    );
    let mut out = vec![0u8; slots * SLOT_BYTES];
    for i in (0..slots).rev() {
        let offset = i * SLOT_BYTES;
        let coeff = coeffs[i];
        if coeff == 0 {
            out[offset..offset + SLOT_BYTES].copy_from_slice(&free[offset..offset + SLOT_BYTES]);
            continue;
        }
        let mut value = results[i];
        let mut rest = coeff >> 1;
        let mut position = i + 1;
        while rest != 0 {
            let shift = rest.trailing_zeros() as usize;
            position += shift;
            rest >>= shift;
            value ^= slot_at(&out, position);
            rest >>= 1;
            position += 1;
        }
        write_slot(&mut out, i, value);
    }
    Some(out)
}

fn slot_at(slots: &[u8], index: usize) -> u32 {
    let at = index * SLOT_BYTES;
    u32::from(slots[at]) << 16 | u32::from(slots[at + 1]) << 8 | u32::from(slots[at + 2])
}

#[allow(clippy::cast_possible_truncation)] // masked to one byte each
const fn slot_bytes(value: u32) -> [u8; SLOT_BYTES] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

fn write_slot(slots: &mut [u8], index: usize, value: u32) {
    let at = index * SLOT_BYTES;
    slots[at..at + SLOT_BYTES].copy_from_slice(&slot_bytes(value));
}

fn aad(directory_id: &[u8; DIRECTORY_ID_LEN], epoch: u64, filter_version: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_DOMAIN.len() + DIRECTORY_ID_LEN + 16);
    out.extend_from_slice(AAD_DOMAIN);
    out.extend_from_slice(directory_id);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&filter_version.to_be_bytes());
    out
}

/// Group `entries` by tag, repeats removed, rows ascending, at most
/// [`MAX_ROWS_PER_TERM`] per tag. Returns the keyed entries and how many were
/// dropped for the cap.
fn keyed_entries(entries: &[FilterEntry]) -> Keyed<'_> {
    let mut by_tag: BTreeMap<&[u8; TAG_LEN_BYTES], Vec<u32>> = BTreeMap::new();
    for entry in entries {
        by_tag.entry(&entry.tag).or_default().push(entry.row);
    }
    let mut out = Vec::with_capacity(entries.len());
    let mut dropped = 0usize;
    for (tag, mut rows) in by_tag {
        rows.sort_unstable();
        rows.dedup();
        dropped += rows.len().saturating_sub(MAX_ROWS_PER_TERM);
        for (ordinal, row) in rows.into_iter().take(MAX_ROWS_PER_TERM).enumerate() {
            #[allow(clippy::cast_possible_truncation)] // below MAX_ROWS_PER_TERM
            out.push((tag, ordinal as u8, row));
        }
    }
    (out, dropped)
}

/// Build and seal a filter over `entries` under `key`.
///
/// `row_count` is the number of row indices in use; every entry's row must be
/// below it.
///
/// # Errors
///
/// [`CircleError::RowOutOfRange`] for a row at or past `row_count`, or a
/// `row_count` above [`MAX_ROWS`]; [`CircleError::FilterTooLarge`] when the
/// entries do not fit the largest bucket; [`CircleError::Seal`] on a cipher
/// failure.
pub fn seal_filter(
    key: &EpochKey,
    filter_version: u64,
    entries: &[FilterEntry],
    row_count: u32,
) -> Result<(Vec<u8>, FilterStats), CircleError> {
    if row_count > MAX_ROWS || entries.iter().any(|e| e.row >= row_count) {
        return Err(CircleError::RowOutOfRange);
    }
    let (keyed, dropped) = keyed_entries(entries);
    let index_bits = index_bits_for(row_count);
    let needed = required_slots(keyed.len());
    let hash_key = key.filter_hash_key();
    for wire_len in wire_lengths() {
        let slots = (wire_len - OVERHEAD) / SLOT_BYTES;
        if slots < needed {
            continue;
        }
        for attempt in 0..ATTEMPTS {
            let Some(solved) = solve(
                &hash_key,
                filter_version,
                attempt,
                &keyed,
                slots,
                index_bits,
            ) else {
                continue;
            };
            let wire = seal_plain(key, filter_version, attempt, index_bits, row_count, &solved)?;
            return Ok((
                wire,
                FilterStats {
                    stored_entries: keyed.len(),
                    dropped_entries: dropped,
                    slots,
                    wire_len,
                    attempt,
                },
            ));
        }
    }
    Err(CircleError::FilterTooLarge)
}

fn seal_plain(
    key: &EpochKey,
    filter_version: u64,
    attempt: u8,
    index_bits: u32,
    row_count: u32,
    slots: &[u8],
) -> Result<Vec<u8>, CircleError> {
    let mut plain = Vec::with_capacity(PLAIN_HEADER_LEN + slots.len());
    plain.push(attempt);
    #[allow(clippy::cast_possible_truncation)] // at most 16
    plain.push(index_bits as u8);
    plain.extend_from_slice(&row_count.to_be_bytes());
    plain.extend_from_slice(slots);
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aead_key: AeadKey = key.filter_seal_key();
    let sealed = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &aad(key.directory_id(), key.epoch(), filter_version),
        &plain,
    )
    .map_err(|_| CircleError::Seal)?;
    let mut wire = Vec::with_capacity(HEADER_LEN + sealed.len());
    wire.push(FORMAT_V1);
    wire.extend_from_slice(&key.epoch().to_be_bytes());
    wire.extend_from_slice(&filter_version.to_be_bytes());
    wire.extend_from_slice(&nonce);
    wire.extend_from_slice(&sealed);
    Ok(wire)
}

/// The clear header of a sealed filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterHeader {
    /// The epoch whose key sealed it.
    pub epoch: u64,
    /// Its version, from the directory head when it was written.
    pub filter_version: u64,
}

/// Read the clear header of a sealed filter, to learn which epoch key opens
/// it.
///
/// # Errors
///
/// [`CircleError::Length`] when the length is not a bucket,
/// [`CircleError::UnknownFormat`] on any other format byte.
pub fn filter_header(wire: &[u8]) -> Result<FilterHeader, CircleError> {
    if wire.len() > MAX_FILTER_BYTES || !wire_lengths().contains(&wire.len()) {
        return Err(CircleError::Length);
    }
    let (format, rest) = wire.split_first().ok_or(CircleError::Length)?;
    if *format != FORMAT_V1 {
        return Err(CircleError::UnknownFormat);
    }
    let epoch = u64_at(rest, 0)?;
    let filter_version = u64_at(rest, 8)?;
    Ok(FilterHeader {
        epoch,
        filter_version,
    })
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64, CircleError> {
    let end = at.checked_add(8).ok_or(CircleError::Length)?;
    let field: [u8; 8] = bytes
        .get(at..end)
        .and_then(|s| s.try_into().ok())
        .ok_or(CircleError::Length)?;
    Ok(u64::from_be_bytes(field))
}

/// An opened filter, ready to test terms.
#[derive(Clone)]
pub struct OpenedFilter {
    hash_key: SecretBytes<KEY_LEN>,
    filter_version: u64,
    attempt: u8,
    index_bits: u32,
    row_count: u32,
    slots: Vec<u8>,
}

impl core::fmt::Debug for OpenedFilter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenedFilter")
            .field("filter_version", &self.filter_version)
            .field("row_count", &self.row_count)
            .field("slots", &self.slot_count())
            .finish_non_exhaustive()
    }
}

/// Open a sealed filter under the key of the epoch it names.
///
/// # Errors
///
/// [`CircleError::WrongEpoch`] when `key` is another epoch's,
/// [`CircleError::Authentication`] when it does not open, and what
/// [`filter_header`] and the plaintext checks return.
pub fn open_filter(key: &EpochKey, wire: &[u8]) -> Result<OpenedFilter, CircleError> {
    let header = filter_header(wire)?;
    if header.epoch != key.epoch() {
        return Err(CircleError::WrongEpoch);
    }
    let nonce: Nonce = wire
        .get(17..HEADER_LEN)
        .and_then(|s| s.try_into().ok())
        .ok_or(CircleError::Length)?;
    let ciphertext = wire.get(HEADER_LEN..).ok_or(CircleError::Length)?;
    let aead_key: AeadKey = key.filter_seal_key();
    let plain = aead::open(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &aad(key.directory_id(), key.epoch(), header.filter_version),
        ciphertext,
    )
    .map_err(|_| CircleError::Authentication)?;
    parse_plain(key.filter_hash_key(), header.filter_version, &plain)
}

/// Parse an opened plaintext. Authenticated bytes, but written by the owner,
/// so bounded as any other input.
pub(crate) fn parse_plain(
    hash_key: SecretBytes<KEY_LEN>,
    filter_version: u64,
    plain: &[u8],
) -> Result<OpenedFilter, CircleError> {
    if plain.len() < PLAIN_HEADER_LEN {
        return Err(CircleError::Length);
    }
    let (fixed, slots) = plain.split_at(PLAIN_HEADER_LEN);
    let attempt = fixed[0];
    let index_bits = u32::from(fixed[1]);
    let row_count = u32::from_be_bytes([fixed[2], fixed[3], fixed[4], fixed[5]]);
    if row_count > MAX_ROWS || index_bits != index_bits_for(row_count) {
        return Err(CircleError::Malformed);
    }
    if slots.len() % SLOT_BYTES != 0 || slots.len() / SLOT_BYTES < BAND {
        return Err(CircleError::Length);
    }
    Ok(OpenedFilter {
        hash_key,
        filter_version,
        attempt,
        index_bits,
        row_count,
        slots: slots.to_vec(),
    })
}

impl OpenedFilter {
    /// The rows this term sits on. Never misses a stored term (up to
    /// [`MAX_ROWS_PER_TERM`] rows). A term that is not stored returns nothing
    /// or, rarely, a row that does not carry it: the caller opens the row and
    /// compares, which is what a false positive costs.
    #[must_use]
    pub fn lookup(&self, tag: &[u8; TAG_LEN_BYTES]) -> Vec<u32> {
        let mut rows = Vec::new();
        for ordinal in 0..MAX_ROWS_PER_TERM {
            #[allow(clippy::cast_possible_truncation)] // below MAX_ROWS_PER_TERM
            let Some(row) = self.value(tag, ordinal as u8) else {
                break;
            };
            rows.push(row);
        }
        rows
    }

    fn value(&self, tag: &[u8; TAG_LEN_BYTES], ordinal: u8) -> Option<u32> {
        let slots = self.slot_count();
        let p = probe(
            &self.hash_key,
            self.filter_version,
            self.attempt,
            tag,
            ordinal,
            slots,
        );
        let mut value = 0u32;
        let mut rest = p.coeff;
        let mut position = p.start;
        while rest != 0 {
            let shift = rest.trailing_zeros() as usize;
            position += shift;
            rest >>= shift;
            value ^= slot_at(&self.slots, position);
            rest >>= 1;
            position += 1;
        }
        let expected = value_of(p.check, 0, self.index_bits);
        let index_mask = (1u32 << self.index_bits) - 1;
        let row = value & index_mask;
        (value & !index_mask == expected & !index_mask && row < self.row_count).then_some(row)
    }

    /// Slots in the filter.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len() / SLOT_BYTES
    }

    /// Row indices in use.
    #[must_use]
    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    /// The bits of check a value carries.
    #[must_use]
    pub const fn check_bits(&self) -> u32 {
        VALUE_BITS - self.index_bits
    }
}
