<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-contacts/src/open_items.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if open_items.rs changes and this file does not. -->

# `open_items` — open-items hand-over codec, planner and assembler (spec)

**Module:** `crates/lakko-contacts/src/open_items.rs`
**One line:** The wire codec of the ADR-0090 section 3.9 hand-over, the committer's row planner, the newcomer's trust rule and the part assembler; bodies, rings and votes stay bytes.

## Why this module exists

A device admitted to a group reads nothing sealed before its admission, so without help it would never see an open poll or checklist. The committer of any admission that adds a leaf (a new account or a new device of an account already present) posts the open items it holds. This module defines what that message looks like, how a large item (a ring of up to 262,144 keys, a list of votes) is split over several messages, whose message a newcomer takes, and how the parts are joined again. It cannot decode a poll body (`lakko-attachments` depends on this crate) and must not decode a ring (it would pull `lakko-ballot` into the iOS notification extension); the typed check is `lakko-open-items`.

## Public contract

- `compose_open_items` / `decode_open_items` / `try_unwrap_open_items`: a canonical, bounded codec. `compose` refuses exactly what `decode` refuses; whatever decodes re-composes to the same bytes. `try_unwrap` returns `Ok(None)` when the domain does not match, so the dispatcher can fall through; `is_open_items_envelope` is the prefix test alone.
- `plan_open_items_rows(items)`: sorts items by `poll_ref`, refuses two items naming one row, sorts votes by voter, added items by message id and ticks by item, and packs them greedily into envelopes of at most `OPEN_ITEMS_ENVELOPE_MAX_LEN` bytes and `OPEN_ITEMS_MAX_ITEMS` items. A ring or vote list that does not fit is split into parts, each repeating the item header and carrying its offset and the total. Every envelope it returns decodes.
- `open_items_handover_due(added_leaves, held_items)`: true when the commit added a leaf and there is something to hand over.
- `open_items_sender_trusted(sender, admitting, is_owner_or_admin)`: a hand-over is taken from an owner or admin, or from the account that committed this device's admission (the sender of its Welcome); nobody else.
- `assemble_open_items(received, held)`: groups parts per item and sender, joins only parts that agree on header, body, totals and version lengths, ignores an identical repeat, drops a group whose parts overlap with different content, and returns, per item, the first group to become whole, in the order of `received`. Items in `held` are skipped. An incomplete group returns nothing. An anonymous poll's ring comes back in `lakko_ballot::Ring::encode`'s layout.

## Wire layout

See the module documentation. Domain `lakko/open-items/v1\0`, version `0x01`, item count `0..=64`, items strictly ascending by `poll_ref`; per item `poll_ref`, kind (1 anonymous poll part, 2 non-anonymous poll part, 3 checklist), author key, author signature, body (1..=4096 bytes, starting with the poll or checklist domain), then the kind's part. A checklist tick names an author item by `u32` id or an added item by the `i64` message id of its add row, and an added-item tick must name an added item in the same part. Every vote, added item and tick follows the item's own row (message id above `poll_ref`).

## Invariants upheld

- The decoder checks the version before any length and every count against the bytes left before allocating.
- Parts from different senders are never joined, so one sender cannot complete or corrupt another sender's item.
- C-68: the caller passes the sender's account key resolved from the MLS sender at receipt.

## Failure modes

Every refusal is an `OpenItemsError` with a frozen `code()`: wrong domain, unknown version, bad length, too long, too many items, item order, unknown kind, body, ring, votes, checklist, op order, duplicate item. No input panics (fuzzed; proptests over random bytes and byte flips).

## What this module does NOT do

It does not check the author signature, decode the body, check a ring against `ring0_hash`, check a vote's choices against the poll or a tick's author item against the body: that is `lakko-open-items::adopt`. It does not decide which items are open (the caller's fold does) and sends nothing.

## Attacker exposure

The envelope is an MLS application message any member (or the DS replaying a corrupt row) can craft. Bounded by the domain, version, item cap, body cap, ring and vote caps, envelope cap. Fuzz target `lakko-contacts/open_items_decode` (decode, re-compose, bounds, dispatch, assembly).

## References

ADR-0090 section 3.9; tests in `open_items/tests.rs` (`a_ring_at_the_cap_splits_and_reassembles`, `a_missing_part_or_a_contradiction_gives_nothing`, `plan_then_assemble_is_identity_for_votes`).
