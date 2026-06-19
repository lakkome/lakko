<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0024: Typing indicators + read receipts

- **Status:** Accepted
- **Date:** 2026-06-19
- **Context:** Phase 3 — block B (chat-feature parity)

## Context

Two more table-stakes chat signals are missing: a **typing indicator** ("Alice
is typing…") and a **read receipt** ("Alice has read up to here"). Block B names
them. Unlike edit/delete/reactions (ADR-0023), which mutate persistent messages,
these are *ephemeral status* signals — and that exposes a transport question.

The reconnaissance is unambiguous:

- **The DS journal is the only client-to-client transport for chat.** It is an
  append-only FIFO, persisted with a 30-day TTL + hourly pruner
  (`services/ds/src/journal.rs`). There is no transient / ephemeral channel and
  no per-message TTL.
- **`lakko-presence` exists but is unwired and orthogonal.** It derives blinded,
  per-group, 60-second-rotating presence *tokens* for online/away status
  (`crates/lakko-presence/`); it carries no typing/read-receipt capability, no
  client uses it, and sourcing its `presence_seed` from the MLS exporter is
  Phase-1.11 future work.
- **Receive latency is push-driven.** All three clients wake on the post-publish
  push (`notifyGroup`), so a journal-carried signal lands sub-second when push
  is up; the long-poll fallback is 30 s (iOS/Android) / immediate-return (web).
- **The proto `TypingIndicator` / `ReadReceipt` are dead** (defined in
  `schemas/mls-app/v1.proto`, never wired — same as the edit/delete/react ones
  ADR-0023 superseded in practice).
- **The ADR-0023 envelope pattern just landed:** the JSON `Envelope` carries
  optional `#[serde(default)]` op fields, `parse_body` classifies them, the FFI
  exposes `compose*Envelope` + `parseBodyEnvelope`, and every Application
  message surfaces its MLS-authenticated `senderLeaf`.

The tension: typing is ephemeral + relatively high-frequency, but the only
transport is a permanent log.

## Decision

**Typing indicators and read receipts ride the same JSON `Envelope` as
ADR-0023 (a v1.2 extension), journal-carried, reusing `parse_body` + the
MLS-sender-leaf attribution + the FFI pattern — with client-side throttling +
expiry that keeps typing ephemeral despite the permanent log, and a monotone
broadcast watermark for read receipts.** No new transport, no new service, no
`lakko-presence` dependency.

### Wire format (`lakko-attachments`, v1.2)

```rust
pub struct Envelope {
    // ... v1 + v1.1 (ADR-0023) fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")] pub typing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub read_up_to: Option<i64>,
}
```

`MessageBody` gains `Typing { is_typing: bool }` and `ReadReceipt { up_to: i64 }`;
`parse_body` classifies them (still fail-closed: an envelope setting more than
one op field — including a typing/read field alongside an edit/etc. — degrades
to `Chat`). `compose_typing(is_typing)` + `compose_read_receipt(up_to)` builders.
The `message_body_parse` fuzz target is extended.

### Typing — ephemeral despite a permanent log

- **Throttled send:** while actively typing, the client publishes
  `{ typing: true }` at most once per ~4 s. It does NOT spam a `false`; instead a
  `{ typing: false }` is sent once on blur / on sending a message (best-effort
  clear). Volume is bounded — a 30 s typing burst is ~7 tiny entries, auto-pruned
  by the 30-day TTL.
- **Client-side expiry:** the receiver shows "X is typing…" and auto-clears it
  after ~6 s with no fresh `typing:true` from that leaf (an explicit `false`
  clears immediately). Typing state is NEVER persisted.
- **Re-sync safety:** on a full journal re-download a client would replay old
  `typing:true` rows; it ignores any typing signal whose DS row `created_at` is
  older than the expiry window (the client already sees each row's `created_at`).
  So stale typing never resurrects.

### Read receipts — monotone broadcast watermark

- A read receipt is `{ read_up_to: messageId }` — the highest messageId the
  sender has read in this group. The client publishes it (throttled, only when
  the watermark *advances*) when the user views the conversation.
- Each client tracks a per-`(group, senderLeaf)` `readUpTo` watermark
  (monotone — a lower value never overwrites a higher one, so re-sync replay is
  idempotent). The message author renders read-state on its OWN messages
  (e.g. sent → delivered → read checkmarks) from the other members' watermarks.
- **Attribution** is the MLS `senderLeaf` (already surfaced by ADR-0023); a
  member can only assert its own read state.

### Privacy + opacity

The server relays opaque encrypted envelopes it cannot parse (invariant #2); a
typing/read signal is in-group only, never a server-visible artefact (invariant
#5). Read receipts broadcast read-state to the group — a deliberate disclosure;
a per-user "send read receipts" opt-out is noted as future work.

## Alternatives

1. **Wire `lakko-presence` for typing.** Rejected for this phase: it is
   unwired (needs the MLS-exporter `presence_seed` + a network-propagation
   layer that is Phase-1.11 scaffold), it is epoch-tied (rots if the epoch
   rarely advances), and it carries NO read-receipt capability (presence is
   online/away, receipts are a per-message watermark). It remains the right
   long-term home for an ephemeral channel — see Open questions.
2. **Build a dedicated ephemeral (non-journal) transport.** Rejected:
   meaningful new server infrastructure (a non-persisting pub/sub that still
   honours the zero-trust opacity constraint) for a nice-to-have, when the
   journal + push already delivers sub-second latency. Disproportionate scope.
3. **Local-only read receipts (no broadcast).** Rejected for read receipts:
   the whole point is that the *sender* learns the message was read — a purely
   local cursor gives the reader nothing the UI can't already infer and gives
   the sender no "read" signal. (Typing IS effectively local-plus-broadcast;
   read receipts must broadcast to be useful.)
4. **Adopt the dead proto `TypingIndicator`/`ReadReceipt`.** Rejected for the
   same reason ADR-0023 rejected the proto edit/delete: it would migrate the
   send/receive path off the JSON envelope the whole system uses, for no gain.

## Consequences

- **Positive:** zero new architecture — reuses the ADR-0023 envelope +
  `parse_body` + sender-leaf attribution + FFI shape + the client op-routing
  seam; server stays fully opaque; cross-platform-consistent; sub-second latency
  on push.
- **Negative:** typing signals are journal rows (permanent until the 30-day
  prune) — an acknowledged anti-pattern, mitigated by throttling + tiny payload
  + stale-`created_at` ignore on re-sync; read receipts broadcast read-state (a
  privacy disclosure, opt-out deferred); a full offline re-sync re-downloads old
  typing/read rows (ignored on apply, but still bytes).
- **Neutral:** typing/read-receipt messages consume DS messageIds + appear in
  the raw journal between chat messages; clients fold them (typing → ephemeral
  UI; read receipt → watermark) without rendering a bubble, exactly like the
  ADR-0023 ops.

## Open questions

- **A real ephemeral transport** (the long-term home): wire `lakko-presence`
  (online/away) AND add a non-persisting typing channel, so typing leaves the
  permanent log entirely. Deferred — needs the Phase-1.11 presence-seed plumbing.
- **Per-user read-receipt privacy opt-out** (the Signal/Telegram setting).
  Deferred; v1 ships read receipts on.
- **Multi-device read-watermark convergence** lands with the Phase-2
  DeviceGroup message-log sync (a second device should inherit the same
  read state).

## References

- ADR-0023 (message edit/delete/reactions) — the envelope-extension +
  sender-leaf-attribution + `parse_body` + FFI pattern this reuses.
- `crates/lakko-presence/` — the blinded presence-token infra (the deferred
  ephemeral-channel home).
- `services/ds/src/journal.rs` — the append-only FIFO journal + 30-day TTL this
  rides on.
- `schemas/mls-app/v1.proto` — the dead `TypingIndicator`/`ReadReceipt` this
  decision supersedes in practice.
- Security invariants #2 (server opacity), #5 (social graph not materialised).
