<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-transparency/src/reset_seals.rs; keep in lock-step with the source (just spec-check). -->

# `reset_seals` — the seal-announcement leaf of the KT log (spec)

**Module:** `crates/lakko-transparency/src/reset_seals.rs`   **One line:** the
leaf an account's seal announcement is logged as, and the check that a served
announcement sits in the log before the reset it seals (ADR-0101 section 10).

## Why this module exists

ADR-0073 R.2 rule 4 asks whether the announcement a contact was served is the
one the log holds for generation `g-1`, before the reset leaf. Logging the
announcement puts its history under the same witnesses as the identity
bindings (ADR-0039).

## Public contract

- `AnnouncementLeaf::for_announcement(username_hash, key_generation, seq,
  announcement)`: the leaf for the bytes as served, committing them by
  `BLAKE3`.
- `canonical_bytes()`: `"lakko/transparency/reset-seals-leaf/v1\0" ‖
  username_hash(32) ‖ key_generation(8 BE) ‖ seq(8 BE) ‖
  announcement_hash(32)`, always 119 bytes.
- `verify_announcement_before_reset(leaf, proof, reset_leaf_index,
  signed_root)`: `Ok` when the reset leaf's index is inside the tree, the
  announcement's index is below it, and the proof binds the leaf to the
  root.

## Invariants upheld

- An announcement leaf never has an identity leaf's bytes: it opens with an
  ASCII separator, an identity leaf with a 32-byte hash.
- The account, generation, `seq` and announcement bytes are all bound: change
  any one and the proof fails.

## Failure modes

`Error::IndexOutOfBounds` (reset index not in the tree),
`Error::AnnouncementNotBeforeReset`, `Error::InclusionProofMismatch`. No
panic.

## What this module does NOT do

- It does not verify the root: the caller has checked the operator
  signature, no-rollback and the witness quorum, and the reset leaf's own
  inclusion.
- It cannot prove "newest": inclusion shows presence, not the absence of a
  later announcement leaf for the same generation. That rests on the
  operator, kept honest by the witnesses and by each device's `seq` floor.

## References

ADR-0101 section 10. Tests: `an_announcement_before_the_reset_passes`,
`every_failure_is_refused`, `the_leaf_is_pinned_and_differs_from_identity_leaves`.
