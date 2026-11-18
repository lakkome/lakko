<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-ballot/src/ballot.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if ballot.rs changes and this file does not. -->

# `ballot` — the anonymous ballot, its anonymity set and its journal row (spec)

**Module:** `crates/lakko-ballot/src/ballot.rs`
**One line:** Cast a ballot as a sealed group-journal row that hides its voter among at most 256 members, and open and verify one.

## Why this module exists

ADR-0092 section 3.6: an anonymous ballot is not an MLS message (that would name the sender's leaf). It is a row on the group's journal, posted with the epoch access key every member holds, sealed under a key from the poll's secret, carrying a proof that its voter is in the poll's ring and a tag that makes one account one vote. Since 2026-11-15 (maintainer, after proving over the whole ring was rejected for its cost) the proof covers an anonymity set of at most 256 keys, so cost does not grow with the group.

## Wire layout

```text
row      = "lakko/poll-ballot/v1\0" || 0x01 || nonce(24)
           || XChaCha20-Poly1305(seal_key, aad, pad(ballot))
aad      = "lakko/poll-ballot/v1\0" || 0x01
seal_key = HKDF-SHA256(ikm = poll secret, salt = empty, info = "lakko/poll/ballot-seal/v1" || id)
pad(b)   = len(b) u16 BE || b || zero bytes to a multiple of 256
ballot   = id(16) || ring_hash_k(32) || set || choices || tag(32) || proof
set      = count(u16 BE) || index(u32 BE)*      strictly ascending, each < L_k,
                                               count == min(256, L_k)
choices  = count(u8, 0..=10) || index(u8)*     strictly ascending, each < 10
```

The longest ballot is 2,110 bytes (`MAX_BALLOT_LEN`), the longest row 2,366 (`MAX_ROW_LEN`). The set is plain u32 indices (1,026 bytes at 256), chosen over a delta code for a simpler canonical form.

The tag is `x^-1 U`, `U = hash_to_point("lakko/poll/tag-base/v1", len(group_id) u8 || group_id || id)`. The proof context is `"lakko/poll/ballot-context/v1\0" || len(group_id) u8 || group_id || id || ring_hash_k || set || choices`; the proof's own transcript also hashes the selected keys.

## The anonymity set

- A ring version of at most 256 keys: the set is the whole version, as before the rework.
- A larger version: the voter's own position and 255 others, drawn by `draw_set` from ring version `v`: a BLAKE3 XOF keyed by `BLAKE3-derive-key("lakko/poll/anonymity-set/v1", x || len(group_id) || group_id || id || v u32 BE)`, `x` the voter's secret scalar; 64-bit words, rejected at or above the largest multiple of `L_v` below 2^64 so each index is uniform, then reduced mod `L_v`; an index already chosen, or whose key is not a valid point, is skipped; sorted. The seed is a one-way derivation of the secret; the scalar bytes and the seed are zeroized.
- **Which version** (amended 2026-11-15). The account's first vote in the poll draws from the newest version the device holds (`set_version = None`); `cast` returns that version (`Cast::set_version`) and the client records it in the account's own poll-choice register (`lakko_sync::synced::PollChoiceEntry::set_version`), which every device of the account receives. Every later vote passes the recorded version, so it draws the identical set whatever the ring has grown to; it never moves to a newer version. `own_set_version` is a fallback the client uses when the register has not arrived yet but the account's own earlier ballot is readable (found by its tag): it returns that ballot's version. It is kept because it narrows the race below at no cost (the client verifies every ballot for the tally anyway).
- Every member names the newest version at its first vote, so a ballot's version no longer says when its voter joined; it says only that the voter was in the ring at that version.
- The verifier cannot recompute the draw (it needs the secret), so it accepts **any** set that fits the named version: `min(256, L_k)` strictly ascending indices below `L_k`, every selected key a valid point. That cannot give anyone a second vote: the tag depends on the key, the group and the poll only, so every ballot of one key in one poll carries the same tag whatever set it proves over, and the tally counts the last one per tag (test `one_key_with_two_different_sets_has_one_tag_and_counts_once`). A voter who picks its own set only changes how well it hides itself.

## Public contract

- `PollKeys::new(group_id, poll_id, secret)` (group id 1..=255 bytes), `poll_id()`, `tag_of(voter)`.
- `draw_set(keys, ring, voter, set_version: Option<u32>) -> Result<(usize, AnonymitySet)>`: the version and the set, as above (`None` = newest). `UnknownRingVersion` if the device does not hold that version yet; `NotInRing` if the voter's key is not in it; `RingVersionUnusable` if the draw cannot fill the set (more than 2^20 attempts, which only a version mostly made of invalid keys reaches).
- `cast(keys, ring, voter, choices, set_version: Option<u32>) -> Result<Cast { row, set_version }>`: draw, then prove over the set; the returned version is what the client records.
- `own_set_version(keys, voter, verified: &[VerifiedBallot]) -> Option<u32>`: the version of the voter's latest own ballot among those verified. Errors `NotInRing`, `RingVersionUnusable` (a selected key not a point; for a version of at most 256 keys every key is selected), `WitnessMismatch`, `InvalidSet`.
- `cast_with_set(...)` (doc-hidden): the same over a caller-chosen version and set; for tests.
- `AnonymitySet::new(indices)` (1..=256, strictly ascending), `indices()`, `encode()`, `check(version_len)`.
- `open_row(keys, row)`: domain, length bounds and version byte before any decryption, then the AEAD, the padding (all zero, shortest to a 256-byte boundary), the ballot layout and the poll id. Does not verify.
- `verify_ballot(keys, ring, ballot)`: finds the version by hash (`UnknownRingVersion`: keep and retry after the next commit), checks the set fits (`InvalidSet`), decodes the selected keys (`RingVersionUnusable` if one is not a point), decodes the tag as a non-identity subgroup point, verifies the proof. `open_and_verify` does both. `is_ballot_row` is the dispatch test.
- `Choices`, `Ballot { poll_id, ring_hash, set, choices, tag, proof }`, `VerifiedBallot { tag, choices, version }`.

## Invariants upheld

- A verified ballot is bound to one group, one poll, one ring version, one anonymity set and one set of choices: any change makes the proof fail.
- One account has one tag per poll whatever version or set it proves over; two polls give tags with no visible relation.
- Cost of casting and verifying is bounded by the 256-key set, not the group.

## What a ballot reveals (also ADR-0092 section 3.8)

- In a version over 256 keys, the voter is hidden among the 256 members of its set, not among the whole group.
- Every member's own set contains its own key; members who do not vote appear only in others' sets. Counting how often a member's key appears across the poll's ballots therefore says a little about whether that member voted (a voter appears at least once, its own ballot), never which ballot is theirs.
- **Two sets for one account, in a race.** A device that votes before both the register and the account's earlier ballot reach it draws at its own newest version. If the ring has not grown since the first vote, that is the same version and the same set. If it has, the second ballot proves over another set. It still carries the same tag, so the tally counts the account once (the last ballot wins), and the last register write decides which version later votes reuse. The cost is anonymity: anyone holding both ballots sees one tag over two sets, so the voter is in their intersection. For two sets drawn independently from versions of about `N` keys, the expected intersection is the voter plus about `255 * 255 / N` others, a handful for `N` in the thousands (arithmetic, not a measurement).

## Failure modes

`Malformed`, `UnknownVersion`, `RowDidNotOpen`, `WrongPoll`, `InvalidChoices`, `InvalidSet`, `UnknownRingVersion`, `RingVersionUnusable`, `NotInRing`, `WitnessMismatch`, `ProofRejected`, `InvalidGroupId`, `Kdf`. No panic on any input.

## What this module does NOT do

- It does not delay or post the row (the 0 to 10 s queue is the client's) and does not record the voter's own choice (`lakko-sync` own poll-choice register).
- It does not check the choices against the poll's option count or single/multiple rule; the tally does.
- It does not enforce the section 3.4a session-landing rule; the client must post no ballot before its session landing.

## Deviations and choices made here

- The tag base input carries a one-byte length before the group id (variable length).
- The seal key does not depend on the group id (the ADR's formula); the proof context does.
- Padding block 256 and the u16 length prefix are this module's choice.

## Attacker exposure

Every row on a group journal can be written by any member or the Delivery Service. The row length is bounded before decryption; the plaintext, set and ballot parsers are bounded and exact. Fuzz target `lakko-ballot/ballot_open` (row path and direct plaintext path, then verification against a ring of four hash-to-point keys nobody holds, half the time with the one set that fits it, asserting nothing verifies). Compile-checked; not run in this session.

## References

- ADR-0092 sections 3.3 to 3.8, 10.
- Tests in `tests/properties.rs`: `completeness_for_every_position_of_small_rings`, `in_a_large_ring_the_set_is_256_keys_drawn_deterministically`, `a_first_vote_draws_from_the_newest_version_and_a_revote_reuses_the_recorded_one`, `one_key_with_two_different_sets_has_one_tag_and_counts_once`, `a_ballot_verifies_only_for_its_version_set_poll_group_and_choices`, `ill_fitting_sets_and_sets_over_bad_keys_are_refused`, `the_draw_skips_keys_that_are_not_points`, `a_non_member_cannot_produce_a_verifying_ballot`, `hostile_bytes_never_panic`.
