<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0053: A journal on the Delivery Service is opened by a key, not by its name

- **Status:** Accepted (2026-09-27, the maintainer's call; S1-S4 built the same day and host-tested; not yet deployed or walked live; S5 open)
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
"lakko/ds/proof/v1" || 0x00 || method || 0x00 || canonical_path || 0x00 || query || 0x00
  || BLAKE3(body) || epoch(8 BE) || unix_ms(8 BE) || nonce(16)
```

(NUL separators keep `method || path || query` unambiguous, and the epoch is
bound so a proof under one epoch's key cannot be presented under another
label - both added when the crate was written, S1.)

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
- no header, unknown epoch, bad signature, stale time, replayed nonce - is
refused with one uniform `401` and no detail.

> **Amended before S1 (2026-09-27).** The first draft answered a failed proof
> "as an unclaimed, empty journal would" so the server never said "this exists
> but not for you". Two facts made that the wrong trade. A member whose clock is
> wrong would see its publishes accepted and silently dropped - the failure a
> chat must never have - where a `401` lets the client say "check the device
> clock" (open question 2). And what a `401` reveals - that the name is claimed
> - is nothing a caller does not already know, since a 256-bit name cannot be
> probed and is only ever held because it was given out. So the refusal is
> loud and uniform; what stays hidden is everything behind it. A malicious server can still refuse service or
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
  **Built 2026-09-27** (`crates/lakko-ds-proof`, `services/ds/src/access.rs`,
  migration `0002_journal_access.sql`; 7 crate tests, the `proof_parse` fuzz
  target, six `LAKKO_DS_TEST_DATABASE_URL`-gated DS tests green against a local
  MariaDB). See *What S1 found* below.
- **S2** FFI: `Group.dsAccessKey()` / `DeviceGroup` / `PublisherGroup`, `signDsProof`,
  the mailbox proof under the identity key; UniFFI + wasm. **Built 2026-09-27**
  as one opaque `DsProofSigner` per journal per epoch rather than a key
  accessor plus a free signing function: `Group.dsAccessSigner(identity)` (and
  the `DeviceGroup` / `PublisherGroup` twins) returns the signer, `dsMailboxSigner`
  the identity-key one, and the signer carries `sign`, the epoch, the verifying
  key and the two claim bodies - so the exporter seed never crosses the FFI and
  no client spells a JSON field. Also on the NSE extension surface (`sign` only,
  no claim body: a claim is a write, C-59). Host tests on all three prove the
  header a signer produces is what `services/ds` verifies.
- **S3** web: `DsClient` signs every call; claim at create and on open; the
  service worker's wake fetch; the `/g/` joiner over the knock; the sweep's
  Welcome to the mailbox; the one-to-one forward-origin rule.
- **S4** iOS + Android: the same, plus the NSE and the background decrypt.
- **S5** docs + security: C-69 amendment, G-42 to closed, the invariant index,
  PRODUCT-READINESS, and the `lakko-security` pass (touches auth of a service and
  every client's transport; invariants #2, #4, #5).

## What S1 found

- **The gate is a per-handler call, not middleware.** The proof covers the
  request body, and axum's body is a stream, so the handler that publishes has
  to collect the bytes before the check can run; a layer would have to buffer
  and re-inject them. `publish` now takes the collected `Bytes` and the three
  gated handlers call `gate_group` / `gate_mailbox_read` on them (a `GET` proves
  over an empty body). Section 2's "middleware" reads as "the check every gated
  handler runs first".
- **The nonce cache is the table's primary key.** `ds_proof_nonces
  (pseudonym, nonce)` with `INSERT IGNORE`: zero rows affected is the replay.
  No in-process set, so a restart does not forget the last two minutes of
  nonces, and section 5's "moves to the shared store" is already true of it.
- **Two registrations are legal at `latest`.** A replacement of the latest
  epoch (the abandoned-staged-sweep shape) must be proven under the epoch
  *before* it, never under the key it replaces: otherwise a device holding the
  latest key could swap it for one of its own with nothing to vouch for the
  change. The test pins both directions.
- **The knock's PoW and the knock's payload are one table apart.** The proof
  of work is consumed in `pow_challenges` by the shared `lakko-pow-server`
  verifier before the row is written; a repeated proof is refused there, so a
  knock cannot be replayed for free even though the knock route itself carries
  no proof header.
- **Only the registration rows and the routes are new on the DS.** The journal
  tables, the wire of `messages` and the long-poll are untouched; the
  unclaimed posture is byte-for-byte Phase 1.3, which is what lets the flag
  day proceed client by client (S2-S4) with the server already deployed.

## The client contract (S3 + S4)

Written before the three clients were built, so they build one protocol. It
amends sections 2, 4 and open question 1 where S2 and S3 found them short.

**Server amendments made with it (S1 revision, 2026-09-27).**

- **Open question 1 is resolved: two write epochs, a read-only tail.** The two
  newest registered epochs may write (publish, knock-read, register the next
  epoch); every older registered epoch still **reads**, for the journal's own
  retention window and at most 64 epochs. As first written, a device offline
  across two commits held a key the server had dropped, could not fetch the
  commits that would bring it forward, and was stranded for good - with a
  commit on every member add, that is an ordinary week, not an edge. The cost:
  an evicted member keeps reading ciphertext it cannot decrypt (sizes, timing)
  until its epoch ages out. It cannot write.
- **Re-sending an already-registered `(epoch, key)` needs no proof** and is a
  no-op: it is what claim-on-open sends, and it grants nothing.
- **The knock takes `payloads_b64: [..]`** (at most 4 rows, 16 KiB in total,
  one PoW), and **an empty list is a probe**: nothing is stored, the response
  carries the epoch. The joiner probes, builds its proposal for that epoch,
  then knocks with the attribution and the proposal. The response is
  `{ ids: [..], epoch }`.
- **`Group.dsStagedAccessSigner(identity)`** (UniFFI and wasm) returns the
  signer of the epoch a staged sweep moves to, while the group still sits at N
  (`lakko-mls` `Group::staged_export_secret`, a read-only accessor over
  OpenMLS's own staged-commit exporter, pinned by
  `a_staged_sweep_exports_the_secret_of_the_epoch_it_moves_to`).

**1. Every DS request to a claimed journal carries the proof.** The DS client
signs `(method, service path, raw query, exact body bytes)` with the signer of
the journal: `group.dsAccessSigner(identity)` for a conversation group,
DeviceGroup or PublisherGroup journal, `dsMailboxSigner(identity)` for the
device's own identity mailbox. The path is the service path
(`/ds/group/<hex>/messages`), never the nginx prefix. Posting to **another**
account's mailbox and every **rendezvous** mailbox (pairing, recovery payload,
welcome courier, state bundle, history bundle, contact-invite reply) stays
unsigned - those are unclaimed or open for writes by design.

**2. Claim.** `PUT /ds/group/<hex>/access` with the signer's
`groupClaimBodyJson()`: at `Group.create` (and DeviceGroup founding and
PublisherGroup creation), and on every open of a group (idempotent). Unsigned
when the device holds no older signer for that group; signed with the older
signer when it does. `PUT /ds/mailbox/<hex>/access` with
`mailboxClaimBodyJson()` after signup and after every login and restore. A
failed claim is logged and retried on the next open; it never blocks the UI.

**3. Register before publish.** Every epoch-advancing commit is preceded by
registering the next epoch's key, proven under the current one:

- *direct (eager-merge) commit* (add, remove, role change, self-update,
  PublisherGroup approve): take `signerN = dsAccessSigner()` **before** the
  create call, create (the group is now at N+1), take `signerN1 =
  dsAccessSigner()`, `PUT access` with `signerN1.groupClaimBodyJson()` signed by
  `signerN`, and only then persist the ADR-0040 outbox entry and publish (the
  commit POST is signed by `signerN`). A failed registration aborts exactly as
  a failed outbox write does - roll back, no publish.
- *staged sweep*: stage, `signerN1 = dsStagedAccessSigner()`, register signed by
  `signerN = dsAccessSigner()`, then publish and adjudicate as today. A failed
  registration abandons the sweep before publishing.

A commit is never on the wire before the server knows its epoch, so no peer
that applies it holds an epoch the server refuses.

**4. Heal on apply.** The receive loop takes the current signer before
applying a batch; when the batch advanced the epoch it registers the new key,
signed by that pre-batch signer, best effort (a refusal is expected and
ignored - someone else registered first, or the device was too far behind).
This is what repairs the one race left: a losing committer that registered its
own N+1 before learning it lost; the replace rule lets any member who applied
the winning commit put the real key back, and the loser does so itself after
abandoning.

**5. Retry on 401.** A signed group request refused with `401` is retried once
signed by the previous epoch's signer when the device still holds it (a read,
before a lagging registration landed), and once after a claim-on-open. A
second refusal surfaces: if the response `Date` header is more than 60 seconds
from the device clock, the copy is "Check this device's date and time";
otherwise the ordinary delivery error.

**6. The link joiner goes through the knock.** `POST /pow/challenge` on the DS,
solve with the client's existing PoW solver, `POST /ds/group/<hex>/knock` with
`payloads_b64: []` to learn the epoch, build the external-join proposal for it,
solve a second challenge, knock with `[attribution, proposal]`. No journal read.
The joiner's Welcome arrives in its **identity mailbox**, so the joiner's
wait is on its own mailbox receive path, not on the group journal. The
co-publisher joiner does the same against the PublisherGroup journal.

**7. Members read knocks.** Next to the group journal poll, an owner or admin
client polls `GET /ds/group/<hex>/knocks?after=` (signed, its own cursor) and
feeds each row to the same classifier the journal rows use for an attribution
and an external-join proposal. The ADR-0027 quarantine and the sweep are
unchanged.

**8. The sweep delivers Welcomes to mailboxes.** Where the sweep posted a
`WelcomeRendezvous` to the group journal for every added key (C-69), it now
delivers the Welcome to each added member's identity mailbox, the same way a
direct add already does. No `WelcomeRendezvous` is written to a group journal.

**9. Background readers** sign their read: the web service worker, the iOS NSE
and the Android background decrypt call `dsAccessSigner` on the throwaway
group (the extension surface has it) or `dsMailboxSigner` for a mailbox wake. A
refusal falls back to the generic notification. No claim, no registration: a
background process writes nothing (C-59).

**10. Forward from a one-to-one carries no origin** (section 6).

## What S3 and S4 found

Built the same day on all three clients against the contract above (web S3, iOS and
Android S4), host-tested only: web vitest, iOS XCTest and Android JUnit suites green,
nothing yet run against a deployed DS.

- **The knock queue needed a gate in the core, not three filters.** A knock row is an
  unauthenticated write, so a member can post a genuine commit or application message
  there instead of the journal; the admins read knocks and would apply it, every other
  member would not, and the group forks along the line of who reads knocks. Two
  clients first filtered **after** processing (keeping only the external-join event),
  which is too late - the commit was already applied. Closed by
  `lakko_mls::Group::process_knock` over `is_external_join_proposal_for` (a
  `PublicMessage` proposal from a `NewMemberProposal` sender, framed for this group,
  refused before OpenMLS sees it), exposed as `processKnock` on `Group` and
  `PublisherGroup` on both FFI surfaces and used by all three knock readers. Pinned by
  `a_knock_row_is_processed_only_when_it_is_an_external_join_proposal`.
- **The co-publisher's Welcome lands in the account mailbox**, because a channel runs
  on the account identity, wrapped as `KIND_WELCOME` on all three clients. The contacts
  inbox reads the same mailbox and cannot open that Welcome with the device identity;
  each client suppresses the resulting join-failure copy (web by marking account-mailbox
  Welcomes, Android for an hour after a co-publish request). iOS still shows it -
  recorded, not fixed.
- **Channel rotation registers an epoch nobody else reaches.** `RotationCron` advances
  the owner's PublisherGroup with a commit that is never published (pre-existing, and
  already a divergence from co-publishers). All three clients now register the rotated
  epoch so the owner's own journal reads keep working; after two rotations a
  co-publisher's key leaves the write tier. The divergence is older than this ADR and
  is not solved here.
- **Android holds the provider lock across the registration of a direct commit.**
  Releasing it between the create call and the outbox write would let another group's
  whole-provider snapshot capture the unpublished epoch (the ADR-0040 hazard), so all
  MLS work waits for up to one DS request timeout during a direct membership commit - a
  partial reversal of the G-37 fix, reachable only in groups with no role table, where
  direct commits still exist.
- **"Claim on every open" became "claim on the first request per session"** on web and
  iOS: the claim is idempotent and cheap to repeat, but opening a chat is not a
  meaningful boundary on every client, and per session is what each could place without
  a new hook.
- **The web service worker's DS URL had never worked.** It requested
  `/api/ds/group/...`, which nginx and the Vite proxy strip to a path the DS does not
  route, so every web push preview fell back to the generic text. Fixed in the same
  change (`/api/ds/ds/group/...`).
- **The link joiner no longer reads any journal.** `journalHeadEpoch` is deleted from
  the joiner on all three clients (the 2026-09-19 paging defect goes with it); the probe
  knock returns the epoch, and the joiner waits for its group to appear through the
  ordinary device-mailbox Welcome path.

### Walked live, 2026-09-27

Deployed to the test stack the same day. A claimed four-member group carried
messages between web, iOS and Android in all six directions with signed
requests, and the same group by name alone was refused (unsigned read, unsigned
write, forged proof: 401; a competing registration: 409). The `/g/` link join
over the knock worked end to end with an iOS owner and two joiners, web and
Android: probe knock, knock with the proposal, approval, the sweep registering
the next epoch before its commit, the Welcome in the joiner's device mailbox,
messages both ways.

- **The knock's proof of work is too slow for native clients at the default.**
  `lakko-pow-server` defaults to 8 bits; the native solver (no JIT - iOS forbids
  one) needs about 21 s on a Mac for 8 bits and took minutes on the Android
  emulator, past the 300 s challenge life, so the Android knock failed with 401
  until the test stack set `LAKKO_DS_POW_EFFORT_BITS=0`, the value its other
  services already use. The same default governs production auth, discovery and
  push-gw, where no mobile client has exercised it either. Open question 3
  (knock cost) is now a measured problem, not a tuning note.
- **iOS polled the knock queue without a throttle** - about twice a second
  across the groups it owns, and 45 times a minute for a group whose info sheet
  was open, because the join-attribution label re-read the whole queue on every
  poll. Web had the same attribution re-read. Fixed on all three the same day:
  knock reads are held to once per 10 s per group, and the attribution label is
  re-read only when the set of pending joiners changes. Measured after the fix
  with a pending request and the sheet open: 4 to 5 reads a minute per group.
- **Join screen copy** (Android showed protocol words, web a raw error, the
  three said success three ways). Unified on all three: "You joined the group."
  with Open chat; "Your request didn't go through. Check your connection and try
  again." with Try again; "This group isn't taking requests yet. Try again
  later."; "Check this device's date and time." A knock whose proof of work
  expired mid-solve is retried once with a fresh challenge.
- **The owner's pending request read "Member"** even when the joiner was a
  contact: the request carries the joiner's device leaf key, and contacts are
  keyed on the account. `Group::pending_external_join_account_key` (core + both
  FFI surfaces) names the account from the retained KeyPackage's verified device
  binding; re-walked, the request reads "Maria Lopez".

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
