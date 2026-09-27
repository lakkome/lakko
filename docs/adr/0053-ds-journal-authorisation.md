<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0053: A journal on the Delivery Service is opened by a key, not by its name

- **Status:** Proposed
- **Date:** 2026-09-27
- **Context:** Phase 3 (product hardening), closes G-42
- **Supersedes:** the "no authentication" posture of `services/ds` (Phase 1.3), and
  the disclosure statement of [ADR-0052](0052-reply-forward-pin.md) decision 2

## Context

`services/ds` stores two kinds of append-only journal: a **group journal**, keyed
by the MLS `group_id`, and a **mailbox**, keyed by a pseudonym. Both are read and
written by anyone who names the key (`services/ds/src/main.rs`, header: *"No
authentication on Phase 1.3 - anyone with the right pseudonym can publish or
fetch. Pseudonyms are 32 bytes random; guessing one is infeasible"*). The
argument was sound as long as a name stayed inside the set of people it belongs
to. It no longer does, in three places found on 2026-09-27:

1. **A forward carries the origin conversation's `group_id`** ([ADR-0052](0052-reply-forward-pin.md)
   decision 2). The ADR states the recipient learns *"an opaque conversation id
   they cannot open; that is the whole disclosure"*. The first half is true - the
   content is MLS-sealed - and the second is not: with the id a recipient who is
   not in that conversation can `GET` its journal (how many rows, when, how
   large: the activity of two people who did not include them), `POST` rows into
   it (bytes the members' receive loops fetch, fail to decrypt and drop, at their
   cost), and craft an external-join proposal against it ([ADR-0028](0028-group-join-ticket.md):
   *"a leaked link lets a stranger ask, never in"* - the asking arrives in a
   one-to-one as a `Join request` nobody expected). Reproduced live: Anna
   forwarded Dmitry's message into a group and into a one-to-one; both
   recipients, neither of whom shares a conversation with Dmitry, hold the
   Anna-Dmitry `group_id` in their sealed copy.
2. **A mailbox is named by a public key.** `IdentityPseudonym = BLAKE3("lakko-pseudonym/v1/identity" || identity_pk)`
   (`crates/lakko-contacts/src/pseudonym.rs`), and the web client routes contact
   requests, acks and the Welcome of a direct add to exactly that pseudonym
   (`apps/web/src/contacts/client.ts`, `publishMailbox(IdentityPseudonym.fromIdentity(...))`).
   The identity key is published by discovery and resolvable from a username, so
   anyone who can look a person up can derive their mailbox and read its journal:
   not the sealed contents, but *that* they received a contact request or were
   added to a group, and when. Writing into a mailbox must stay open - it is the
   cold inbox strangers are meant to reach - but reading it must not.
3. **A `/g/` link-holder reads the whole group journal before joining.** The
   ADR-0028 joiner reads the head epoch off the journal's public framing (paging
   to the end since 2026-09-19), posts its proposal into the journal and polls the
   journal for its Welcome. Every one of those is the capability-by-name model at
   work on a non-member by design; it is the one legitimate non-member use, and it
   has to survive.

The maintainer's framing (2026-09-27), adopted here as the rule: **no identifier
may by itself grant any access to information; the server checks a proof of the
right to see it, and shows nothing - not even existence - without one.** Hiding
identifiers from people who have no business with them (the client-side rule
"carry the origin only to co-members" that was considered first) treats the
symptom: the next leak of a `group_id` - a screenshot of a URL, a log line, a
backup - reopens it.

Two constraints bound the shape of the proof. The server must not learn **who**
the members are (invariant #5: the social graph is not materialised), which
rules out the session token of `services/auth` - a bearer identifies an account,
and a per-request `(account, group)` pair on the DS is the graph. And the server
sees **only ciphertext and public artefacts** (invariant #2), so the verifier it
holds must be public.

## Decision

**A claimed journal is opened only by a request signed with a key the journal
was claimed under.** For a group journal the key is derived by every member
from the MLS group itself - a per-epoch Ed25519 keypair whose seed is
`Group::export_secret("lakko/ds/access/v1", group_id, 32)` - so the server holds
one public key per registered epoch and nothing about the people behind it,
membership changes rotate the key with no protocol of their own, and an evicted
member loses the key at the commit that evicts them. For a mailbox the key is
the identity key its pseudonym is the hash of. A request without a valid proof
is answered as if the journal did not exist. The one legitimate non-member act,
asking to join, moves to a **knock** endpoint of its own.

### 1. The proof

Every authenticated request carries one header:

```text
X-Lakko-DS-Proof: v1.<epoch>.<unix_ms>.<nonce_b64url>.<sig_b64url>
```

`sig` is Ed25519 over the canonical bytes

```text
"lakko/ds/proof/v1" || method || canonical_path || query || BLAKE3(body) || unix_ms(8 BE) || nonce(16)
```

where `canonical_path` is the service path (`/ds/group/<hex>/messages`), never
the nginx prefix (`/api/ds/`, `/native/ds/`), so web and native sign the same
bytes. The DS accepts `unix_ms` within a two-minute window of its own clock and
refuses a `(pseudonym, nonce)` seen within that window, so a captured request
cannot be replayed. For a mailbox `epoch` is `0` and the key is the identity key.
A long-poll of up to 60 s is validated at receipt and sits inside the window.

The canonical bytes, the header codec and the verifier live in **one new crate,
`lakko-ds-proof`** (the `lakko-auth-token` shape: shared by `services/ds` and
both FFI surfaces so no client re-derives the bytes, with a fuzz target on the
header parser the server runs on attacker-controlled input). Signing is exposed
on UniFFI and wasm as `signDsProof(...)`; the group access key as
`Group.dsAccessKey()` (the seed is an exporter output, so there is **no
`lakko-mls` change** - invariant #3 is not touched).

### 2. Claiming a journal

- **Group.** At `Group::create` the founder registers the epoch-0 access public
  key: `PUT /ds/group/<hex>/access { epoch: 0, access_pk }`. A journal with no
  registration is **unclaimed** and behaves as today; the first registration of an
  unclaimed journal is accepted unauthenticated (the id is 256 random bits, and
  the flag day has no production data). Every later registration - epoch `e+1`
  before the commit that reaches it is published - carries a proof under the key
  of a **registered** epoch. The DS keeps the two newest registered epochs and
  accepts a proof under either, which is the grace a committer needs between
  registering `e+1` and its peers applying the commit, and what an
  [ADR-0040](0040-durable-pending-commit-outbox.md) crash between the two leaves
  recoverable. A registration for an epoch that is already registered replaces it
  when proven under the preceding epoch's key - the shape an abandoned
  [ADR-0045](0045-remote-commit-epoch-fork.md) staged sweep leaves behind.
- **DeviceGroup** journals ([ADR-0007](0007-multi-device-identity.md)) and the
  channel **PublisherGroup** journal ([ADR-0010](0010-channels-two-tier-broadcast.md))
  claim the same way through their own `export_secret`.
- **Mailbox.** `PUT /ds/mailbox/<hex>/access { identity_pk }`, accepted only when
  `BLAKE3(domain || identity_pk) == pseudonym`, at signup and at every login.
  Reads then require a proof under the identity key; **writes stay open** (the
  cold-inbox contract of [ADR-0005](0005-contact-invitations.md); the PoW gate
  planned for Phase 2 still applies to them). The identity key is the phrase-seeded
  account key ([ADR-0017](0017-phrase-seeded-identity.md)), which every paired
  device and the iOS NSE already hold (C-59), so nothing new is shared.
- **Rendezvous mailboxes stay unclaimed.** Pairing, the recovery payload, the
  welcome courier and the state bundle use a random 32-byte pseudonym exchanged
  out of band and alive for minutes; there the name **is** the capability by
  design and the residual (metadata of an ephemeral channel) is accepted and
  listed under G-42.

The server learns, per group, a list of public keys tagged with epoch numbers,
and per mailbox a public key it could already derive the pseudonym from. Neither
names a person, and neither is key material (invariants #2 and #5 hold).

### 3. Reads, writes and existence

For a claimed group journal both `GET` and `POST` require a proof. For a claimed
mailbox `GET` requires one and `POST` does not. Any request that fails the check
- no header, unknown epoch, bad signature, stale time, replayed nonce - receives
the response an **unclaimed, empty** journal would give: the server never says
"this exists but not for you". A malicious server can still refuse service or
serve a stale view; both are availability, which the zero-trust posture never
promised to prevent, and neither reveals content.

### 4. The knock: how a non-member asks

`POST /ds/group/<hex>/knock` is the only unauthenticated write to a claimed
group: a small (16 KiB), PoW-gated (`lakko-pow-server`, the discovery-side gate
[ADR-0028](0028-group-join-ticket.md) already names) row into a separate
`ds_group_knocks` table, pruned after seven days. The response carries the
**latest registered access epoch**, which is the group's epoch - so the joiner
no longer pages the whole journal to learn it (the 2026-09-19 finding of
ADR-0028 disappears with the read that caused it). Members read knocks with an
authenticated `GET /ds/group/<hex>/knocks?after=`, through the same receive path
that classifies an `ExternalJoinProposal` today; the ADR-0027 quarantine and the
ADR-0045 sweep are unchanged. The joiner's Welcome, which the sweep posts to the
group journal as a `WelcomeRendezvous` (C-69) because the journal was the only
place a non-member could read, now goes to the joiner's **identity mailbox**:
the sweep resolves the added leaf's account key through its [ADR-0050](0050-per-device-conversation-leaves.md)
binding and derives the pseudonym exactly as a direct add does - one delivery
path instead of two. C-69 is amended accordingly. The co-publisher joiner
([ADR-0028](0028-group-join-ticket.md) OQ3) follows the same route.

### 5. Background readers

The web service worker, the iOS NSE and the Android background decrypt fetch a
group journal on a push wake. Each already holds the MLS storage snapshot and
restores a throwaway `Group` to decrypt; deriving the access key is one more
exporter call on that same object, and signing a `GET` is not a write to group
state, so the C-59 read-only discipline is unchanged. No new secret crosses the
app / extension boundary.

### 6. What this does to the forward origin

With a claimed journal the `group_id` inside a forward grants nothing, so
[ADR-0052](0052-reply-forward-pin.md) keeps carrying it and its "tap to jump"
keeps working wherever the device holds the message - including for a member
who joins the origin group later and receives the history. One amendment stands
on its own merits and is made now: a forward **from a one-to-one** carries no
origin, since the only other holder of that conversation is its other
participant. The ADR-0052 disclosure sentence is corrected to point here.

### 7. Rollout

There is no deployment and no user data, so this is a **flag day**: the DS
gains the tables and the middleware, clients claim every group at creation and
every existing group and mailbox on first open after the update, and nothing
keeps the unauthenticated group path alive except the knock. The residual during
the transition - an unclaimed journal is exactly today's journal - is stated
rather than engineered around.

## Alternatives

- **Hide the id from people who have no business with it** (carry the forward
  origin only when every recipient is a member of the origin). Cheap and
  immediate, and it was the first proposal. Rejected as the answer: it fixes one
  road by which an id leaks and leaves the id a capability; the mailbox case
  (point 2 of the context) is not a leak at all, the name is public by
  construction.
- **The `services/auth` session token on the DS.** The natural "server checks
  the user" reading, and the one thing invariant #5 forbids: a bearer names an
  account, and a DS log of `(account, group)` requests is the social graph.
- **A symmetric per-epoch MAC key registered with the DS.** Verifies the same
  requests with the same rotation-for-free, but the server then holds a secret
  that lets it mint member requests. An Ed25519 signature costs microseconds and
  leaves the server with a verifier only; the asymmetric form is chosen.
- **A static per-group access secret distributed in-group** (the
  [ADR-0042](0042-push-opaque-group-routing-id.md) `push_routing_secret` shape)
  **with rotation on removal** ([ADR-0043](0043-push-routing-secret-rotation-on-removal.md)).
  Proven pattern, and it needed a distribution envelope, an authority gate, a
  rotation trigger and - as [ADR-0044](0044-push-routing-secret-recency.md)
  found - a recency order, to reach what the exporter gives by construction: a
  key that every member derives and that changes with the epoch.
- **A DS-side epoch gate** ([ADR-0045](0045-remote-commit-epoch-fork.md)
  Option A). Not this, and the case against it stands: the DS still parses no MLS
  and bumps no counter; it verifies a signature over a request and stores a
  public key per epoch that a member registers. A registration is not a commit
  and cannot fork one.
- **Do nothing, record G-42.** The current posture is documented and was
  accepted for Phase 1. It stops being acceptable the moment a `group_id` is
  handed to people outside the group on purpose, which ADR-0052 now does, and
  the moment a mailbox name is a public-key hash anyone can compute, which it
  always was.

## Consequences

- **Positive:** an identifier is an address again, not a pass. A leaked
  `group_id` - by forward, link, screenshot or backup - reads nothing, writes
  nothing and cannot be knocked on without paying the PoW. A stranger who knows a
  username can no longer watch that person's inbox fill. Eviction revokes access
  at the epoch, with no rotation protocol. The `/g/` joiner stops paging a
  journal it is not in. One Welcome path instead of two.
- **Negative:** a real build - the crate, the DS tables and middleware, the
  claim-at-create and claim-on-open on three clients, the proof on every DS call
  in six readers (three apps, three background paths), the knock on both sides,
  the Welcome-to-mailbox change in the sweep on three clients, and a
  `lakko-security` pass. Every DS request grows by a signature and a header. The
  two-epoch grace is a window in which a just-evicted member's key is still
  accepted for at most one epoch beyond the commit that evicted them; it is
  closed by the next commit, and the journal rows it could read are ones their
  old epoch could decrypt anyway.
- **Neutral:** `services/ds` stops being "transport and nothing else" by one
  step - it verifies signatures and stores public keys - while still never
  parsing MLS; the ADR-0045 posture is unaffected. Rendezvous mailboxes keep the
  old model and say so.

## Open questions

1. **Registration grace.** Two registered epochs is the minimum that survives the
   commit publish ordering and the ADR-0040 crash window; whether a slow peer
   two epochs behind should be locked out (as written) or served read-only for
   a bounded time.
2. **Clock skew.** Two minutes matches the auth service's expectations; devices
   with a wrong clock get a distinguishable failure only client-side (the
   server's answer is the same as for a nonexistent journal by design), so the
   client must surface "check the device clock" itself.
3. **Knock cost.** The PoW effort and the per-pseudonym knock rate are deploy
   knobs; the per-group pending cap and dedup of ADR-0027 stay the anti-spam
   boundary behind them.
4. **Channels.** `services/channels` authenticates its subscriber fetches by a
   registered subscriber key already; whether its post fetch for a private
   channel should adopt the same proof is a separate decision.
5. **Horizontal scaling.** The nonce cache and the long-poll wake map are
   in-process today; both move to the shared store when the DS does (Phase 2
   fan-out), which this ADR does not bring forward.

## Sprints

- **S1** `lakko-ds-proof` (canonical bytes, header codec, verifier, fuzz) +
  `services/ds` (migration: `ds_group_access`, `ds_mailbox_access`,
  `ds_group_knocks`; the proof middleware; claim, knock and knocks routes;
  existence-hiding responses) with integration tests over a real MariaDB.
- **S2** FFI: `Group.dsAccessKey()` / `DeviceGroup` / `PublisherGroup`, `signDsProof`,
  the mailbox proof under the identity key; UniFFI + wasm.
- **S3** web: `DsClient` signs every call; claim at create and on open; the
  service worker's wake fetch; the `/g/` joiner over the knock; the sweep's
  Welcome to the mailbox; the one-to-one forward-origin rule.
- **S4** iOS + Android: the same, plus the NSE and the background decrypt.
- **S5** docs + security: C-69 amendment, G-42 to closed, the invariant index,
  PRODUCT-READINESS, and the `lakko-security` pass (touches auth of a service and
  every client's transport; invariants #2, #4, #5).

## References

- [ADR-0005](0005-contact-invitations.md) pseudonym lifecycle; [ADR-0017](0017-phrase-seeded-identity.md)
  phrase-seeded identity; [ADR-0027](0027-external-join-approval.md) /
  [ADR-0028](0028-group-join-ticket.md) external join and the join ticket;
  [ADR-0040](0040-durable-pending-commit-outbox.md); [ADR-0041](0041-ios-silent-push-nse-decrypt.md)
  (C-59); [ADR-0042](0042-push-opaque-group-routing-id.md) /
  [ADR-0043](0043-push-routing-secret-rotation-on-removal.md) /
  [ADR-0044](0044-push-routing-secret-recency.md); [ADR-0045](0045-remote-commit-epoch-fork.md)
  (the case against a DS-side epoch gate); [ADR-0050](0050-per-device-conversation-leaves.md);
  [ADR-0052](0052-reply-forward-pin.md).
- `services/ds/src/main.rs` (the Phase 1.3 posture), `crates/lakko-contacts/src/pseudonym.rs`,
  `crates/lakko-auth-token` (the shared-verifier crate shape this follows).
- RFC 9420 section 8.5 (exporters); RFC 8032 (Ed25519).
