<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-open-items/src/build.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if build.rs changes and this file does not. -->

# `build` — a committer's open item (spec)

**Module:** `crates/lakko-open-items/src/build.rs`
**One line:** Turns a poll or checklist the committer holds into the `lakko_contacts::OpenItem` the hand-over carries.

## Public contract

- `poll_open_item(poll_ref, poll, author, ring, votes)`: requires the author signature. An anonymous poll needs its ring and carries every key and version length; a non-anonymous poll needs none and carries, per voter, the latest vote after the poll row whose choices are valid for the poll (ties on a message id broken by the choice bytes); a voter whose latest such vote is a retract is left out. The newcomer's `lakko_attachments::poll::fold` over the carried votes equals the committer's fold over all of them (tested).
- `checklist_open_item(poll_ref, current, author, state, ticks)`: requires the signature on the current body, and refuses a folded `state` whose title, flags or author items are not that body's. Carries the added items the fold kept and one tick per done item, with the message id of the latest tick row that marked it done by that ticker.

## Failure modes

`BuildError` with a frozen `code()`: signature missing, body does not encode, ring given for the wrong kind of poll, state mismatch.

## What this module does NOT do

It does not decide whether the item is open, and does not plan rows (`lakko_contacts::plan_open_items_rows`).

## References

ADR-0092 section 3.9; tests in `src/tests.rs`.
