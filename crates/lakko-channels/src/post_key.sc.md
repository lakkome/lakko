<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/post_key.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if post_key.rs changes and this file does not. -->

# `post_key` — a subscriber's key for one post, and the write proof (spec)

**Module:** `crates/lakko-channels/src/post_key.rs`
**One line:** Derive the per-post Ed25519 key a subscriber signs reactions and comments with, and define the proof the channels service verifies on every such write.

## Why this module exists

A channel subscriber has no MLS leaf, so a reaction or comment is a row at the channel service. ADR-0087 section 13a (maintainer, 2026-11-01) refuses any key reused from post to post, since it would hand the service one person's activity in a channel. This module gives each (account, channel, post) its own key, the same on every device of the account, and the one signed statement that ties a write to it.

## Public contract

- `channel_post_key_seed(account_seed, channel_id, post_cursor)` — `BLAKE3-derive-key("lakko/channel/post-key/v1", account_seed ‖ channel_id ‖ post_cursor_be)`. Deterministic; distinct for every input; never equal to the owner seed of the same channel.
- `ChannelPostKey::from_account_seed` — the signer bound to one post. `public_key()` is what a row names. `sign_at(write, now_ms, nonce)` signs only for the post it was built for; clock and nonce are the caller's. `Debug` prints the public key and cursor, never the seed.
- `SubscriberWrite` — `AddReaction{label}`, `DeleteReaction{label}`, `AddComment{sealed}`, `DeleteComment{comment_cursor}`, op bytes `0x01..=0x04` (frozen).
- `post_key_proof_canonical_bytes(statement, unix_ms, nonce)` — the signed bytes (below).
- `verify_post_key_proof(proof, statement, now_ms)` — pure: window first, then the post key must be a valid Ed25519 key of full order, then the signature.
- `PostKeyProof` / `PostKeyProofError` are the owner proof's types (`v1.<unix_ms>.<nonce>.<sig>` header, `OwnerProof::parse` / `encode`), carried in `x-lakko-channel-post-key-proof`.

## Wire layout

```text
"lakko/channel/post-key-proof/v1" ‖ 0x00 ‖ op(1) ‖ channel_id(16) ‖ post_cursor(8 BE) ‖ post_key(32)
  ‖ subject ‖ unix_ms(8 BE) ‖ nonce(16)
subject: label(16) for ops 1-2, BLAKE3(sealed)(32) for op 3, comment_cursor(8 BE) for op 4
```

The op fixes the subject length, so the encoding is injective. The domain differs from the owner, co-publisher and invite proof domains.

## Invariants upheld

- A proof covers exactly one write: another op, label, sealed comment, comment cursor, post, channel or key is `BadSignature`.
- Two posts' keys are unrelated without the account seed (BLAKE3 derive-key over the cursor).
- Invariants #2 / #5: the service sees a per-post public key, never an account key.

## Failure modes

`OutsideWindow` (`POST_KEY_PROOF_WINDOW_MS`, the ADR-0053 120 s), `BadSignature` (undecodable or small-order key, or a signature that does not verify); header parse errors are the owner proof's `Malformed` / `UnknownVersion`. No panic.

## What this module does NOT do

- It holds no nonce set: refusing a repeated nonce within the window is the service's (state). Without it a captured add could be replayed after its author took the reaction off.
- It does not spend tokens; the service spends one `channel-react` / `channel-comment` token on an add.
- It does not stop one person deriving several keys for one post with a modified client (accepted by the ADR: a counter can be inflated up to the token quota).

## Attacker exposure

The header reaches the service from anyone. Fuzz target `post_key_proof_parse` (`fuzz_api::post_key_proof_parse`) parses it and verifies it for a fixed write, asserting canonical re-encoding.

## References

ADR-0087 section 13a; ADR-0053 (proof window); tests `the_seed_is_pinned_and_depends_on_every_input`, `a_proof_covers_exactly_one_write`, `the_window_is_checked_first_and_a_weak_key_is_refused`, `the_canonical_bytes_are_pinned`.
