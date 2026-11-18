<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-sync/src/synced/poll_choice.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if poll_choice.rs changes and this file does not. -->

# `poll_choice` — the account's own poll choices (spec)

**Module:** `crates/lakko-sync/src/synced/poll_choice.rs` (fold, prune and envelope helpers in `state.rs`)
**One line:** Keep, on every device of the account, which options the account chose in each poll, because no ballot or channel vote names the voter.

## Why this module exists

ADR-0090 section 3.6 ("Own choice"): an anonymous group ballot is proven by a shared key and a tag, and a channel vote is anonymous at the channel service, so the voter's other devices cannot learn the voter's own choice from what they read. The account's devices keep it among themselves in this register. It never reaches a tally.

## The register

- Key: `(conversation, poll message id)`. The conversation is the `ConversationRef` every synced register uses (a group by MLS group id, a channel by channel id). The poll message id is an `i64`: the DS `messageId` of the poll row in a group, the post cursor in a channel. One register serves group and channel polls (maintainer, 2026-11-15).
- Value: the chosen option indices, strictly ascending, at most 10, each below 10, and `set_version` (u32). An empty list is a retracted vote and is kept, so a device offline across the retraction cannot bring the vote back with an older stamp.
- `set_version` is the ring version the account's anonymity set in this poll was drawn from at its first vote (`lakko_ballot::draw_set`, ADR-0090 section 3.6 as amended 2026-11-15); 0 for a channel poll. Every later vote, from any device of the account, passes it to `lakko_ballot::cast` so it draws the same set; only a first vote, with no register held, draws from the newest version.
- Folded by the ordinary rule of `state.rs`: the greater HLC stamp wins, on an exact tie the larger canonical bytes. Commutative, associative, idempotent (the state property tests include this register in `arb_entry`).
- Canonical persistence tag 20; the persisted state version is 14 (13, which had no set version, and older refused; no back-compat: nothing is deployed).

## Retention

Kept until the conversation ends: `SyncedState::prune_poll_choices()` drops every register whose conversation's end register reads ended (ADR-0087 section 9); a conversation joined again keeps its registers. The prune is a separate call, not part of the fold (as `prune_channel_activity` is): a register a late envelope brings back for an ended conversation shows nothing and goes at the next prune. `POLL_CHOICES_MAX_CLIENT` (10,000, chosen, not measured) bounds new registers at the writer (`BoundsError::TooManyPollChoices`); an update of a held register is always written; the fold never refuses for it.

## Public contract

- `SyncedState::set_poll_choice(hlc, conversation, poll_message_id, choices, set_version) -> Result<Option<Vec<u8>>, BoundsError>`: validates, enforces the cap for a new register, folds; returns the one-entry envelope to send when the register changed, `None` when the held one is newer or identical. Errors `RefIdLength`, `PollChoices`, `TooManyPollChoices`.
- `SyncedState::poll_choice(conversation, poll_message_id) -> Option<Stamped<PollChoiceEntry>>`.
- `SyncedState::poll_choice_envelopes() -> Vec<Vec<u8>>`: every held register with its original stamp, in envelopes of at most 32 entries: the catch-up on DeviceGroup growth and the re-send of a failed live send.
- `SyncedState::apply_poll_choice_envelope(bytes) -> Result<bool, PollChoiceEnvelopeError>`: whole envelope or nothing; returns whether anything changed.
- `SyncedState::prune_poll_choices() -> usize`.
- `compose_poll_choice_envelope`, `decode_poll_choice_envelope`, `is_poll_choice_envelope`, `PollChoiceEntry { conversation, poll_message_id, choices, set_version }`, `PollChoiceEnvelopeError` (codes `wrongDomain`, `unknownVersion`, `count`, `truncated`, `notCanonical`, `trailing`).

## Wire layout

```text
domain(26) "lakko/sync/poll-choice/v1\0" || version(1) = 0x02 || count(1) 1..=32
count x { hlc(8 BE) || ref_kind(1) || ref_id_len(1) 1..=64 || ref_id
          || poll_message_id(8 BE, i64) || choice_count(1) 0..=10 || choice(1)*
          || set_version(4 BE) }
```

Version `0x01` (no set version) is refused. Entries strictly increasing by `(ref kind, ref id, poll message id)`; nothing trails; canonical.

## Carriage

A DeviceGroup service envelope of its own, like the own channel reactions. Not in the growth snapshot (whose codec lives in `lakko-mls`); persisted by `to_bytes`, so the backup carries it.

## Attacker exposure

Bytes from another device of the account or from a backup. A hostile device of the account can only make the others show a choice the account did not make. Bounded, total decoder; driven by the `lakko-sync/synced_state_decode` fuzz oracle (`poll_choice_envelope_holds`: canonical round trip, fold, carry-back, no double apply, prune after the conversations end), compile-checked, not run here.

## References

ADR-0090 section 3.6; ADR-0087 section 9. Tests: `state::tests::a_poll_choice_round_trips_retracts_and_goes_with_its_ended_conversation`, `the_poll_choice_cap_bounds_new_registers_only`, `poll_choice::tests::*`, `fuzz_api_tests::the_synced_decode_oracle_holds_on_poll_choices`.
