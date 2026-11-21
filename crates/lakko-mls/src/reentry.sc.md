<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-mls/src/reentry.rs; keep in lock-step with the source (just spec-check). -->

# `reentry` — the re-entry marker and the replacement rule (spec)

**Module:** `crates/lakko-mls/src/reentry.rs`   **One line:** the marker a device's
re-entry KeyPackage carries, and the one decision every gate takes about an `Add`
that collides with a current leaf and a `Remove` that stands on it (ADR-0091 M1, M2).

## Why this module exists

A device that failed the commit moving its conversation to the next epoch can never
read it again, and its leaf is still in everyone else's roster. ADR-0091 brings it
back by one commit replacing its leaf `L` with a KeyPackage `K` it minted under its
own device identity (same signature key, same ADR-0050 binding). That is the only
case where an `Add` legitimately carries a current member's signature key, and the
only case where the owner's last leaf or a one-to-one person's last leaf may be
removed. Both exceptions are written here once, so the four gates cannot disagree.

## Public contract

- `ReentryMarker { group_id, epoch_left }` with `encode` / `decode`: the body of the
  private-use KeyPackage extension `0xFF04`, exactly 41 bytes
  (`0x01 ‖ group_id(32) ‖ epoch_left(8, BE)`). `decode` checks the version byte
  before the length and refuses anything else; whatever decodes re-encodes to the
  same bytes.
- `marker_of_key_package(kp)`: `None` without the extension, `Some(Err)` when it does
  not decode.
- `Identity::create_reentry_key_package(group_id, epoch_left)`: a last-resort
  KeyPackage under the identity's signature key and credential, carrying the marker,
  whose leaf advertises `0xFF04` beside the ordinary Lakko capabilities (OpenMLS
  refuses a package carrying an extension its leaf does not advertise). Refuses a
  group id that is not 32 bytes.
- `Identity::forget_key_package(bytes)`: deletes the stored private keys of a
  package this identity minted.
- `judge(scope, table, kind, one_to_one_early, sender, proposal, peers)` (crate
  internal): the ordinary rule (`authorise`, and with `one_to_one_early` the early
  one-to-one answer), then:
  - **collision rule:** an `Add` whose signature key is a current leaf's is
    `Standing::ReentryAdd` only when its marker names this group and the scope's
    epoch and its account is that leaf's account; otherwise it is refused with
    `ReentryPairInvalid`;
  - **pair rule:** a `Remove` the ordinary rules refuse with `OwnerUnremovable`,
    `RemoveNotOwnerOrAdmin` or `OneToOneMembershipFixed` is
    `Standing::PairedRemove` when one of `peers` is an `Add` by the same member
    sender that is a valid re-entry for the removed leaf; every other refusal
    stands.
- `reentry_adds_stand(scope, proposals)` (crate internal, commit-wide; security
  review 2026-11-21): every valid re-entry `Add` replacing `L` rides with a
  `Remove(L)` by the same sender, and no `Add` adds a leaf of an account that
  `accounts_removed_by_others` names: an account whose leaf the same proposals
  remove by a `Remove` from another account that is not that remover's own
  re-entry pair (a `Leave` is the account's own act and does not count). Refused
  with `ReentryPairInvalid`. Applied by the peer commit gate and to every
  selection the sweep builds; the sweep's enumeration drops such an `Add` (and a
  re-entry `Add` whose pair cannot be carried) before building, and the exclusion
  search offers a re-entry `Add` and its paired `Remove` as one unit.

## Invariants upheld

- C-93: a behind device is replaced only by a commit adding a KeyPackage it minted
  for that conversation and epoch.
- An `Add` colliding with a current leaf without a valid marker is refused by every
  gate (proposer, receipt, sweep, commit gate), so a replayed directory KeyPackage
  cannot swap a device out.
- `owner_survives` and `one_to_one_holds` are not relaxed: the account keeps a leaf
  through `K`.
- A member being removed by someone else cannot stay by an `Add` of its own (a
  re-entry package of its leaf, or another device of its account) carried in the
  same commit: the commit gate refuses it and the sweep drops the `Add`.
- Invariant #4: every unclassifiable case refuses.

## Failure modes

- `ReentryMarkerError::{BadLength, UnknownVersion}` from `decode`; no panic on any
  input (proptest and fuzz).
- `judge` returns typed refusals only (`ReentryPairInvalid` or the ordinary one).

## What this module does NOT do

- It does not decide whether a request's device is still the account's or whether
  its leaf is writing (`conversation_behind::reentry_admission_verdict`, over client
  facts).
- It does not join: `Group::join_reentry` (`group/reentry_join.rs`) does.
- It does not hold the client's record of minted packages.

## Attacker exposure

Any member can propose an `Add` carrying a KeyPackage it minted, so the marker body
and the `Add`'s signature key reach `judge` from peers. The marker decoder is
bounded and exact; fuzz target `reentry_marker_decode`.

## References

ADR-0091 sections 5 and 6 and "Phase 2 S0-S2, as built"; C-93, C-88, G-58; tests in
`group/reentry_join.rs` (`reentry_tests`, including
`a_member_being_removed_cannot_survive_by_its_own_reentry_add` and
`a_member_being_removed_cannot_survive_by_adding_another_device`) and this module's
own.
