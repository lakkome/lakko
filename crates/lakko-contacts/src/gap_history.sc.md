<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-contacts/src/gap_history.rs; keep in lock-step with the source (just spec-check). -->

# `gap_history` — the missed messages, sealed to the returning device (spec)

**Module:** `crates/lakko-contacts/src/gap_history.rs`   **One line:** the seal, the
rendezvous and the start-time clamp for the history a re-admitter sends a device that
fell behind a conversation (ADR-0091 section 7, security review 2026-11-21).

## Why this module exists

The first build sealed the gap's history under a group exporter every member after
the replacement commit holds, so every current member could open it, and the start of
the gap came unchecked from the request. Here the bundle is sealed to a fresh X25519
gap key the returning device minted for its request (its public half rides in the
`conversation_reentry` envelope, version 2), so only that device opens it, and the
re-admitter clamps the start to what it saw itself.

## Public contract

- `GapKey::generate` / `from_secret` / `secret` / `public`: one key pair per request;
  `Debug` prints the public key only.
- `gap_history_rendezvous(gap_pub)` = `BLAKE3-derive-key(
  "lakko/conversation-reentry/gap-history-rendezvous/v1", gap_pub)`: the DS
  rendezvous the bundle is posted to. Golden: `[0x42; 32]` gives `4060ae14...1019`.
- `seal_gap_history(signer, gap_pub, group_id, bundle)`: `0x02 ‖ eph_x25519_pk(32) ‖
  AEAD(signer_pub(32) ‖ sig(64) ‖ bundle)`, the `envelope` sealed box under this
  module's own labels, AAD `"lakko/conversation-reentry/gap-history/v1\0" ‖ 0x02 ‖
  gap_pub ‖ group_id`; `sig` is Ed25519 by `signer` (the re-admitter's conversation
  leaf signing key) over `"lakko/conversation-reentry/gap-history-sig/v1\0" ‖ 0x02 ‖
  group_id ‖ gap_pub ‖ signer_pub ‖ bundle`. The signer key and the signature are inside
  the box, so the rendezvous shows neither. Refuses an empty bundle or one above
  `GAP_HISTORY_MAX_BUNDLE_LEN` (the 1 MiB DS cap less 145 bytes) as `BadBundle`, and a
  low-order gap key as `BadKey`.
- `open_gap_history(gap_secret, group_id, sealed) -> OpenedGapHistory { signer_leaf_key,
  bundle }`: the version first (`UnknownVersion`; the unsigned version 1 is refused),
  then the length (`BadLength`: at most the overhead, or above 1 MiB), then the box
  (`NotOpened`: another gap key, another conversation, a tampered box), then the
  signature inside it (`BadLength` when the plaintext holds no bundle after the key and
  signature; `BadSignature` for an unusable or weak signer key or a signature that does
  not verify).
- Whether to keep an opened bundle is NOT decided here: the returning device asks
  `lakko_mls::Group::gap_history_signer_accepted(identity, signer_leaf_key)` on the group
  it joined (its own account, the owner or an admin, or in a one-to-one the other
  person; the rule a re-entry Welcome's sender passes), whichever device committed.
- `gap_history_start(claimed_ms, leaf_first_seen_ms, leaf_last_write_ms)`: `None` when
  the re-admitter knows neither when it first saw the leaf nor a message from it;
  otherwise the later of the claim and the floor (first seen, else last write).

## Invariants upheld

- Only the holder of the gap secret opens a gap history; a member holding every group
  secret does not.
- An opened gap history names the leaf that signed it, and the signature cannot be
  moved to another bundle, signer key, gap key or conversation; with the group-side
  rule, a plain member who saw the request cannot put invented messages in it.
- The signer is not visible to a reader of the rendezvous.
- A bundle sealed for one conversation does not open for another.
- The rows sent never start before the re-admitter's own evidence that the leaf was in
  the conversation.

## Failure modes

`GapHistoryError::{BadKey, BadBundle, BadLength, UnknownVersion(u8), NotOpened,
BadSignature}`, each with a frozen `code()`. No panic on any input.

## What this module does NOT do

It does not choose the rows or encode the bundle (the FFI structured variants encode a
prost `HistoryBundle`), and does not decide which signer to trust (`lakko_mls::Group::
gap_history_signer_accepted`). Anyone who saw the request can still post to the
rendezvous; a post signed by a plain member is dropped by that rule.

## Attacker exposure

The rendezvous is writable by anyone who learns the gap public key, so `open_gap_history`
sees attacker-chosen bytes; fuzz target `gap_history_open` (also drives the
signed-plaintext parser directly), a proptest of the same oracle,
`the_signer_is_reported_and_hidden`, `a_signature_cannot_be_moved_or_the_bundle_altered`
and `a_bundle_signed_by_another_member_names_that_member`.

## References

ADR-0091 section 7; `conversation_reentry.rs`; `envelope.rs` (labels); security review
2026-11-21.
