<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-open-items/src/adopt.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if adopt.rs changes and this file does not. -->

# `adopt` — a newcomer's check of an open item (spec)

**Module:** `crates/lakko-open-items/src/adopt.rs`
**One line:** Checks an assembled hand-over item for one group and returns the records the newcomer stores.

## Why this module exists

The hand-over reaches a newcomer through another member. ADR-0090 section 3.9 lets that member speak for what the newcomer cannot check (ring extensions, earlier votes, added items, ticks) but not for the item itself: the newcomer must be able to refuse a poll or checklist invented or altered in someone else's name, and a version 0 of the ring that is not the one the author signed.

## Public contract

`adopt_open_item(group_id, item)`:

- decodes the body strictly as the item's kind and verifies the author signature for `group_id` under the stated author key;
- anonymous poll: the body must be anonymous, the ring must decode (`lakko_ballot::Ring::decode`) and its version-0 hash must equal the signed `ring0_hash`;
- non-anonymous poll: the body must not be anonymous and every vote must have valid, non-empty choices for the poll;
- checklist: every tick of an author item must name an item of the body; added items and ticks are returned as `privileged` records, because the committer's fold already applied the authority rule under flags this device never saw, and judging them again against the current flags would make the newcomer's checklist disagree with everyone else's (tested with an edit that turns "others can mark" off).

Any failure refuses the whole item.

## Failure modes

`AdoptError` with a frozen `code()`: body, signature, kind mismatch, ring, vote, tick.

## What this module does NOT do

It does not decide whose hand-over to take (`lakko_contacts::open_items_sender_trusted`) or join parts (`assemble_open_items`), and it cannot check ring extensions or earlier votes (accepted in the ADR).

## References

ADR-0090 section 3.9; tests in `src/tests.rs`.
