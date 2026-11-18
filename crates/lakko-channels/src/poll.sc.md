<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/poll.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if poll.rs changes and this file does not. -->

# `poll` — channel poll option labels, the vote message and the frozen tally (spec)

**Module:** `crates/lakko-channels/src/poll.rs`
**One line:** Derive the opaque labels a channel poll's options are counted under, define the bytes a vote's anonymous-credential presentation is bound to, and encode the tally a publisher freezes at close.

## Why this module exists

A channel subscriber is not an MLS member, so a channel poll vote is a row at the channel service, keyed by the per-poll tag of an anonymous credential (`lakko-anon-cred`, ADR-0090 section 4). The service must count votes without learning which option a label is, and a captured vote request must not be replayable as a retract or to restore a vote its voter has changed. Clients and the service build the bound message from one definition here, so they cannot disagree on its bytes.

## Public contract

- `poll_option_label(reaction_key, post_cursor, option_index)` — `BLAKE3-keyed(reaction_key, post_cursor_be ‖ "lakko/poll-option/v1\0" ‖ option_index)[..16]` under the post's `ReactionKey`; index 0..=9, else `OptionOutOfRange`. `poll_option_labels(key, post_cursor, option_count)` gives labels `0..option_count` in option order, count 2..=10.
- `VoteAction::{Put, Delete}` with frozen bytes `0x01` / `0x02`.
- `check_vote_labels(action, labels)` — a put carries 1..=10 strictly ascending labels, a delete none.
- `vote_message(action, at_ms, labels)` — the canonical message (layout below); refuses what `check_vote_labels` refuses.
- `ChannelPollTally { voters, counts }` — `from_counts(option_labels, label_counts, voters)` maps the service's `(label, count)` pairs to option order (unknown labels dropped, repeats summed saturating, each clamped to `voters`); `encode()`; `decode_poll_tally(bytes)` strict and canonical.

## Wire layout

```text
vote message:  "lakko/channel-poll/vote/v1\0" ‖ action(1) ‖ at_ms(8 BE) ‖ label_count(1) ‖ labels(16 each, strictly ascending)
frozen tally:  "lakko/channel-poll/tally/v1\0" ‖ 0x01 ‖ voters(8 BE) ‖ option_count(1: 2..=10) ‖ counts(8 BE each)
```

The poll (channel and post) is not in the message: the presentation's Fiat-Shamir transcript binds it as the `PollScope`. The service accepts `at_ms` only within its window and strictly above the tag's stored value; that rule is the service's (`services/channels` `votes`).

## Invariants upheld

- Option labels are distinct within a poll and differ by post and by reaction key (`option_labels_are_distinct_and_depend_on_post_and_key`); the derivation matches the ADR formula (`option_label_matches_the_adr_formula`).
- One vote message per (action, time, label set): the encoding is length-fixed by its count, so injective (`vote_message_is_pinned_and_canonical`).
- A decoded tally re-encodes to its bytes and every count is at most its voters (`tally_round_trips_and_refuses_bad_input`, fuzz oracle).
- Invariant #2: the service sees opaque labels, never an option or a question.

## Failure modes

`ChannelPollError::{OptionOutOfRange, BadLabelCount, LabelsNotCanonical, MalformedTally, TallyCountAboveVoters}`; no input bytes in an error. No panic on any input (`decode_never_panics_on_short_or_odd_input`).

## What this module does NOT do

- It verifies no presentation (`lakko-anon-cred`) and keeps no ordering state (the service).
- It does not carry the tally in a post: where the frozen tally rides in the edited post's plaintext is the chat envelope's business.
- The tally is the publisher's reading; decoding checks only its shape (ADR-0090 4.6).

## Attacker exposure

The frozen tally is written by a publisher and decoded by every subscriber: fuzz target `channel_poll_tally_decode` asserts no panic, the count bound and canonical re-encoding. The vote message is built, never parsed, by the service from fields it validated.

## References

ADR-0090 sections 4.3, 4.5, 4.6; ADR-0087 section 13a (reaction key).
