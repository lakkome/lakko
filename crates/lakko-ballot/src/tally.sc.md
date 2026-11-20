<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-ballot/src/tally.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if tally.rs changes and this file does not. -->

# `tally` — counting an anonymous poll (spec)

**Module:** `crates/lakko-ballot/src/tally.rs`
**One line:** Fold verified ballots into totals, one vote per tag, so every member counts the same.

## Public contract

`fold(rules, ballots) -> Tally { counts, voters }`.

- `PollRules { option_count, multiple, close_message_id, close_at_ms }` comes from the poll body and its accepted close (ADR-0092 section 3.5).
- Each `CountedBallot` is a verified ballot with its row's DS `messageId` and `created_at`.
- The input is sorted by `(message_id, created_at_ms, tag, choices)`; for each tag the last ballot that is before the close and whose choices fit the poll wins.
- A ballot after the close (`message_id` above the close's, or `created_at_ms` at or past `close_at_ms`) is ignored. A ballot with an index at or past `option_count`, or with more than one choice in a single-choice poll, is dropped and replaces nothing.
- An empty choice list wins like any ballot and counts as a retracted vote. `voters` is the number of tags whose winning ballot chose something; `counts` has `option_count` entries.

## Invariants upheld

- Total and order-independent: the same set of ballots in any order gives the same tally (property test `tally_is_independent_of_order`).
- One tag counts at most once, so one account counts at most once in a poll (`a_ring_extension_never_counts_one_account_twice`).
- Saturating counters; no panic.

## What this module does NOT do

It does not verify ballots (`ballot` does) and does not decide which close is accepted (the client applies the author / owner / admin rule).

## References

ADR-0092 sections 3.5 and 3.6 ("The tally").
