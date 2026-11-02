<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/reaction.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if reaction.rs changes and this file does not. -->

# `reaction` — reaction labels under a channel post (spec)

**Module:** `crates/lakko-channels/src/reaction.rs`
**One line:** Turn an emoji under a post into a 16-byte label the channel service counts without learning the emoji, and turn the service's counts back into emoji.

## Why this module exists

ADR-0087 section 13a stores reactions at the channel service as `(post, post_key, label)` rows and gives readers `{label: count}`. The label must hide the emoji from the service and must not be comparable between posts, yet every reader holding the epoch seed must resolve it.

## Public contract

- `ReactionKey::derive(epoch_seed, channel_id, epoch)` — HKDF-SHA-256, IKM the seed of the post's epoch, salt `channel_id ‖ epoch_be`, info `lakko/channel/reaction-key/v1`. Zeroed on drop; `Debug` prints nothing of it.
- `ReactionKey::label(post_cursor, emoji)` — `BLAKE3-keyed(reaction_key, post_cursor_be ‖ emoji)[..16]`; refuses an emoji outside `lakko_ui_rules::reaction::CHANNEL_REACTION_EMOJI` (exact byte match) with `ChannelError::UnknownReactionEmoji`, since nobody could resolve its label.
- `ReactionKey::labels_for_post(post_cursor)` → `ReactionLabels`: the 64 labels in list order.
- `ReactionLabels::resolve(label)` → the emoji or `None`; `resolve_counts(&[(label, count)])` → `(emoji, count)` in list order, unknown labels dropped, repeats summed saturating, zeros left out.

## Invariants upheld

- One emoji has a different label under every post, epoch seed and channel (test `labels_differ_between_posts_seeds_and_channels`).
- Every listed emoji resolves to itself and the 64 labels of one post are distinct (`every_label_resolves_to_its_emoji_and_nothing_else_does`).
- A label written by someone without the seed resolves to nothing, so it shows nothing.
- Invariant #2: the service holds labels and counts, no emoji.

## What this module does NOT do

It does not hide how a post's rows split across labels (ADR-0087 section 13a declines that), and it holds no counts or rows.

## References

ADR-0087 section 13a; `lakko_ui_rules::reaction`; tests `the_label_is_the_documented_derivation`, `an_emoji_outside_the_list_has_no_label`, `counts_resolve_in_list_order_and_junk_is_dropped`.
