<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/write_key.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if write_key.rs changes and this file does not. -->

# `write_key` — a subscriber's key for one write, and the write proof (spec)

**Module:** `crates/lakko-channels/src/write_key.rs`
**One line:** Derive the Ed25519 key a subscriber signs one reaction or one comment with, and define the proof the channels service verifies on every such write.

## Why this module exists

A channel subscriber has no MLS leaf, so a reaction or a comment is a row at the channel service. ADR-0087 section 13a: the maintainer refused any key reused across writes (2026-11-01 across posts, 2026-11-02 within one post), since a shared key would hand the service one person's activity in a channel. This module gives every write its own key, the same on every device of the account, and the one signed statement that ties a write to it.

## Public contract

- `reaction_write_seed(account_seed, channel_id, post_cursor, label)` — `BLAKE3-derive-key("lakko/channel/reaction-write-key/v1", account_seed ‖ channel_id ‖ post_cursor_be ‖ label)`. Adding and taking off one reaction use one key; two reactions of one person on one post have unrelated keys.
- `comment_write_seed(account_seed, channel_id, post_cursor, comment_nonce)` — the same under `lakko/channel/comment-write-key/v1` with a random 16-byte nonce (`new_comment_nonce`); the author's other devices learn the nonce from the own-comments register in `lakko-sync`. The two domains differ, so a reaction key never equals a comment key.
- `comment_reaction_write_seed(account_seed, channel_id, post_cursor, comment_cursor, label)` — the same under `lakko/channel/comment-reaction-write-key/v1` (2026-11-05, UX 13.1: a reaction on a comment), the label a `CommentReactionKey` label; a third domain, so it is never a post-reaction or comment key.
- `ReactionWriteKey::new` signs only that reaction: `sign_add`, `sign_delete`. `CommentWriteKey::new` signs only that comment: `sign_add(sealed)`, `sign_delete(comment_cursor)`. `CommentReactionWriteKey::new` signs only that reaction on that comment: `sign_add`, `sign_delete`. `public_key()` is what the row names; `Debug` prints the public key and the post, never a seed. Clock and nonce are the caller's.
- `SubscriberWrite` — `AddReaction{label}`, `DeleteReaction{label}`, `AddComment{sealed}`, `DeleteComment{comment_cursor}`, `AddCommentReaction{comment_cursor, label}`, `DeleteCommentReaction{comment_cursor, label}`, op bytes `0x01..=0x06` (frozen).
- `write_proof_canonical_bytes`, `verify_write_proof(proof, statement, now_ms)` — pure: window first, then a valid Ed25519 key of full order, then the signature.
- `WriteProof` / `WriteProofError` are the owner proof's types (`v1.<unix_ms>.<nonce>.<sig>`), carried in `x-lakko-channel-write-proof`.

## Wire layout

```text
"lakko/channel/write-proof/v1" ‖ 0x00 ‖ op(1) ‖ channel_id(16) ‖ post_cursor(8 BE) ‖ writer_key(32)
  ‖ subject ‖ unix_ms(8 BE) ‖ nonce(16)
subject: label(16) for ops 1-2, BLAKE3(sealed)(32) for op 3, comment_cursor(8 BE) for op 4, comment_cursor(8 BE) ‖ label(16) for ops 5-6
```

The op fixes the subject length, so the encoding is injective.

## Invariants upheld

- A comment-reaction proof names its comment: another comment cursor, or the same label read as a post reaction, is `BadSignature` (`a_comment_reaction_proof_names_its_comment`).
- A proof covers exactly one write: another op, label, sealed comment, comment cursor, post, channel or key is `BadSignature` (`a_proof_covers_exactly_one_write`).
- No two writes of one account share a key (`every_write_has_its_own_key_and_every_device_the_same_one`).
- Invariants #2 / #5: the service sees one public key per write, never an account key.

## Failure modes

`OutsideWindow` (`WRITE_PROOF_WINDOW_MS`, 120 s), `BadSignature`; header parse errors are the owner proof's `Malformed` / `UnknownVersion`. No panic.

## What this module does NOT do

- No nonce set: `services/channels` refuses a repeated nonce (`channel_write_nonces`).
- No tokens: the service spends one `channel-react` / `channel-comment` token on an add, after the proof and the nonce.
- It cannot stop a modified client from deriving other keys and adding up to its token quota (accepted by the ADR), nor enforce the three-reactions-per-post rule (a client rule, `lakko-sync`).

## Attacker exposure

The header reaches the service from anyone. Fuzz target `write_proof_parse` parses it and verifies it for a fixed write, asserting canonical re-encoding.

## References

ADR-0087 section 13a; ADR-0053 (window); tests `the_seeds_are_pinned_and_depend_on_every_input`, `every_write_round_trips_through_the_header_and_verifies`, `the_window_is_checked_first_and_a_weak_key_is_refused`, `the_canonical_bytes_are_pinned`, `a_comment_reaction_proof_names_its_comment`.
