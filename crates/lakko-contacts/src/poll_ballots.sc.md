<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-contacts/src/poll_ballots.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if poll_ballots.rs changes and this file does not. -->

# `poll_ballots` — old ballot rows for a newcomer (spec)

**Module:** `crates/lakko-contacts/src/poll_ballots.rs`
**One line:** The envelope a committer uses to hand a newcomer the sealed ballot rows of an open anonymous poll that the journal may no longer hold.

## Why this module exists

A newcomer reads an anonymous poll's ballots from the group journal from the poll's row onward. The DS keeps rows for 30 days (ADR-0024), so a poll open longer has ballots the newcomer cannot fetch. The committer sends its stored copies; they are the sealed rows themselves, which the newcomer opens and verifies like any ballot, so the committer cannot forge one.

## Public contract

- `compose_poll_ballots` / `decode_poll_ballots` / `try_unwrap_poll_ballots`: canonical and bounded; `compose` refuses what `decode` refuses.
- `ballot_needs_handover(created_at_ms, now_ms)`: true when the row is older than the retention less one day (`BALLOT_HANDOVER_MARGIN_MS`, chosen); a duplicate of a row the journal still holds is harmless.
- `plan_poll_ballots_rows(poll_ref, rows)`: drops rows that are not ballot rows or are too long, sorts and de-duplicates, packs into envelopes within the bound.

## Wire layout

`lakko/poll-ballots/v1\0 || 0x01 || poll_ref(i64 BE) || count(u16 BE, >= 1) || (len(u16 BE) || row)*`; each row 1..=4096 bytes, starts with `lakko/poll-ballot/v1\0`, rows strictly ascending; the envelope at most `POLL_BALLOTS_ENVELOPE_MAX_LEN` (the open-items bound).

## Failure modes

`PollBallotsError` with a frozen `code()`: wrong domain, unknown version (checked before any length), bad length, too long, empty, row, row order. No input panics.

## What this module does NOT do

It does not open, verify or count a ballot (`lakko-ballot`), and does not decide which polls are open.

## Attacker exposure

An MLS application message any member can craft. Fuzz target `lakko-contacts/poll_ballots_decode`.

## References

ADR-0090 section 3.9.
