<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-attachments/src/poll.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if poll.rs changes and this file does not. -->

# `poll` — poll body, author signature and non-anonymous vote fold (spec)

**Module:** `crates/lakko-attachments/src/poll.rs`
**One line:** The canonical binary poll body of ADR-0090 section 3.1, the author signature over it, and the one fold every client runs over non-anonymous votes and closes.

## Why this module exists

A poll is a chat message whose body every member must read identically, and whose tally every client must compute identically, or the three clients disagree on a vote count. The body is binary and canonical so the author signature covers exactly one encoding; the signature lets a poll reach a newcomer through another member (the ADR-0090 section 3.9 hand-over) without that member being able to forge or alter it. The fold lives here, once, so no client re-implements last-writer-wins.

## Public contract

- `PollBody` — id (16 random bytes), question, 2..=10 options, `multiple`, an optional `PollQuiz { correct, explanation }`, an optional positive `close_at_ms`, an optional `PollAnonymous { secret, ring0_hash }`. `validate` / `encode` / `decode`: `encode` refuses exactly what `decode` refuses, and anything `decode` accepts re-encodes to the identical bytes.
- `PollBody::choices_valid(choices)` — empty (a retract), or strictly ascending indices below the option count, exactly one unless `multiple`.
- `choices_shape_valid(choices)` — the structural rule the envelope parser applies before the poll is known: at most 10, each below 10, strictly ascending.
- `SignedPoll { body, author_sig: Option<[u8; 64]> }` — `sign(body, group_id, author_seed)`, `unsigned(body)` (a channel post, where the post signature authenticates the publisher), `verify(group_id, author_key)`.
- `fold(poll, author, poll_message_id, votes, closes) -> PollTally` and `effective_close(author, poll_message_id, closes)`.
- `PollTally { counts, voters, choices_by_voter, closed_by, close_at_ms }`, `is_closed(now_ms)`, `choices_of(voter)`.
- `MAX_POLL_CLOSE_TALLY_BYTES` (118) — the longest frozen channel-poll tally a close op carries; a copy of `lakko_channels::POLL_TALLY_MAX_LEN` (that crate depends on this one), pinned equal by the FFI crates. The tally itself is opaque here.

## Wire layout

`"lakko/poll/v1\0" || 0x01 || id(16) || flags || question_len(u8) || question || option_count(u8) || (len(u8) || option)* || [quiz: correct(u8) || explanation_len(u16 BE) || explanation] || [close_at: i64 BE] || [anonymous: secret(32) || ring0_hash(32)]`. Flags: bit0 multiple, bit1 quiz, bit2 anonymous, bit3 close_at present; every other bit refuses the poll. The author signature is Ed25519 over `"lakko/poll/author-sig/v1\0" || group_id(32) || body`. Text fields are display text: no control character (so no line break) and no format character from the profile table; question 1..=255 bytes, option 1..=100, explanation 0..=600. A quiz never has `multiple`; `correct` is below the option count.

## Invariants upheld

- **C-68**: the fold takes voters, closers and authors as account signature keys the caller resolved at receipt; it never sees a leaf index.
- A poll is one vote per voter account: the latest valid vote in DS `messageId` order replaces earlier ones; an empty vote retracts; an invalid vote is dropped, not treated as a retract.
- A vote counts only after the poll row, before the effective close (the earliest close after the poll row by the author or a closer the caller marked as admin), and with DS `created_at` before `close_at_ms`. Closing is final.
- The fold result does not depend on the order of its inputs (equal message ids are ordered by their choices).
- An anonymous poll counts no named vote (ballots are `lakko-ballot`'s); its close state is still folded.

## Failure modes

`decode` returns `ContentError` (`TooLarge`, `WrongDomain`, `UnknownVersion`, `UnknownFlags`, `Truncated`, `TrailingBytes`, `Text`, `OutOfRange`); it never panics. `verify` returns `SignatureMissing` when the poll carries none and `SignatureInvalid` for a bad signature or a weak (small-order) key.

## What this module does NOT do

It does not refuse an anonymous poll in a one-to-one (the caller knows the conversation kind), does not decide whether a closer is an admin (the caller reads the live role table at receipt), does not compute `ring0_hash`, and does not handle ballots.

## Attacker exposure

The body is base64 in a chat envelope any member writes; bounded by `MAX_POLL_BODY_LEN` before decoding. Driven by the `message_body_parse` fuzz target through `fuzz_api::content_within_bounds`, and by the module's proptests (round trip, canonical re-encode, hostile bytes, truncation, fold order independence).

## References

ADR-0090 sections 3.1, 3.2, 3.5; C-68; tests in `poll.rs` and `tests/attachment_kinds.rs`.
