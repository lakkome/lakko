<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-channels/src/owned.rs; keep in lock-step with the source (just spec-check). -->

# `owned` — the owned-channels list and its locks (spec)

**Module:** `crates/lakko-channels/src/owned.rs`   **One line:** the lock
roots, the lock heads and the bounded list codec through which an account
that reset its keys takes its own channels back (ADR-0101 section 7).

## Why this module exists

A channel's owner key comes from the account seed, which a key reset
replaces. The account keeps, on the storage service, a list of the channels
it owns, sealed under a random list key `R`; each channel's recovery key
derives from `R` (`crate::recovery::ChannelRecoveryKey::from_list_key`). `R`
sits under locks a reset leaves openable. This module defines the lock roots,
what each lock holds, and the list's bytes. The vault seal, names and
transport are `lakko-vault`'s; the server half `S` is `services/auth`'s.

## Public contract

- `password_lock_root(export_key: 64, server_half: 32)`,
  `keys_lock_root(lineage_seed: 32)`, `passkey_lock_root(prf_output: 32,
  server_half: 32)`: `BLAKE3-derive-key("lakko/channels/owned-lock-{password,keys,passkey}/v1",
  concatenated inputs)`, returned as `SecretBytes`; the scratch input buffer is
  a `SecretBytes` too. Each root names one vault through
  `lakko_vault::VaultRoot::from_root` and `VaultRef::OwnedChannels`.
- Lock heads: `encode_list_key_lock` / `decode_list_key_lock` hold `R`
  (exactly 32 bytes; password and passkey locks); `encode_keys_lock` /
  `decode_keys_lock` hold `R ‖ S` (exactly 64 bytes; the keys lock).
- `OwnedChannelsList::encode` writes exactly 65 532 bytes, refusing a list
  that breaks a bound or the canonical order; `decode` reads exactly that and
  refuses everything else; `channel(id)` looks one up.
- `OwnedChannelsList::merge(theirs, deleted)`: the merge after a `409` on the
  list vault. By channel id; the channels in `deleted` dropped; admission
  seeds unioned and the eight highest by `(version, seed)` kept; epoch seeds
  unioned by epoch (this side's seed on the same epoch) and the four highest
  kept; a read key and a handle seed each from either side, this side's
  first; this side's password
  lock root; passkey locks unioned by tag, this side's first, at most eight;
  past 64 channels this side's are kept first. The result is sorted and,
  for two valid lists, encodes.

## Wire layout

`0x02 ‖ password_lock_root(32) ‖ lock_count(0..=8) ‖ lock_count × (tag(16) ‖
root(32)) ‖ count(0..=64) ‖ count × entry ‖ zero padding`, with entry =
`channel_id(16) ‖ read_key_len(0|32) ‖ read_key ‖ handle_seed_len(0|32) ‖
handle_seed ‖ admission_count(0..=8) ‖ admission_count × (version(4 BE) ‖
seed(32)) ‖ epoch_count(0..=4) ‖ epoch_count × (epoch_id(4 BE) ‖ seed(32))`.
An entry is at most 516 bytes; the content at most 33 443 bytes. The total is
always 65 532 bytes, so with the vault's 4-byte length frame every list seals
to the one 64 KiB bucket (asserted against `lakko_vault::seal::sealed_len` in
a test). Version 1 (no handle seed, 32 764 bytes, the 32 KiB bucket) is
refused: the fullest list with handle seeds does not fit 32 KiB.

`handle_seed` is the channel's ADR-0087 handle write seed (`BLAKE3-derive-key(
"lakko/handle/write/v1", journal_root_seed)` of the owning account), absent
for a channel with no `@name`. A device that retakes the channel after a key
reset signs `rekey` with it, moving the name to the key its new root derives.

## Invariants upheld

- One encoding per list: passkey locks strictly ascending by tag, channels by
  id, admission seeds by `(version, seed)` (two seeds may share a version, as
  ADR-0090 allows), epoch seeds by epoch; every padding byte zero.
- Every count is checked against its bound before its items are read.
- `Debug` never prints a root, a key or a seed (a handle seed shows only as
  `has_handle_seed`).

## Failure modes

`OwnedChannelsError`: `badLength`, `unknownVersion`, `tooMany`,
`badReadKey`, `badHandleSeed`, `notCanonical`, `badPadding` (also a read past the content),
each with a frozen `code()`. No path panics.

## What this module does NOT do

- It does not seal, name or send a vault; it does not hold `S` or know the
  storage routes.
- It does not decide when a list is written or which channels exceed 64; the
  client lists the 64 it rotated most recently.
- `merge` does not know about concurrent deletions by the other side beyond
  what that side's list omits.

## Attacker exposure

The list is opened from a vault the storage service returns, so after the
AEAD a hostile input needs a device of the account or a rollback to an older
genuine object. Fuzz target `owned_channels_decode` asserts that whatever
decodes re-encodes to its own bytes; proptests cover arbitrary prefixes and
arbitrary valid lists.

## References

ADR-0101 section 7 (items 2 to 8). Tests:
`contexts_and_sizes_are_pinned`, `the_lock_roots_are_the_documented_derivations_and_differ`,
`the_fullest_list_fits_and_round_trips`, `decode_refuses_every_bad_shape`,
`debug_never_prints_a_seed`, `merge_keeps_the_newest_and_drops_the_deleted`.
