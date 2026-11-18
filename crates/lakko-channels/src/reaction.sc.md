<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/reaction.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if reaction.rs changes and this file does not. -->

# `reaction` — reaction labels under a channel post (spec)

**Module:** `crates/lakko-channels/src/reaction.rs`
**One line:** Turn an emoji under a post into a 16-byte label the channel service counts without learning the emoji, and turn the service's counts back into the emoji a reader knows.

## Why this module exists

ADR-0087 section 13a stores reactions at the channel service as `(post, writer_key, label)` rows and gives readers `{label: count}`. The label must hide the emoji from the service and must not be comparable between posts, while every reader holding the epoch seed resolves it. A channel post takes any reaction a chat message takes (maintainer, 2026-11-02).

## Public contract

- `ReactionKey::derive(epoch_seed, channel_id, epoch)` — HKDF-SHA-256, IKM the seed of the post's epoch, salt `channel_id ‖ epoch_be`, info `lakko/channel/reaction-key/v1`. Zeroed on drop; `Debug` prints nothing of it.
- `ReactionKey::label(post_cursor, emoji)` — `BLAKE3-keyed(reaction_key, post_cursor_be ‖ emoji)[..16]`; the emoji must pass the chat rule `lakko_attachments::is_reaction_emoji` (the one definition, not a second check), else `ChannelError::InvalidReactionEmoji`.
- `ReactionKey::labels_for_post(post_cursor, candidates)` → `ReactionLabels`: the labels of a candidate list the caller supplies (the client's bundled emoji set plus its recent emoji); a candidate failing the chat rule or repeating an earlier one is skipped, the order kept.
- `CommentReactionKey::derive(epoch_seed, channel_id, epoch)` (2026-11-05, UX 13.1: a reaction on a comment) — the same HKDF with info `lakko/channel/comment-reaction-key/v1`, so its labels are unrelated to post labels. `label(post_cursor, comment_cursor, emoji)` = `BLAKE3-keyed(key, post_cursor_be ‖ comment_cursor_be ‖ emoji)[..16]`, the chat rule applied; `labels_for_comment(post_cursor, comment_cursor, candidates)` → `ReactionLabels` by the same keep-and-skip rule.
- `ReactionKey::keyed_label(input)` (crate-private, 2026-11-15) — `BLAKE3-keyed(reaction_key, input)[..16]`, the one keyed hash behind every post label; also used by `crate::poll` for the ADR-0090 channel poll option labels (`post_cursor_be ‖ "lakko/poll-option/v1\0" ‖ option_index`). The reaction rule is a byte length only, so a crafted "emoji" may equal an option label's input; votes and reactions are counted in separate service tables and resolved against separate candidate lists, so that buys nothing.
- `ReactionLabels::resolve(label)` → the emoji or `None`; `resolve_counts(&[(label, count)])` → `(emoji, count)` in candidate order, unknown labels dropped, repeats summed saturating, zeros left out.

## Invariants upheld

- One emoji has a different label under every post, epoch seed and channel (`labels_differ_between_posts_seeds_and_channels`), and on every comment, never equal to a post's (`a_comment_label_is_the_documented_derivation_and_never_a_post_label`).
- A label resolves only against a list that holds its emoji; a label from someone without the seed, or an emoji the client does not know, shows nothing (`a_label_resolves_only_against_a_candidate_list_that_holds_its_emoji`).
- Invariant #2: the service holds labels and counts, no emoji.

## What this module does NOT do

It does not hide how a post's rows split across labels (ADR-0087 declines that). It does not enforce a channel's allowed reactions (`crate::meta::AllowedReactions`, advisory, readers hide) or the three-per-person rule (`lakko-sync`); the service can check neither. It holds no counts or rows.

## References

ADR-0087 section 13a; ADR-0047 (any emoji in a chat); tests `the_label_is_the_documented_derivation`, `the_chat_rule_decides_what_has_a_label`, `counts_resolve_in_candidate_order_and_junk_is_dropped`.
