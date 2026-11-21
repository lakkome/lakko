<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-contacts/src/reach_plan.rs; keep in lock-step with the source (just spec-check). -->

# `reach_plan` — which held reach grant writes a Welcome or signs a delegation (spec)

**Module:** `crates/lakko-contacts/src/reach_plan.rs`   **One line:** one rule,
called by every client, that orders the reach grants held for one person, names
the ones a newer version replaced, and says when the unsigned write to an open
mailbox may follow (G-53, maintainer decision 2026-11-21).

## Why this module exists

A device keeps the reach grants other people handed it, filed per person, class
and the group or channel they arrived in. The `group`, `member` and `channel`
keys are account-wide since ADR-0078 section 9, so the filing scope names where a
grant came from, not which key it is. Each client ordered the grants its own way,
none compared versions across scopes, a search-found grant ranked behind stale
ones, and holding any grant (even a dead one) stopped the unsigned write to an
open mailbox. This module replaces the three orderings.

## Public contract

- `plan_reach_writes(held, purpose, ctx)`: `held` is the grants this device keeps
  for ONE person and ONE mailbox (the caller verified every binding against the
  account key it addresses the person by; grants for an older mailbox of a rotated
  account go in a separate call). Each `HeldReach` is class, filing scope (at most
  64 bytes), version and the caller's unique `index`. `ctx` is the target group
  (or none), the scopes this account and the person are both currently members of,
  and the `index` of the grant the search of this add found (or none). Returns:
  - `superseded`: every grant whose version is below the highest of its key
    family, ascending by index. The family of an account-wide class (`contact`,
    `group`, `member`, `channel`, `fof`) is the class; of `link` and `reply` the
    class and scope. The caller deletes these.
  - `candidates`: one grant per family at its highest version, best first.
    `Welcome` order: the found grant, `contact`, each `reply` (by scope bytes),
    `member`, `group`, `channel`, `fof`. `Delegation` order: the found grant,
    `contact`, `group`, `channel`, `fof`. `link` is never a candidate.
- A `group` or `member` family is a candidate only when `shared_scopes` is not
  empty or the found grant is one of its highest-version grants. Otherwise its
  highest version is kept (neither candidate nor superseded).
- Grants of one family and version are one key; the candidate is represented by
  the found grant, else one filed in the target scope, else one in a shared
  scope, else the lowest scope bytes, else the lowest index.
- A found grant leads only when it is at its family's highest version and the
  purpose allows its class; a superseded found grant is superseded like any.
- `open_mailbox_write_allowed(record_open, candidates, refused)`: true exactly
  when the person's record is open and `refused >= candidates`, so holding no
  candidate or having every candidate refused (`401`) both allow it.

## Invariants upheld

- The result depends only on the set of held grants and of shared scopes, never
  on their order (property test `the_plan_does_not_depend_on_input_order`).
- A candidate is never superseded, never of a class the purpose excludes, and
  no two candidates share a family (`supersession_and_collapse_hold`).
- A live found grant is always first (`a_live_found_grant_leads`).
- The unsigned write is never planned as a candidate: the Delivery Service answers
  a proofless write to a closed mailbox as if stored, so it may only follow every
  signed candidate (ADR-0060 section 6 residual).

## Failure modes

`ReachPlanError::{ScopeTooLong, DuplicateIndex, FoundNotHeld}` with frozen codes
`scopeTooLong`, `duplicateIndex`, `foundNotHeld`. No panic on any input.

## What this module does NOT do

It holds no seed, verifies no binding, and decides nothing about delivery: the
caller tries each candidate, moves to the next on a `401`, and asks
`open_mailbox_write_allowed` afterwards. It does not compute shared scopes from
rosters, and it does not order contact requests or replies to a request.

## Attacker exposure

None directly: every input is the device's own bookkeeping. A grant's class and
version arrive inside an E2EE carrier authored by the person it reaches (kept only
from that sender, C-68), so a version that is too high can only come from the
person; it makes the caller delete its lower copies of that key, and the
candidate it then offers is refused with a `401` like any dead key.

## References

ADR-0060 sections 1 and 6, ADR-0050 section 2.5a item 7 (contact, then `member`,
then `group`, then `channel`), ADR-0078 section 9 (account-wide keys); `reach.rs`.
