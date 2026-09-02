<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0023: Message edit / delete / reactions

- **Status:** Accepted
- **Date:** 2026-06-18
- **Context:** Phase 3 — block B (chat-feature parity)

> **AMENDED 2026-08-31 (`5b2b654`) — the author gate was an impersonation, and
> it is fixed. Recorded here 2026-09-02.** The decision below is unchanged in
> shape: an author-only, peer-verified, client-side, fail-closed gate is still
> exactly right. **What it compares changed.** This ADR specified the gate on
> the **MLS leaf index**, and OpenMLS hands a **vacated leaf to the next member
> added** — so remove Bob, add Dave, and Dave inherits Bob's leaf and may
> rewrite and delete Bob's history. On both natives that was **permanent**,
> because the leaf was persisted beside the message. The gate now keys on the
> **Ed25519 signature public key, resolved at receipt, in the epoch the row
> arrived** — indexed as **C-68**. See the two marked sections below
> (§ Author-only integrity gate, and the reaction item under Open questions,
> whose severity call was wrong).
>
> Two things worth carrying out of it. First, **[ADR-0045](./0045-remote-commit-epoch-fork.md)
> made this cheap rather than possible** — it lets a remove and an add compose
> into one commit — so the hazard predates that ADR and was simply never
> reachable in one step before. Second, the fix is *not* "compare keys" but
> "compare keys resolved at receipt": resolving later, against the current
> roster, reproduces the impersonation under a comparison that looks correct,
> which is why all three clients pin the timing with a permanent contrast test.

## Context

Lakko chat carries a user-visible text message plus optional attachment
references, encoded as the JSON [`Envelope`](../../crates/lakko-attachments/src/lib.rs)
(`{ v: 1, text, attachments }`) inside an MLS Application message. Three
table-stakes messenger features are missing: **edit**, **delete**, and
**reactions**. Block B of the Phase-3 closeout names them.

Two pre-existing artefacts look like they already solve this but do not:

1. **The proto `ApplicationPayload`** (`schemas/mls-app/v1.proto`) defines
   `EditMessage { target_message_id, new_text }`, `DeleteMessage`, `Reaction
   { target_message_id, emoji }`, `TypingIndicator`, `ReadReceipt`, all under a
   `oneof body`. **It is dead code** — a grep across `apps/` + `services/`
   confirms no client or service constructs or dispatches on it. The generated
   Rust type exists (it is fuzzed via `lakko-proto/mls_app_payload`) but nothing
   on the wire uses it. Every client sends the JSON `Envelope` instead.

2. **The `lakko-sync` `EditChain`** (`EditOp::Update` / `EditOp::Delete`, LWW by
   HLC) models the *multi-device merge* of an edit history. It is the Phase-2
   DeviceGroup-sync substrate, not the per-message receive path the chat clients
   run today (which applies the DS journal in order).

So the carrier question is real, and there is a load-bearing **security**
finding underneath it: the FFI `ProcessedMessage` surfaces only `kind` +
`applicationData` — **not the sender leaf**. Today that is harmless (a chat
bubble is just rendered). The moment one message can *reference and mutate*
another, an unauthenticated reference is an integrity hole: a malicious group
member could publish an `Edit`/`Delete` targeting *another member's* message and
rewrite or erase their words. MLS authenticates *that a member sent the op*, but
nothing today checks *which* member — and the zero-trust DS cannot enforce
authorship (invariant #2/#5). This is the same shape as ADR-0022 group roles: a
peer-verified, client-side, fail-closed authorization gate.

## Decision

**Edit/delete/reactions ride the JSON `Envelope` (a forward-compatible v1.x
extension), reference their target by its DS-assigned `messageId`, are ordered
by the DS journal (LWW = highest-messageId op wins), and are gated client-side
by an author-only integrity check that requires threading the Application
sender leaf through the receive path.** The dead proto `ApplicationPayload`
edit/delete/reaction types are left in place but marked non-authoritative.

Implementation specifics:

### Wire format (`lakko-attachments`)

`Envelope` gains four optional, `#[serde(default, skip_serializing_if)]` fields
(unknown to a v1.0 parser, so forward-compat holds without a `v` bump — exactly
the path the existing module doc anticipates):

```rust
pub struct Envelope {
    pub v: u8,
    #[serde(default)] pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub attachments: Vec<AttachmentRef>,
    // v1.1 — at most one of these is set; a plain chat message sets none.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub edit_of: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub delete_of: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub react_to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub emoji: Option<String>,
}
```

A structured `parse_body(bytes) -> MessageBody` is added beside the existing
`parse` (kept for back-compat callers):

```rust
pub enum MessageBody {
    Chat { text: String, attachments: Vec<AttachmentRef> },
    Edit { target: i64, text: String },
    Delete { target: i64 },
    Reaction { target: i64, emoji: String },  // bounded: emoji length-capped, validated UTF-8
}
```

`MAX_EMOJI_BYTES` caps the reaction string (a small constant — a few grapheme
clusters). The parser stays bounded + fail-closed: a malformed or
over-long/ambiguous (more than one op field set) envelope degrades to `Chat`
with the raw text, never panics. The `message_body_parse` fuzz target is
extended to drive the new fields.

### Target reference + ordering

The target is the DS `messageId` (`i64`, per-group monotonic, assigned on
publish, observed identically by every polling client). An edit/delete/reaction
is itself an Application message with its own `messageId`; its body references
the *target's* id. Because the DS journal is a total order, **the
highest-`messageId` edit/delete for a target wins** — LWW with no clock needed
on the single-journal path. The multi-device DeviceGroup merge (where
`lakko-sync` `EditChain` + HLC become load-bearing) is explicitly **out of scope
here** and deferred to Phase 2.

### Author-only integrity gate (the security core)

The FFI Application receive surface is extended to carry the **sender leaf
index** (`lakko-mls` already exposes it via `ProcessedMessage::sender()` — the
same accessor ADR-0022 uses to read a committer). Each client tracks, per
displayed message, the original sender leaf. On receiving an `Edit`/`Delete`,
the client **applies it only if the op's sender leaf equals the target
message's original sender leaf** — otherwise the op is dropped (fail-closed,
invariant #4). A reaction is accepted from any member (reacting to others'
messages is the point) but is attributed to its sender leaf for dedup,
aggregation ("👍 ×3"), and per-user toggle (re-sending the same emoji removes
it).

> **CORRECTED (`5b2b654`, 2026-08-31) — the identity above is wrong and was
> exploitable. C-68.** Read every "sender leaf" in this section as **"author
> signature key"**:
>
> - **The comparison.** Not the leaf index — the sender's **32-byte Ed25519
>   signature public key**. OpenMLS reuses a vacated leaf for the next member
>   added, so a leaf index identifies a *seat*, not a person; a newcomer
>   seated at a departed member's leaf passed this gate as that member. It was
>   permanent on iOS and Android (the leaf was on disk with the message) and
>   session-scoped on web only because the author was not persisted at all —
>   a deferral that narrowed the window, not a defence.
> - **The timing, which is the load-bearing half.** The key is resolved from
>   the MLS-authenticated sender **at receipt, inside the batch loop, in the
>   epoch the row arrived** — one line after the receive call returns. Never in
>   a view model, never hoisted. A batch carrying both the removal and the add
>   would otherwise resolve an epoch-N message against the N+1 roster, which
>   **is** the same impersonation wearing a correct-looking comparison. Both
>   receive paths (the poll and the ADR-0045 sweep's verdict apply) resolve
>   identically.
> - **Reactions had the same shape.** Attribution is on the reactor's
>   signature key, not the leaf — see the corrected Open question below.
> - **Fail-closed at every unknown.** A torn roster read, an unresolvable
>   sender, and a message stored under the old shape (no author key) all refuse
>   — including for the genuine author. Refusing an edit is the safe direction;
>   there is deliberately no *this row is ours, assume our current key*
>   fallback.
>
> Landed as `authorKey` / `authorKeyB64` plus an `AuthorIdentity` resolver on
> all three clients ([web](../../apps/web/src/chat/author-identity.ts),
> [iOS](../../apps/ios/Lakko/Chat/ChatRepository.swift),
> [Android](../../apps/android/app/src/main/kotlin/me/lakko/app/chat/MessageLogStore.kt)).
> Web gained something it never had along the way: an author that survives a
> reload, so edit and delete now work after one.

### UI (all three clients, consistent affordances)

A per-message action menu (web right-click / long-press; iOS context menu;
Android long-press) offers **Edit** + **Delete** on *own* messages and
**React** on any. An edited message renders its latest text with an "edited"
marker; a deleted message renders a "message deleted" tombstone; reactions
render as emoji chips with counts under the bubble. Editing/deleting is offered
only on own messages (the gate is the receive-side enforcement, not just the UI
affordance).

## Alternatives

1. **Adopt the proto `ApplicationPayload` `oneof`.** Rejected: it would migrate
   every client's send + receive path off the JSON envelope the whole system
   uses, for no gain — the envelope already carries text+attachments and the
   forward-compat extension is exactly what its design anticipated. Large blast
   radius, no benefit. (The proto stays as a non-authoritative historical
   artefact; a future cleanup ADR may remove or `reserved`-out the unused
   bodies.)

2. **Reference messages by a client-generated content id (UUID/hash) instead of
   the DS `messageId`.** Rejected: it adds a second identity space that must be
   carried in every envelope and reconciled against the DS id the clients
   already key on, with no advantage on the single-journal path. The DS id is
   already stable, shared, and monotonic.

3. **Route edit/delete through the `lakko-sync` `EditChain` now.** Rejected for
   this phase: `EditChain` solves multi-device *merge*, which needs the
   DeviceGroup message-log sync that is not built. On the single journal the DS
   total order already gives a well-defined LWW. Pulling `EditChain` in now
   would be machinery without its substrate. Deferred to Phase 2 — at which
   point the envelope's `edit_of`/`delete_of` ops feed the chain.

4. **No author-only gate (trust MLS membership).** Rejected: MLS authenticates
   group membership, not per-message authorship. Without the gate, any member
   can rewrite/erase any member's message — an in-group integrity break. The
   gate is cheap once the sender leaf is exposed and is fail-closed.

## Consequences

- **Positive:** table-stakes chat features land on the existing wire with a
  forward-compatible, fuzzed, bounded envelope extension; the server stays
  opaque (it relays ciphertext envelopes it cannot parse — invariant #2);
  edit/delete carry a peer-verified author-only integrity gate (strengthens the
  in-group trust model the way ADR-0022 did for membership); no schema/proto
  migration, no new service, no DS change.
- **Negative:** a new attacker-touchable parser surface (the extended envelope —
  mitigated by the bounded fail-closed codec + extended fuzz target); a small
  FFI surface addition (sender leaf on the Application event) that all three
  clients must thread; the author-only gate is client-enforced (a compromised
  *client* can still rewrite its own view — but cannot forge a peer-accepted
  edit of another's message, since peers run the same gate).
- **Neutral:** reactions/edits/deletes are themselves journal messages, so they
  consume DS message ids and appear in the raw journal; clients fold them into
  the target's render rather than showing them as standalone bubbles. LWW means
  a late-arriving older-id edit never supersedes a newer one (monotone).

## Open questions

- **Multi-device convergence** (Phase 2): when DeviceGroup message-log sync
  lands, the `edit_of`/`delete_of`/`react_to` ops should feed `lakko-sync`
  `EditChain` so a second device reconstructs the same resolved view; the HLC
  tie-break becomes load-bearing there. Deferred.
- **~~Reaction identity across a leaf renumber~~ — RESOLVED, and the severity
  call here was wrong (`5b2b654`, 2026-08-31).** The question was right and
  its own suggested fix was the one adopted: reaction attribution now keys on
  **the reactor's signature pubkey**, not the leaf. What this text got wrong
  was the cost of waiting. It reasoned that *"attribution is per-call/epoch and
  ephemeral (a count), so a renumber at worst mis-dedups a live reaction across
  an epoch boundary — acceptable for v1."* It is not a count and it is not
  ephemeral: a reaction is stored per `(reactor, emoji)`, so a newcomer seated
  at a departed member's leaf **un-reacted on that member's behalf**, and all
  three clients independently measured that this is worse than a mis-toggle —
  removing the last reactor **prunes the set, so the chip disappears
  entirely**. The read-receipt watermark map ([ADR-0024](./0024-typing-read-receipts.md))
  carried the identical defect: never reset on a membership change, it let a
  newcomer **inherit** a departed member's watermark, so the UI reported
  messages as read by someone who never saw them.
  <br>The general lesson, indexed as **C-68**: every module built after this
  ADR keys on the signature pubkey and several name leaf reuse in a comment as
  the reason — C-63's role table, C-64's committer rank and beacon store,
  C-60/C-62's push-routing gate. **This ADR predates that convention and was
  never revisited when it formed.**
- **Typing indicators + read receipts** share this envelope's extension shape
  (the proto reserved them too) but are a separate block-B item; this ADR does
  not build them.

## References

- ADR-0022 (group-admin roles) — the peer-verified, fail-closed, client-side
  authorization-gate precedent this reuses for the author-only edit/delete gate.
- ADR-0005 §13 — the message-key envelope paradigm `lakko-attachments` follows.
- `crates/lakko-attachments/src/lib.rs` — the `Envelope` + `compose`/`parse`
  this extends; its module doc already names `reply_to`/`edit_of` as the
  forward-compat path.
- `schemas/mls-app/v1.proto` — the dead `ApplicationPayload` edit/delete/react
  definitions this decision supersedes in practice.
- Security invariants #2 (server opacity), #4 (fail-closed), #5 (social graph
  not on the server).
