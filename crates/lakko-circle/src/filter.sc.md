<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: regenerate from the source when filter.rs changes -->

# `filter` — the static retrieval filter of a circle directory (spec)

**Module:** `crates/lakko-circle/src/filter.rs`   **One line:** a compact sealed map from a term tag to the rows carrying it, tested on the phone with no network.

## Why this module exists

A search tests every contact's filter locally, so nobody's phone wakes and no request says who was looked for. The filter must be small enough to hold one per contact, and must hold no list of terms.

## Public contract

- `seal_filter(key, filter_version, entries, row_count)`: builds and seals. Repeats collapse; a term keeps at most `MAX_ROWS_PER_TERM` rows, the lowest; the rest are counted in `FilterStats::dropped_entries`.
- `filter_header(wire)`: the clear epoch and version, so a reader knows which key opens it.
- `open_filter(key, wire)`: authenticates and parses.
- `OpenedFilter::lookup(tag)`: every stored row of the term, up to the cap. A term that is not stored answers nothing, or rarely a row that does not carry it.

## Construction

A ribbon retrieval structure, band 128, 24 bits per slot. Each entry `(tag, ordinal)` hashes to a start, a subset of 128 consecutive slots, and a check; the XOR of the subset equals `check << index_bits | row`. The ordinal is how a term reaches several rows: a lookup walks ordinals until one does not match. `index_bits` fits the row count, so a smaller directory has a longer check. Slots no equation fixes are filled pseudo-randomly.

## Wire layout

```text
wire  := format(1) = 0x01 || epoch(8) || filter_version(8) || nonce(12) || ciphertext
plain := attempt(1) || index_bits(1) || row_count(4) || slots(3 x n)
aad   := "lakko/circle/filter/v1\0" || directory_id || epoch(8) || filter_version(8)
```

The length is one of `wire_lengths()`: 49 buckets, four steps per doubling, 1 KiB to 4 MiB.

## Invariants upheld

- A stored term is never missed (`a_filter_never_misses_a_stored_term`).
- A false positive needs the check to match: at most `2^-check_bits` per filter (`the_false_positive_rate_and_the_size_measured`).
- A filter is sealed under one epoch's key and opens under no other.
- `index_bits` is the one value `row_count` gives, so an opened filter has one reading.

## Failure modes

`RowOutOfRange`, `FilterTooLarge`, `Seal` on build; `Length`, `UnknownFormat`, `WrongEpoch`, `Authentication`, `Malformed` on open. No panic on any input.

## What this module does NOT do

- It does not filter by a whole query: a hit is a candidate, and the caller compares the opened card.
- It has no deltas. Any change rewrites the whole filter.
- It does not hide its bucket from the server.

## Attacker exposure

`open_filter` runs on bytes the Delivery Service returns. Length, format and bucket are checked before decryption, the plaintext fields before use. Fuzz target `filter_open`, which also drives the plaintext parser directly, since a fuzzer cannot reach it through a tag.

## References

ADR-0086 section 2; Dillinger and Walzer, Ribbon filter, 2021; `tests/filter.rs`.
