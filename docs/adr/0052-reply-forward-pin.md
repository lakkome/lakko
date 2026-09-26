<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0052: Reply, forward and pin

- **Status:** Accepted (maintainer, 2026-09-26)
- **Date:** 2026-09-25
- **UX spec:** [`docs/ux/message-actions.md`](../ux/message-actions.md)
- **Extends:** [ADR-0023](0023-message-edit-delete-reactions.md) (the chat
  envelope and its author gate), [ADR-0024](0024-typing-read-receipts.md) (the
  same envelope's second wave of optional fields)
- **Touches:** `crates/lakko-attachments` (the envelope),
  `crates/lakko-contacts` (a new group envelope for the shared pin), both FFI
  surfaces, all three clients. **No `lakko-mls` change**, so security invariant
  #3 is not engaged.

## Implementation status

- **Decision 1 (Reply): built 2026-09-26** on the core (`lakko-attachments`
  `reply_to` / `reply_author` / `reply_excerpt`, `MessageBody::Chat { reply }`,
  `compose_reply`, the extended `message_body_parse` fuzz oracle), both FFI
  surfaces and the NSE extension, and all three clients (the Reply row first in
  the card, the composer strip, the quote block in the bubble, tap-to-jump with a
  short highlight, persistence of the quote with the message, a voice/video take
  or a GIF sent as the reply). Flow and copy in
  [`docs/ux/message-actions.md`](../ux/message-actions.md) section 3.1. Gates
  observed: `cargo test` on the four crates, web `tsc` + vitest + `vite build`,
  iOS `xcodebuild` build + 1736 unit tests, Android `compileDebugKotlin` + 2012
  unit tests. Walked live the same day between the iOS simulator and the Android
  emulator in both directions (quote on both sides, strip cleared by the send,
  tap-to-jump highlight seen on Android); web seen in the Playwright harness
  (`?case=reply`) and deployed to `test.lakko.me`.
- **Decision 2 (Forward): built end to end 2026-09-26** - core
  (`forwarded_from` + the origin pair, `MessageBody::Chat { forward }`,
  `compose_forward`, the fuzz oracle), both FFI surfaces
  (`compose_forward_envelope` / `buildForward` and the three parsed fields)
  and all three clients per [`docs/ux/message-actions.md`](../ux/message-actions.md)
  section 5.1 (the Forward row, the "Forward to" picker, re-encryption of every
  attachment, the attribution line, tap-to-jump across conversations). Walked
  live web -> iOS, iOS -> iOS and Android -> Android; see PRODUCT-READINESS.
- **Decision 3 (Pin): not built.**

## Context

The message action menu offers React, Edit and Delete. `client-redesign.md` had
been promising Reply and Copy for longer than either existed, and the maintainer
has since asked for Forward and Pin as well. Copy needs nothing from this
document - it is a clipboard call. The other three need wire format, and two of
them need rules about authority, which is what makes this an ADR rather than
three tickets.

The shape of the menu and the reasoning about what a person sees is settled in
the UX spec and is not repeated here. This document decides only what travels
between clients and what a receiver does with it.

## Decision 1 - Reply is a reference carried beside the text, not an operation

`reply_to: Option<i64>` on the v1 `Envelope`, holding the DS `messageId` of the
target, exactly as `edit_of` / `delete_of` / `react_to` already hold one.
`WIRE_VERSION` stays 1: receivers have tolerated unknown fields since 1.19 and
the crate's own module docs already name `reply_to` as the example of a v1.x
addition, so an older client shows a reply as an ordinary message rather than
refusing it.

**It is NOT one of the mutually exclusive operations, and that is the whole
decision.** `parse_body` classifies an envelope carrying more than one of
`edit_of` / `delete_of` / `react_to` as a plain chat, deliberately, because
those three are things done TO a message and doing two at once is meaningless. A
reply is not one of them: it is an ordinary message that also points somewhere,
so it must coexist with `text` and with `attachments`. Putting it in that set
would make replying and writing text mutually exclusive - the field would work
in every test and fail on the first person who typed something.

**The quote is carried as a snapshot, not resolved locally.** Alongside
`reply_to` the envelope carries the quoted author's display name and a bounded
excerpt of the quoted text, captured when the reply was composed. The maintainer
decided this, and the rule is that **a quote does not depend on the message it
quotes**: if the original is later edited or deleted, the quote stays as it was.

The first draft of this ADR proposed resolving the quote locally from the target
and rejected carrying it. That was wrong, for a reason the objection made plain:
a quote resolved locally is missing for anyone who does not hold the original -
someone who joined the group later, cleared their history, or arrived on a new
device - so the same reply would render differently for different people in one
conversation, and the person who most needs the context is exactly the one who
does not get it. A carried snapshot reads the same for everyone.

Two consequences follow and both are deliberate:

- **A quote is a claim by the replier, not a quotation anything verified.** The
  replying client writes the text and the name, and any client could write any
  text and any name. This is the same class as the forwarded-author name of
  Decision 2 and it gets the same UI rule: it must not be dressed as confirmed.
  A receiver that happens to hold the original still shows the snapshot rather
  than the current text, because two renderings of one reply depending on what a
  device happens to hold is worse than one that is merely a claim.
- **A deleted message's words survive in the replies to it.** That is the point
  of the rule rather than a leak in it: the reply is its own message, and its
  author wrote those words into it. It does mean deletion no longer erases every
  copy of a sentence, which is worth saying out loud once.

**A quote is one level deep and never nests.** What is captured is the target's
OWN text, never the target's quote: replying to a reply quotes what that person
wrote, not what they were quoting. So a bubble draws at most one quoted block
and an envelope carries at most one excerpt, whatever the length of the exchange
behind it. The rule is the maintainer's and it does two things at once - it
keeps the payload from growing with the depth of the chain, and it matches what
the person is actually doing, which is answering the message in front of them
rather than its history. Walking further back is what `reply_to` is for: one hop
at a time, through messages the device holds.

The excerpt is bounded like every other string the crate accepts, or a reply
becomes a way to make an arbitrarily large message out of somebody else's.

## Decision 2 - A forward carries a display name and nothing else

`forwarded_from: Option<String>` on the same envelope, bounded like every other
string the crate accepts, holding the original author's display name as it stood
when the message was forwarded.

The reasoning is in UX spec section 5 and the three consequences there are
normative, not commentary: a recipient cannot reach the original author through
a forward, the name is therefore unverifiable and must never be rendered as
confirmed authorship, and it is a snapshot that a later profile rename does not
update. Carrying an account id or a key instead would hand the receiving
conversation a route to somebody who never agreed to be reached by them, which
is a way around the capability-based contact model of
[ADR-0005](0005-contact-invitations.md).

**Attachments are re-encrypted, not re-sent by reference** (OQ1, closed by the
maintainer 2026-09-26). An `AttachmentRef` carries its own decryption key, so
forwarding the reference would have given the new conversation the same stored
blob for free - and left the two conversations sharing one ciphertext object,
which the storage tier could see fetched by members of both. The maintainer
chose the private answer: a forward downloads, re-encrypts under a fresh key
and uploads its own blob, so nothing on the server ties the two conversations
together. The cost is an upload per forwarded attachment; it is paid.

**A forward carries where it came from, for one purpose.** Inside the sealed
envelope, beside the name, rides a reference to the original message - its
conversation id and its DS `messageId`. It exists so that a recipient who is
ALSO a member of that conversation can tap the attribution and be taken to the
original, the way Telegram does. Two rules make it safe. The reference travels
only inside the encrypted payload and is never sent to any server, so the
server learns nothing about which message became which (the maintainer's
condition, stated as such). And a recipient who is not in that conversation
sees the name only: tapping does nothing, and it does not say why, because an
answer would confirm the conversation exists. What the recipient does learn is
an opaque conversation id they cannot open; that is the whole disclosure.

A message forwarded onward keeps the original name rather than the forwarder's,
so a chain does not rewrite its own history.

## Decision 3 - Pin is two features, and the person pinning picks one

The maintainer's decision, recorded in UX spec section 4: pinning offers a
choice between pinning for oneself and for everyone.

**Pin for me** is per-device state keyed on `(conversation, messageId)`. No wire
surface, no envelope, no authority question. It is deliberately listed as a
decision anyway, because the temptation is to build only the shared one and call
the private case a setting.

**Pin for everyone** is group state and therefore needs the three things group
state always needs. The shape follows [ADR-0051](0051-group-and-channel-names.md)
exactly, because the group name solved the same problem a week ago: a bounded,
domain-separated envelope on an MLS application message, accepted only from an
authorised sender resolved at receipt, ordered by a stamp that a re-send repeats
verbatim.

- **Who may write.** Owner or admin of the live role table
  ([ADR-0022](0022-group-admin-roles.md)), resolved from the MLS sender at
  receipt to the account key (**C-68**), never from anything the envelope
  asserts. A table-less group is open, as it is for the group name.
- **Which write survives.** A strictly-greater `(stamp, ...)` comparison owned
  by Rust and consumed over both FFI surfaces, so no client re-implements it.
  This is the lesson of [ADR-0044](0044-push-routing-secret-recency.md) applied
  before the defect rather than after: *who may write* and *which write survives*
  are different questions, and a design that answers only the first leaves the
  second to arrival order.
- **Reading it requires current state.** C-62's fifth sub-invariant applies: the
  comparison is only meaningful against the receiver's current stored pin, so the
  whole read-decide-write runs under one serializing per-`(account, group)` lock.
  ADR-0044 shipped a correct total order that a caller then consulted against a
  stale snapshot, and the order decided nothing.

**A shared pin outranks a personal one.** Where a conversation has both, the
shared pin is the one shown: it is the one the other people in the room can see
and refer to, so a device that quietly preferred its owner's private bookmark
would show that person something nobody else is looking at. The personal pin is
not lost, it is simply not what the conversation's pinned slot displays.

**Who may set and clear a shared pin** (the maintainer's decision, closing what
this ADR first left open):

- **In a group** - anyone with the standing to do it: owner or admin of the live
  role table, resolved at receipt per C-68. The same set may clear a pin
  somebody else set, which is the point rather than a concession: a pin is
  visible to the whole room, so the room's administrators must be able to take
  down something that should not be there without hunting for whoever put it up.
- **In a channel** - the channel's author.
- **In a one-to-one** - the affordance reads **Pin for both**, it outranks a
  personal pin the same way, and **either participant may clear it**. There is
  no role table in a one-to-one and inventing an asymmetry there would mean one
  of two people could pin something the other could not remove.

**Several pins, no product limit** (OQ3, closed by the maintainer 2026-09-26).
A conversation holds as many shared pins as people set, ordered newest first,
and the UI shows them through a pinned bar that displays ONE at a time with a
way to move between them; the bar's behaviour is UX spec section 4. "No limit"
is a product statement, not a codec one: the envelope stays bounded like every
other envelope in `lakko-contacts`, and a per-conversation cap is added only if
abuse ever calls for it, which nothing does today.

## Alternatives considered

- **Reply as a mutually exclusive op field.** Rejected in Decision 1: it would
  make replying and writing text exclusive.
- **Resolving the quote locally from the target message** instead of carrying
  it. Rejected in Decision 1 after the maintainer overturned the first draft: it
  renders differently for anyone who does not hold the original, which is
  precisely the person the quote exists for.
- **A forward carrying the original account id.** Rejected in Decision 2: a
  route around the contact model.
- **Only a shared pin, or only a private one.** Rejected by the maintainer in UX
  spec section 4: one makes a private bookmark impossible, the other makes "look
  at this" impossible.
- **The shared pin as a `GroupContext` extension** rather than an application
  message. Rejected for the reason ADR-0051 rejected it for the group name: it
  would cost an MLS commit per pin, and under
  [ADR-0045](0045-remote-commit-epoch-fork.md) a commit is a proposal, a sweep
  and an epoch advance - an absurd price for a bookmark.

## Consequences

- Three new optional envelope fields' worth of surface on the receive path, each
  of which must be bounded and fail-closed, and each of which extends the
  `message_body_parse` fuzz target rather than getting its own.
- One new `lakko-contacts` envelope with its own domain separator, its own
  bounded codec, its own fuzz target and its own cross-kind isolation tests
  against every sibling domain - the pattern the crate already applies to six
  other envelope kinds.
- Invariant #2 holds: every new field rides inside the MLS-sealed application
  payload or the same, and the server relays bytes it cannot read. Invariant #5
  holds: no new server-side artefact, so no new social-graph surface. Invariant
  #3 is untouched - no `lakko-mls` change.
- The forward's shared-blob property (Decision 2) is a real, if small, widening
  of what the storage tier can correlate, and it is the one thing in this
  document a security review should look at hardest.

## Status of the open questions

All three open questions are answered above: OQ1 (re-encrypt), OQ2 (who may
unpin) and OQ3 (several pins, a bar showing one at a time). Nothing in the
first implementation wave - Reply and Copy - depended on any of them.
