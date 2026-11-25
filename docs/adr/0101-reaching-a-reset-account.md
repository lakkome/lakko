<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0101: Reaching an account after a key reset

- **Status:** Accepted (maintainer, 2026-11-25: everything returns by itself after a reset; chats, groups, channels, the profile and the history)
- **Date:** 2026-11-25
- **Context:** ADR-0073 (key reset, silent continuity), ADR-0078 (servers cannot link accounts), base scenario 4b
- **Amends:** ADR-0073 R.4, ADR-0078 section 1 (the continuity carrier left open there)

## Context

After an ADR-0073 key reset the person's devices hold a new identity with a
new account seed, and with it a new mailbox. ADR-0073 R.4 let peers reach it
with a continuity reach key derived from `(account_id, key_generation)`,
registered on a mailbox whose address peers could compute from the account.
ADR-0078 section 1 made a mailbox address `BLAKE3-derive-key(mailbox_pk)`
with the mailbox key derived from the account seed, so that no server copy
joins an account to its mailbox. Since then a peer cannot compute the new
address, the continuity Welcome is a recorded no-op, and ADR-0078 left the
carrier as an open question.

Seen on 2026-11-25 (base scenario 4b on test.lakko.me): an account reset its
keys on web; its contact's device accepted the reset and found no way to
deliver the successor's Welcome. The reset screen says "Your contacts keep
their chats with you"; nothing came back. The order defects found beside it
(lineage recorded before delivery, an undelivered Welcome marked delivered)
are fixed separately on all three clients; this ADR is about the missing
carrier.

The constraint that shapes every option: a peer and the reset identity share
no secret. The person lost everything (ADR-0073 is the lost-everything path),
so the new identity knows none of its contacts, and a peer knows only what it
knew before plus what the directory serves. The operator knows the same
public facts (the account id, the attested new key from the KT log, the
generation). **Any address a peer can find without a shared secret, the
operator can find too.** The design question is therefore not whether the
operator can link the account to the place peers write, but what that place
reveals and for how long.

## Decision

Proposed (Variant A below): **a continuity mailbox, separate from the real
one, announced in the attested identity bundle for a window.**

1. **Key.** The resetting device derives a second mailbox key from the new
   account seed:
   `continuity_mailbox_seed = BLAKE3-derive-key("lakko/mailbox/continuity/v1", account_seed)`.
   Its address is `mailbox_address(continuity_mailbox_pk)` as for any
   mailbox. It is never the real mailbox and never shares a key with it.
2. **Announcement.** The reset's client order (after the attested publish)
   uploads a continuity binding to discovery beside the identity bundle:
   `version ‖ continuity_mailbox_pk(32) ‖ expires_at_ms(8 BE) ‖ sig(64)`, the
   signature by the new identity key over a domain of its own. Discovery
   stores it opaque, serves it on `GET /discovery/identity/{account_id}` while
   unexpired, and deletes it at expiry (a sweep). The window is the DS
   journal retention, 30 days today (ADR-0073 D2; chosen, not measured).
3. **Gate.** The resetting device claims the continuity mailbox as any
   mailbox and registers on it only the ADR-0073 R.4 continuity reach key
   (`continuity_reach_seed(account_id, key_generation)`), already in
   `lakko-contacts`. The DS learns nothing new: an ordinary mailbox with one
   registered reach key.
4. **Peer side.** A peer that accepted the reset (ADR-0073 R.2 verdict)
   verifies the binding under the attested new key and its expiry, then
   writes the successor Welcome (and a contact request where that is the
   path) to the continuity mailbox, signed with the continuity reach key, as
   an ordinary sealed mailbox row (ADR-0078 phase 1), spending a
   `mailbox-write` token. `newIdentityReachable` (the gate the clients now
   have) is true when the peer holds an ordinary grant for the new identity
   or a verified, unexpired continuity binding.
5. **Reset side.** For the window, the reset identity's devices read the
   continuity mailbox beside the real one, with the same mailbox code. Once
   a conversation is back, the identity's profile envelope hands its peers
   the ordinary `contact` / `member` grants and the real mailbox; the
   continuity mailbox is not written to again by that peer. After the
   window the devices stop reading it and the DS prunes it like any idle
   mailbox.
6. **Expiry is a refusal.** A peer that comes back after the window finds no
   binding; the conversation stays on its old session and its sends end in
   the ordinary "Not sent" state (ADR-0073 D2, unchanged).

7. **Channels (maintainer, 2026-11-25: channels come back too).** ADR-0073
   left them out ("a subscriber who reset is not re-admitted by anyone").
   Each role returns through the same continuity mailbox:
   - **Subscriber.** A device of the channel's owner or a publisher holds the
     subscriber list with each member card's account binding (ADR-0060
     section 3: the card opens to the account identity key and its signature
     over `channel_id ‖ subscriber_pk ‖ BLAKE3(profile)`). When the reset
     check accepts a reset for an account named in that list, the device
     sends a channel return to the continuity mailbox: the channel's
     ordinary invitation (for a private channel with an admission ticket)
     plus the old card's binding signature and profile. The reset device
     checks that signature against its own previous account key (from the
     KT log, which it reads for its own account anyway) and re-subscribes
     with no prompt; a return without a valid old signature is shown as an
     ordinary invitation, never accepted silently, so knowing the account id
     is not enough to subscribe someone to a channel. A subscriber that never
     uploaded a card (it chose not to be listed) cannot be found and does not
     come back.
   - **Publisher.** An owner device retires the old identity's leaves and
     admits the new identity's devices on the PublisherGroup (the ADR-0090
     admit path), and re-sends the read key and the newest epoch seeds; the
     Welcome goes to the continuity mailbox.
   - **Owner.** The owner key is derived from the account seed, so a reset
     loses it and nobody else holds it. Decided (maintainer, 2026-11-25: the
     owner gets the channel back with no action, even when alone):
     a **channel recovery key** bound to the account, not to its keys. Its
     seed is a VOPRF evaluation by `services/auth` under a per-account key
     `k_acct = BLAKE3-derive-key("lakko/auth/channel-recovery/v1",
     master ‖ account_id)` (no table), requested by a signed-in device with
     the channel id blinded, so auth never learns the channel id. At
     creation (and, for an existing channel, at the owner device's next
     pass) the owner registers the recovery public key with
     `services/channels`, beside the owner key; the service stores a key and
     learns no account. After a reset, the new identity's device learns its
     channel ids from a **channel return** a subscriber or publisher device
     sends to the continuity mailbox when the owner's member card names a
     key that reset (the same detection as for subscribers); it evaluates
     the recovery seed for each, and where the derived key equals the
     registered recovery key it hands the channel to its new owner key
     (`PUT /v1/channels/:id/owner` gains a recovery-key proof) and takes the
     PublisherGroup back by re-founding it at the id the new seed derives
     (ADR-0090 decision A). A channel whose owner card nobody holds (the
     owner hid it) and that no publisher knows is not returned.
     Cost, stated: whoever can sign in to the account (the password, as for
     the reset itself) can take its channels; the operator, which holds
     `master` and can already reset any account (ADR-0073), can take any
     channel whose id it knows. An owner change could be logged in the
     moderation or KT log for detectability (open).
8. **The profile comes back.** Profile envelope version 5 (flag day) adds
   an Ed25519 signature by the account identity key over the envelope. A
   peer that returns a conversation includes the reset person's newest
   profile it holds; the new identity's device keeps one only if it verifies
   under its own previous account key (read from the KT log), takes the
   newest stamp, and republishes it under its new key. The `@username`
   never left (it belongs to the account and was typed at sign-in).
9. **The history comes back, newest first.** Once the new identity's device
   is in a returned conversation it asks, inside it, for history (a field-less
   request carrying a fresh ADR-0091 gap key and a "before" message id). The
   other person in a one-to-one, or an owner or admin in a group, answers
   with the ADR-0091 gap-history construction: rows sealed to that gap key,
   signed by the sender's leaf, posted to the gap-history rendezvous, the
   newest page first (its size chosen, not measured), and the device asks for
   the next page until the sender has no more. The receiver applies the same
   signer rule as ADR-0091 (`gap_history_signer_accepted`). Channels: the
   owner's return carries the newest 64 epoch seeds
   (`channel_epoch_seeds`), so the posts the channels service retains open
   again. Only what the sender still holds comes back.

No `lakko-mls` change. No DS change. A discovery migration (one nullable
column and a sweep), one codec in `lakko-contacts` with a fuzz target, the
binding sign / verify on both FFI surfaces, the three clients.

## What the operator learns (Variant A)

- That this account has a continuity mailbox, for the window. That the
  account reset is already public in the KT log (`is_reset`).
- The arrival times and count of rows in the continuity mailbox: roughly how
  many conversations came back and when. Rows are sealed (sender and kind
  hidden) and padded to buckets, so not who wrote or what.
- Not the real mailbox: no record joins it to the account, as ADR-0078
  requires. After the window discovery deletes the binding; a copy of the
  databases taken during the window keeps the join to the continuity
  mailbox only.
- Inherited from ADR-0073 R.4: anyone who knows the account id (former
  contacts and co-members, including a co-member the person had hidden from
  under ADR-0060, and the operator) can write to the continuity mailbox for
  the window. Each write costs a token.

## Alternatives

- **B. Shared buckets.** No binding. A peer writes the Welcome, sealed to the
  new identity's X25519 key, into a shared rendezvous chosen by a short
  prefix of the new identity key (for example 12 bits); the reset devices
  read their whole bucket for the window. The operator learns only which
  bucket, not which of the accounts in it the rows are for, so the count of
  returning conversations is hidden among the bucket. Costs: every reset
  device downloads every row of its bucket for 30 days, and the gain exists
  only when many accounts reset in one window; at test scale and early
  production a bucket holds one account and hides nothing. Can be layered on
  later without changing the peer's verdict or the reset order. Not
  recommended now.
- **C. Publish the real mailbox in the bundle.** The simplest; joins the
  account to its real mailbox for good, which is exactly what ADR-0078
  section 1 removed. Rejected.
- **D. Derive the address from the account id** (ADR-0073 R.4 as first
  built, ADR-0078's "continuity mailbox derivable from account id and
  generation"). Same disclosure as A but with no expiry under the identity's
  control and a mailbox anyone who knows the id can compute and therefore
  try to claim first. A's binding is signed by the new key, so it cannot be
  squatted. Rejected in favour of A.
- **E. No automatic return** (the person re-adds contacts by hand). Rejected
  by the maintainer on 2026-11-25: the reset screen promises the contacts
  keep their chats.
- **F. The backup survives the reset** (ADR-0078's open question): the reset
  device would read its contacts from the backup and write to them itself.
  Still undecided and orthogonal: if it is ever built, it reaches peers from
  the reset side, and A stays the path for peers the backup does not name.

## Consequences

- **Positive:** chats come back by themselves after a reset, as the reset
  screen says, with no record anywhere joining the account to its real
  mailbox. Uses pieces already built (the continuity reach seed, sealed
  mailbox rows, the reach verdict and the reachability gate).
- **Negative:** invariant #5 is weakened for reset accounts during the
  window: the operator sees the count and timing of returning conversations.
  Stated, not hidden. The ADR-0060 hidden-from co-member residual of R.4
  stays.
- **Neutral:** one more field in the identity bundle; one more mailbox read
  per reset device for 30 days.

## Open questions

1. Whether the reset screen's line keeps its wording or says the chats come
   back "when your contacts open Lakko" (a peer must be online within the
   window for its conversation to return).
2. Whether a peer that missed the window should get a visible state on the
   old conversation (today: ordinary "Not sent").
3. Logging an owner change made with a channel recovery key, so a takeover is
   visible (where, and to whom).

## Plan

| Sprint | Content |
|---|---|
| S1 core | `lakko-contacts`: continuity mailbox seed and binding codec (sign, verify, expiry); channel return envelope (channel id, role, the old card's binding signature and profile, invitation, epoch seeds); history request envelope; profile v5 signed by the identity key; goldens, proptests, fuzz targets; `lakko-channels`: recovery-key owner proof; both FFI surfaces; NSE recognisers. |
| S2 services | Discovery: `continuity_binding` (migration, verified upload, served while unexpired, swept). Auth: channel-recovery VOPRF evaluation under `k_acct` (session, rate-limited). Channels: register a recovery key, `PUT owner` with a recovery proof (migration). Route tests. |
| S3 web | Reset order (continuity mailbox, binding, reach key); read loop for the window; peer side for chats, groups, subscribers, publishers, owners; profile return; history request and answer; channel recovery. |
| S4 iOS, Android | The same, in parallel. |
| S5 walk | Base scenario 4b with the return of a chat, a group, a subscription, an owned channel, the profile and history, on two clients. |

## References

- ADR-0073 (R.2 verdict, R.4 continuity key, D2 expiry), ADR-0078 (sections 1 and 7, open question), ADR-0060 (reach keys), ADR-0054 (tokens)
- `docs/ux/identity-recovery.md` sections 15 and 16
