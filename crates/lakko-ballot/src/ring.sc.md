<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-ballot/src/ring.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if ring.rs changes and this file does not. -->

# `ring` — a poll's electorate and its versions (spec)

**Module:** `crates/lakko-ballot/src/ring.rs`
**One line:** Hold the list of account keys that may vote in one anonymous poll, as versions that are prefixes of one list, so every member derives the same versions from the commits it applies.

## Why this module exists

ADR-0090 section 3.4: the electorate is every account that was a member at any moment while the poll was open. Version 0 is the roster at the poll's epoch; each merged commit that adds new accounts appends a version, with no message sent. Ballots name the version they were proven against by its hash, so every member must compute identical versions and hashes.

## Public contract

- `Ring::new(keys)`: version 0. Refuses (`InvalidRing`) an empty list, one longer than `MAX_RING_LEN` (262,144), or one not strictly ascending bytewise (which also refuses duplicates). Keys are stored as bytes; nothing is decoded here.
- `Ring::extend(added)`: applies one merged commit. Keeps the keys of `added` not already in the ring, sorts them bytewise, removes duplicates and appends them as the next version; returns its index, or `None` when nothing new was added (no version is made). Past `MAX_RING_LEN` keys or `MAX_VERSIONS` (65,535) versions it returns `RingTooLarge` and leaves the ring unchanged, identically on every member.
- `version_count`, `version_len(k)` (`L_k`), `version_hash(k)`, `find_version(hash)`, `contains`, `position`, `keys`.
- `point(index)` (crate-private): decodes one key as a canonical, torsion-free, non-identity point, `RingVersionUnusable` otherwise. Only the keys of an anonymity set are decoded, when it is drawn or verified.
- `ring_hash(keys)` = `BLAKE3-derive-key("lakko/poll/ring/v1", keys concatenated)`, the ADR's `ring_hash_k`. A client checks the poll's signed `ring0_hash` against `version_hash(0)`.
- `encode` / `decode`: `ring_count(u32 BE) || keys || version_count(u16 BE) || lengths(u32 BE)*`, the ring part of the ADR-0090 section 3.9 hand-over item. Decode is strict: exact length, lengths strictly increasing from at least 1 to exactly the key count, version 0 and every later batch strictly ascending, no key twice. Re-encoding a decoded ring gives the same bytes.

## Invariants upheld

- Versions are prefixes: version `k` is the first `L_k` keys; extension only appends.
- A key is never in the ring twice, so an account cannot gain a second index by being added again (and its tag in a poll does not depend on the version anyway).
- A key that is not a valid point is stored like any other, so every member still derives the same versions and hashes; an anonymity set that selects it is refused by the verifier, and the prover's draw skips it (fail-closed, ADR-0090 section 3.6 "Key checks").
- `MAX_RING_LEN` = 262,144, chosen, not measured. Proof cost no longer depends on the ring (a proof covers at most 256 keys); the cap bounds what a member stores per poll (32 bytes a key, 8 MiB at the cap) and the hand-over, which at the cap spans about nine 1 MiB DS rows.
- Hashes are computed incrementally from one running BLAKE3 hasher, so building `v` versions costs one pass over the keys, not `v`.

## What this module does NOT do

- It does not read the roster or MLS commits; the client passes each commit's added account keys in commit order, after resolving leaves through `account_key_at_leaf`.
- It does not know whether the poll is closed; the client stops extending at the close.
- It does not check `ring0_hash` against the poll; the client does.
- It does not apply the ADR-0090 section 3.4a alias rule (Stage B rotation); that is unbuilt and would filter `added` before `extend`.

## Attacker exposure

`decode` reads the hand-over a committer writes (ADR-0090 section 3.9); it bounds the count before allocating. Fuzz target `lakko-ballot/ring_decode` asserts the canonical round trip (compile-checked; not run here). `extend` takes keys from the client's own commit processing.

## References

- ADR-0090 sections 3.4, 3.6, 3.9.
- Tests: `ring::tests::versions_are_prefixes_and_hash_like_the_adr`, `an_invalid_key_is_stored_but_does_not_decode`; `tests/properties.rs` `ring_encoding_round_trips`, `a_ring_extension_never_counts_one_account_twice`.
