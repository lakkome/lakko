<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0078: Lakko's servers cannot link accounts: sealed senders, anonymous admission, unlinkable ids

- **Status:** Proposed (2026-10-14). The maintainer accepts or amends it; the
  decision points are collected at the end.
- **Date:** 2026-10-14
- **Context:** security model, invariant #5 (the social graph is not
  materialised on the server)
- **Amends:** [ADR-0005](0005-contact-invitations.md) (its "the server only
  sees pseudonym -> pseudonym routing" claim was false for the request and ack
  wire, section 1 below), [ADR-0021](0021-sfu-peer-identity-exposure.md)
  (peer tags), [ADR-0033](0033-call-admission-antispam-throttle.md) (the
  admission throttle), [ADR-0042](0042-push-opaque-group-routing-id.md) /
  [ADR-0054](0054-anti-abuse-without-client-work.md) /
  [ADR-0057](0057-journal-ownership.md) (the wake path and push bindings),
  [ADR-0050](0050-per-device-conversation-leaves.md) (the re-admission sweep),
  [ADR-0053](0053-ds-journal-authorisation.md) (mailbox claims, knocks),
  [ADR-0054](0054-anti-abuse-without-client-work.md) (token refill),
  [ADR-0060](0060-scoped-discovery.md) (mailbox addresses, per-group reach
  keys), [ADR-0028](0028-group-join-ticket.md) /
  [ADR-0009](0009-link-based-call-invitations.md) (invite ledgers),
  [ADR-0064](0064-conversation-backup-and-device-re-entry.md) (external
  commits on re-entry, not built yet)
- **Related:** [ADR-0055](0055-websocket-delivery.md) (proposed; conflicts
  with section 11), [ADR-0065](0065-attachment-lifetime-and-access.md)
  (anonymous blob passes)

## Context

### The requirement

The maintainer's requirement, stated firmly: Lakko's own servers must not be
able to reconstruct the social graph - who talks to whom, who is in which
group, channel or call together - neither from what they store nor from what
they see in requests, even if the operator is malicious or a server is fully
compromised.

Invariant #5 says the same in one line ("the social graph is not materialised
on the server"). An inventory of the tree on 2026-10-14 found it broken in
many places. This ADR records that inventory, splits it by what an adversary
must hold to exploit each finding, and decides a mechanism per finding.

### Threat model

Three adversary classes, each including the one before it:

- **(a) At rest.** The operator holds every database, the environment file
  and every log, as files. Today every service, MariaDB and one
  `/etc/lakko/lakko.env` (`EnvironmentFile=` in every
  `infra/systemd/lakko-svc-*.service` and `lakko-sfu.service`) live on one
  host; only coturn runs elsewhere. **Every pepper is in that file**, so a
  keyed hash under a server pepper (`ds_push_bindings`, the login handle
  table) protects nothing against class (a).
- **(b) Live inside the services.** The operator also runs modified service
  binaries, or reads every request and response inside a service: request
  paths, headers, bodies and tokens, at the moment they are processed.
- **(c) Network origin and timing.** The operator also sees which client
  address made which request, when, and over which connection, across all
  services.

A finding is placed in the weakest class that exploits it. A design that
closes class (a) but not (b) is still progress: it means a database dump, a
backup, or a seized disk no longer contains the graph. Class (c) cannot be
closed by any change inside servers we run (section 12).

### The inventory, verified against the code

Each item below was re-read in the tree on 2026-10-14 unless marked
otherwise. "Same ms" and "seconds apart" are properties of the code paths,
not measurements of traffic.

1. **Contact request and ack name both people in the clear (class a).**
   `ContactRequest` carries `from_pubkey` (the sender's Ed25519 identity key)
   and `from_account_id` outside `sealed_envelope`
   (`crates/lakko-contacts/src/request.rs`, struct `ContactRequest`, schema
   v4). `ContactAck` is 185 bytes with no sealed envelope at all and carries
   `accepter_pubkey` and `accepter_account_id` (`ack.rs`, module docs "No
   optional fields, no sealed envelope"). Both are stored in
   `ds_mailbox_messages` (`services/ds/migrations/0001_init.sql`) for 30 days
   (`RETENTION_SECS`, `services/ds/src/journal.rs`). The recipient is named
   by the mailbox: a mailbox pseudonym is `IdentityPseudonym::from_identity`
   = BLAKE3 over a domain and the public identity key
   (`crates/lakko-contacts/src/pseudonym.rs`, used through
   `lakko_ds_proof::mailbox_pseudonym`), and `ds_mailbox_access` stores the
   identity key itself beside it (`0002_journal_access.sql`, column
   `identity_pk`). Discovery maps account id to identity key
   (`GET /discovery/identity/{account_id}`). **The DS database alone is the
   contact graph**: a request row in mailbox(Bob) names Alice; an ack row in
   mailbox(Alice) names Bob.
2. **Calls name the account (class a and b).** The call JWT's `sub` is the
   account id beside `call_id` (`crates/lakko-call-jwt/src/lib.rs`,
   `CallJwtClaims`); `POST /call/jwt` is bearer-authenticated
   (`services/auth/src/call_jwt.rs`, the `..._without_bearer_rejects_401`
   test), so auth sees every join; `call_admission_events (call_id,
   account_id, at)` is written for accounts younger than the guest window
   (`services/auth/src/throttle.rs`, ADR-0033). Members that never join mint
   JWTs to probe `GET /v1/calls/{call_id}` every
   `SFU_CALL_STATE_PROBE_MS = 10_000` ms (`apps/web/src/calls/sfu-call-state.ts`),
   so auth learns which members of a group are online while a call runs. The
   SFU holds `account_id` per peer in memory (`services/sfu/src/peer.rs`),
   sends the first 8 characters of it to every other peer as
   `account_pseudonym` (`services/sfu/src/router.rs`, `pseudonymise`), and
   logs it at info on admission (`signaling.rs`, "ws: admission ok"). The
   ADR-0021 `peer_tag` is BLAKE3 over `call_id` and `account_id`
   (`crates/lakko-sfu-proto`, `peer_tag`), computable by any party that knows
   both, the server included. The TURN username is
   `<expiry>:<account_id_hex>` (`services/auth/src/turn.rs`) and coturn sees
   it. The JWT travelling in URL query strings is being moved to headers in
   a parallel change and is not decided here.
3. **Access logs (class a).** nginx access logs were on for the whole API;
   they are being turned off in a parallel infra change. Service logs at
   info level carry identifiers too, verified: auth logs `account_id` on
   login, TOTP and login-handle bind (`opaque.rs`, `totp.rs`,
   `login_handle.rs`, `passkey_vault.rs`); discovery logs
   `uploader_account_id` and `issuer_pubkey` on every group invite upload
   (`group_invite.rs`), which ties an account to a group's inviter key; the
   SFU logs `call` and the account prefix; push-gw logs 4-byte prefixes of
   the device pseudonym and the push token on every register and every send
   (`registry.rs`, `provider.rs`). Every service mounts
   `TraceLayer::new_for_http()`, whose spans carry the request URI (journal,
   mailbox and account ids are path segments) at debug level.
4. **Journal membership by origin (class c).** Members poll
   `/ds/group/{J}/messages`; the same device polls its account mailbox (a
   public function of the identity key) and its DeviceGroup journal, whose id
   is `BLAKE3("lakko-device-group/v1" || account_id)`
   (`crates/lakko-mls/src/device_group.rs`, `group_id_for`). The DeviceGroup
   id alone is a class (a) link: auth and discovery hold every account id, so
   every DeviceGroup journal in the DS is attributable to its account.
5. **The re-admission sweep (class b, c).** Every 10 minutes
   (`READMISSION_INTERVAL_MS`, `apps/web/src/chat/device-readmission.ts`) a
   web client fetches `/discovery/keypackage/{account_id}` (and the identity
   check of ADR-0073 R.2, `checkContactIdentity`) for its own account and
   for every other roster person of every chat whose account id it knows - a
   contact row, else the id the person's profile envelope carries
   (`apps/web/src/chat/BackgroundReceiveContext.tsx`). iOS and Android were
   not re-read for this ADR; ADR-0073 item 14 records them looking up
   accepted contacts only. Both routes are open
   (`services/discovery/src/main.rs`, the `open` router) and name the account
   in the path. Discovery sees, per requester address, the set of accounts
   that share chats with it.
6. **Joins by stored timing (class a).** A membership commit row in
   `ds_group_messages` and the Welcome row in the added account's mailbox are
   written by one client in one flow and stored with millisecond
   `created_at`, for 30 days. On web every mailbox payload starts with a
   clear kind byte, `KIND_WELCOME = 0x02` (`apps/web/src/contacts/envelope.ts`).
   Beyond the kind byte, **an MLS Welcome names its recipient by
   construction**: each `EncryptedGroupSecrets` carries `new_member`, the
   `KeyPackageRef` (a hash of the KeyPackage) of the joiner (RFC 9420
   section 12.4.3.1), and discovery serves every KeyPackage by account id, so
   the server can compute the ref of every published KeyPackage and match it.
   Journal epoch changes are visible anyway: the ADR-0053/0057 epoch key is
   registered with the DS right before a commit is published, and MLS
   `PrivateMessage` framing carries `epoch` and `content_type` in the clear.
7. **Knocks and channel journals (class a).** `ds_group_knocks` (7 days,
   `KNOCK_RETENTION_SECS`) holds the external-join proposal, a
   `PublicMessage` (`crates/lakko-mls/src/group.rs`, `process_knock` docs)
   whose KeyPackage leaf carries the ADR-0050 device binding with the
   joiner's account key, and the join attribution with the plaintext `/g/`
   capability (group id, issuer key = the inviter). The PublisherGroup
   journal id is `"lakko/pubgrp/v2/" || channel_id`
   (`crates/lakko-mls/src/publisher_group.rs`, `publisher_group_id_bytes`),
   and `services/channels` knows every channel id.
8. **Push (class a for DS + push-gw, class b live).** The DS sees `push_gid`
   in `x-lakko-wake` on every waking publish (`services/ds/src/wake.rs`) and
   stores `keyed-BLAKE3(LAKKO_DS_BINDING_PEPPER, push_gid)` per journal in
   `ds_push_bindings` (`0006_journal_ownership.sql`). push-gw stores
   `push_group_subscriptions (device_id, group_id)` and
   `push_tokens (device_id, provider, token)`
   (`services/push-gw/migrations/0001`, `0002`), with one device pseudonym
   subscribed to all its groups: **push-gw alone holds the devices x groups
   bipartite graph**, and with the DS binding (pepper in the env file) each
   group is the DS journal. This makes C-60 sub-invariant 1 ("a
   push-gw-vs-DS cross-correlation is impossible without the secret")
   false since ADR-0054 S1.
9. **Anonymous tokens link by timing (class b, c).** The web pool refills a
   context when it falls below `LOW_WATER` (1 for most contexts) in batches
   of 1 to 8 (`apps/web/src/tokens/anon-token-pool.ts`), so the bearer
   issuance (`POST /auth/tokens`, `AuthenticatedAccount`) lands seconds from
   the spend it refills for, from the same address. `anon_token_issuance`
   stores per-account per-epoch counts (`services/auth/migrations/0019`).
10. **Blobs (class c).** `/blob/{sha256}` takes no credential; uploader and
    downloaders are linked by address. ADR-0065's anonymous passes are
    accepted and not built.
11. **TURN** - see item 2.
12. **Contact add by username (class b, c).** OPRF lookup, then the identity
    and KeyPackage fetch by account id, then the mailbox write, from one
    address within seconds.
13. **Invite ledgers (class a).** `invites.owner_account_id` and
    `call_invites.owner_account_id` (`services/discovery/migrations/0004`,
    `0007`, used by `account.rs` for deletion), `group_invites.issuer_pubkey`
    (`0014`); `/g/` consumer tags are recomputable from the plaintext
    capability in the knock (item 7).
14. **Per-group reach keys (class a).** ADR-0060 `group`, `member` and
    `channel` reach keys are derived per (person, scope id)
    (`crates/lakko-contacts/src/reach.rs`); the proof header names the key
    (`crates/lakko-ds-proof/src/lib.rs`, the sixth field). Every write to a
    mailbox under such a key carries a stable per-group label; writes under
    one label are writes from co-members of one group.
15. **Channels (class c).** One device polls all its channels' subscriber
    envelopes from one address. Whether subscriber keys are per-channel
    random on all three clients was **not verified** for this ADR.
16. **Registrar pseudonyms (class a).** Per-journal registrar keys
    (ADR-0057) are acknowledged as stable per-journal pseudonyms; they name
    no account.

Two findings surfaced while verifying, not in the original inventory:

- **ADR-0064 external commits will name the joiner.** RFC 9420 requires a
  message from a sender that is not a member (an external commit) to be a
  `PublicMessage`; its UpdatePath leaf node carries the credential, which in
  Lakko is the ADR-0050 device binding with the account key. When ADR-0064
  is built, every re-entry would write the account key into the journal in
  the clear. To be confirmed against OpenMLS when ADR-0064 is built.
- **SFU relay payloads are not encrypted.** The `signal-relay` payload is
  opaque to the SFU in the sense that it is not parsed, but the media-state
  envelope of `lakko-contacts::media_state` rides it in plaintext. It names
  nobody today; section 3 puts a peer announcement on that channel, so the
  channel must be sealed first.

### What was claimed

ADR-0005 states, in its Context and Consequences, that "the server routes
ContactRequests by encrypted envelope without seeing the identity of either
sender or recipient" and "the social graph cannot be reconstructed even on
full compromise". **For the request and ack wire this was false when it was
written**: the request carried `from_pubkey` from its first version, and the
ack has never had a sealed part. ADR-0005 needs a correction note pointing
here (listed under Consequences).

## Decision

**No server we run may hold, at rest, any pair of identifiers that ties two
accounts together, or an account to a conversation, call, channel or group;
and no request a service must process may carry such a pair, except where
section 12 says the link is only removable by separating operators.** Each
finding gets a mechanism below. All are flag days: there are no production
users and no persisted data to migrate.

### 1. Sealed sender for every mailbox row (finding 1, 6)

- **One mailbox envelope.** Every payload stored in a person's mailbox
  becomes `version(1) || eph_x25519_pk(32) || AEAD(...)`, a sealed box to the
  mailbox owner's X25519 key (the existing `lakko-contacts::envelope`
  sealed-box construction), padded to a fixed set of size buckets. The
  plaintext is `kind(1) || body`: a contact request, a contact ack, a
  Welcome, and any later kind. The web clear kind byte (`envelope.ts`) and
  its native mirrors go away; the kind is inside.
- **The request and ack move inside unchanged in meaning.** The inner body
  is today's signed `ContactRequest` / `ContactAck` wire, including
  `from_pubkey`, `from_account_id`, `from_pseudonym` and the signature, so
  the recipient's verification does not change. The DS sees: the mailbox,
  the size bucket, and the ADR-0060 reach proof or delegation it already
  requires for anti-abuse.
- **The Welcome moves inside too.** Sealing it is not cosmetic: it hides
  `new_member` (finding 6).
- **Reply keys stop timing the request.** Today the requester registers its
  `reply` reach key on its own mailbox just before sending the request, so
  `ds_mailbox_reach.created_at` sits next to the request row in the other
  mailbox and the later ack is written under that same key. Reply keys are
  instead registered ahead of time from a small pool, at randomised times,
  and each request uses one from the pool.
- **Mailbox addresses stop being a public function of the identity key.**
  The address becomes `BLAKE3(domain || mailbox_pk)` for a mailbox key pair
  derived from the account seed (`BLAKE3-derive-key("lakko/mailbox/v1",
  account_seed || generation)`), and the claim stores `mailbox_pk`, not
  `identity_pk`, so `ds_mailbox_access` no longer joins to discovery's
  identity bundles. A writer learns the address with the means it already
  needs to write at all: a reach grant (the grant wire grows by the 32-byte
  address), a link capability (inside the sealed capability), an
  introduction record (inside the sealed payload), or the ADR-0060 public
  record of a person in Everyone (who has chosen to be findable, so that
  binding is accepted). Device mailboxes derived from a device key that is
  published in a KeyPackage go away; ADR-0060 already has one inbound
  mailbox per person.
- **What breaks.** (i) Every path that computes a mailbox from an identity
  key: `accountMailboxOf` / `mailboxOfExactKey`
  (`apps/web/src/reach/mailbox-write.ts`) and their native twins, ADR-0074
  "resolve before you address", and the ADR-0073 R.4 continuity key, which
  is derivable from account id and generation precisely so that peers can
  reach a reset account. The continuity path needs a new carrier for the new
  address; see the decision points. (ii) An open (Everyone) mailbox takes
  proofless writes today; under sealed sender nothing about the writer is
  visible, so such writes should spend an anonymous token (new context).

### 2. Calls admitted anonymously (finding 2, 11)

- **Admission by anonymous token.** `POST /call/jwt` stops taking a bearer.
  It spends one token of a new `call-join` context and returns a JWT whose
  claims are the `room` id (defined below), a fresh random `peer_id`,
  `role`, `iat`, `exp`; no `sub`, no `call_id`. Auth keeps the signing key
  and JWKS; the SFU verifies as today. Auth then needs the ADR-0054
  spent-set table, which today only the consuming services carry.
- **The SFU holds no account id.** `Peer.account_id`, `account_pseudonym`
  and the account prefix in logs are removed; the SFU keys everything on the
  random `peer_id`.
- **Peer tags without the server.** The ADR-0021 server-derived `peer_tag`
  is removed. Each participant announces its own SFU `peer_id` to the call:
  `Sign_leaf(call_id || peer_id)` with its MLS signature key, sealed under a
  key derived from the call seed (`lakko/call/relay/v1`) and sent over
  `signal-relay`. Receivers verify the signature against the roster leaf
  (account-bound through ADR-0050), so the binding stays spoof-proof and
  the SFU never sees it. Every other relay payload (media state) is sealed
  under the same key.
- **The ADR-0033 throttle is rebuilt on the token budget.** The issuer
  already knows the account at issuance, so the "fresh account" signal moves
  there: young accounts get a small `call-join` quota per epoch. The SFU (or
  auth at JWT time) caps admissions per `room` per window with no account
  id. `call_admission_events` is dropped. Whether issuance uses a separate
  key family for fresh accounts (which leaks one bit at spend) is a decision
  point.
- **The call-state probe needs no identity.** The SFU names a room by
  `room = BLAKE3-derive-key("lakko/sfu/room/v1", call_id)`. The `call_id` is
  16 random bytes that today reach a server only inside the JWT; under this
  section it stays inside the E2EE call signalling, so only members can
  compute the room id. The admission JWT carries `room` in place of
  `call_id`, and the state route is `GET /v1/rooms/{room}` with no token:
  knowing the room id is the capability. No JWT is minted to probe, so auth
  no longer sees probes. (A room derived from the call seed was considered
  and rejected: the seed changes with the MLS epoch, ADR-0029, so a late
  joiner could not compute the room a call started in.)
- **TURN without the account.** The TURN username tag becomes random per
  credential (`<expiry>:<random>`); credentials are issued beside the call
  JWT through the same anonymous exchange, not through a bearer route.

### 3. Push without a bipartite graph (finding 8)

- **The wake is sealed to push-gw.** The `x-lakko-wake` header becomes a
  sealed box to push-gw's X25519 public key (the `lakko-contacts::envelope`
  construction; `lakko-crypto` has no HPKE) containing `push_gid`, kind, a nonce, a
  timestamp, an unlock key (below) and a signature by a **wake key**
  `derive(push_routing_secret, "lakko/push/wake-sign/v1")`, whose public half
  each subscriber registers at push-gw on subscribe. The DS forwards the
  blob unread and rate-limits per journal. `ds_push_bindings` and
  `LAKKO_DS_BINDING_PEPPER` are removed: the anti-spoofing they gave
  (ADR-0057: a journal may wake only its own group) is now the wake-key
  signature, which only members hold, plus nonce dedup at push-gw.
- **Subscriptions unreadable at rest.** push-gw stores per subscription
  `(push_gid, AEAD_{unlock_key}(token, provider))`, where
  `unlock_key = derive(push_routing_secret, "lakko/push/wake-unlock/v1")`.
  The key travels only inside each sealed wake and is never stored, so the
  push-gw database and env file at rest hold no token equal across groups
  and no device pseudonym. A device keeps a random id per subscription so it
  can remove its own rows, and re-registers every subscription when its
  provider token changes. `push_tokens` keyed by a device pseudonym, and
  `push_group_subscriptions`, are removed.
- **Class (b) residual, stated.** A live push-gw decrypts the tokens of
  every subscription a wake names and sends to them, so it sees, per wake,
  which device tokens were woken together, and can accumulate group
  co-membership over time. No design that delivers through APNs / FCM / Web
  Push removes this, because whoever calls the provider must name the token.
  Mitigations (cover wakes, batching) blur it; separating the push operator
  (section 12) is what removes it. FCM topics would move the set to Google;
  APNs has no general topic mechanism.

### 4. Knocks sealed to the group (finding 7, 13)

- The `/g/` capability (inside its sealed link) gains a per-link X25519
  **knock key**. The issuer derives the private half from its journal root
  seed or mints it, and hands it to the group's admins in-group (the
  ADR-0057 registrar-seed envelope pattern). A knock payload - the
  external-join proposal and the join attribution - is sealed to it. The DS
  stores opaque bytes and still returns the epoch.
- Symmetric sealing under the link secret was rejected: every link holder
  could then read every other knock on that link, including other joiners'
  account keys.
- The same applies to ADR-0064 external commits when they are built: the
  journal row is wrapped under a group transport key the re-entering device
  holds from its vault head, so the account key in the leaf node is never in
  the clear on the DS.

### 5. Welcome timing decoupled (finding 6)

- Sealing (section 1) removes the kind byte and the KeyPackage ref. The
  remaining link is timing: a sealed mailbox row of Welcome size close to an
  epoch change in journal J. The DS sees journal epoch changes anyway
  (ADR-0053 registration), so the decorrelation must happen on the mailbox
  side.
- Clients send a Welcome after a random delay in `[0, T]`, and all mailbox
  rows are padded to the same buckets so a Welcome, a request and an ack are
  not told apart by size. `T` trades a new member's first-message latency
  for anonymity; see the decision points.
- Hiding `content_type` and `epoch` behind an outer group wrap was
  considered and rejected: the epoch-key registration discloses epoch
  changes regardless.

### 6. Deterministic ids derived from secrets (finding 4, 7)

- DeviceGroup journal id:
  `BLAKE3-derive-key("lakko/device-group/id/v2", account_seed)`. Every
  device of the account holds the account seed (ADR-0017, ADR-0050), and the
  ADR-0064 `devices` vault is already derived the same way. An ADR-0073 key
  reset changes the id, and a reset re-founds the DeviceGroup anyway.
- PublisherGroup journal id: derived from a secret the channel's publishers
  hold (the owner's journal root seed and the channel id), not from the
  channel id alone. `lakko_mls::channel_id_from_group_id` (FFI `channelIdFromPublisherGroupId`) goes away; a
  co-publisher capability carries the channel id inside its sealed link.

### 7. No periodic discovery sweep by account id (finding 5, 12)

- The scheduled re-admission arm is removed. Its cases are covered without
  it: a person's own devices add their new devices (the ADR-0050
  self-service arm), a restored device re-enters by ADR-0064 knock or
  external commit, and a forgotten device is retired by the account's own
  devices from their DeviceGroup roster, which needs no directory fetch.
- When a KeyPackage of another account is needed (adding a contact to a
  group), it is fetched once, at that moment, never on a schedule.
- The ADR-0073 R.2 scheduled identity check moves to an in-group signal:
  the person whose key changed announces it in each chat, and a peer fetches
  once on that signal. ADR-0073 defines no such announcement today (its
  successor groups and Welcomes are the visible effect of a reset); adding
  one is part of this phase, and whether a signal-only check is enough for
  R.2 is a decision point.
- A username lookup returns everything a contact add needs in one response
  (identity, mailbox address, a KeyPackage), so no second request names the
  account.

### 8. Anonymous tokens decoupled from spending (finding 9)

- Refill on a randomised schedule independent of spending, in batches
  sized to a day of use, and keep a reserve so no spend waits for an
  issuance. Issue on a connection separate from the spending requests.
- `anon_token_issuance` keeps per-account counts only as long as the quota
  needs them (the current and previous epoch), which it already does
  through the pruner; it records volume, not peers.

### 9. Reach keys without per-group labels (finding 14)

- `group`, `member` and `channel` reach keys stop being per scope: one key
  per class per account, handed out in every group and channel where the
  setting allows it. The server learns "a co-member of some group of this
  person wrote", not which group.
- Cost: leaving one group cannot revoke that group's key without rotating
  the class key everywhere (re-distribution through the profile envelope in
  every remaining group). Whether to accept that, or keep per-scope keys and
  accept the label, is a decision point.

### 10. Invite ledgers without owners (finding 13)

- `owner_account_id` and `issuer_pubkey` are removed from the three
  ledgers. Each ledger row stores a random per-link revocation public key
  instead; the issuer keeps the private half and signs a revocation with it.
  Account deletion stops deleting by owner (`account.rs`); links expire by
  TTL, and a deleting client revokes its live links first.
- With knocks sealed (section 4), `/g/` consumer tags are no longer
  recomputable from the knock journal.

### 11. Logs carry no identifiers (finding 3)

- No log line at info or above may carry an account id, identity key,
  journal id, mailbox address, call id, room id, channel id, `push_gid`,
  device pseudonym, push token, blob id, or any prefix of one. TraceLayer
  spans record the route template, not the URI.
- Enforcement: a test or lint that fails CI when a `tracing` macro names a
  denied field, plus a capture layer in service integration tests that fails
  on any 16- or 32-byte identifier in output. Which of the two is a decision
  point.
- nginx access logs off and error logs at `crit` (the parallel infra
  change); coturn logging reviewed on the same terms.

### 12. Network origin and timing: a later phase, its own ADR

No change inside servers one operator runs removes class (c). Whoever sees
the client's address and the request together can link a device's journals,
mailbox, channels, blobs and calls, however opaque each request is. Removing
it requires that the party who sees the address is not the party who sees
the request:

- **Oblivious HTTP (RFC 9458)** through a relay run by an **independent
  operator**: the relay sees the address and an encrypted request; Lakko's
  gateway sees the request and the relay's address. It covers
  request/response APIs: discovery, auth, token issuance, mailbox writes,
  journal fetches and publishes, storage. Each journal must be fetched in its
  own request, so a batched multi-journal poll is excluded.
- **What OHTTP does not cover:** WebSocket and long poll (one connection
  spans many requests), and SFU media. ADR-0055's single foreground socket
  subscribing to every journal of a device would hand the DS the device's
  journal set per connection; it conflicts with this ADR unless subscriptions
  ride separate connections through a relay. SFU media exposes every
  participant's address to the SFU; an independently operated TURN relay
  with relay-only ICE moves that to a party that does not see rooms.
- **Tor** (or the ADR-0026 tunnels operated by a third party) is the
  alternative for clients that already use it.
- **At minimum, distinct operators per service** with separate hosts and
  separate secrets, so class (a) needs more than one seizure.

This ADR recommends treating class (c) as a later phase with its own ADR,
after sections 1 to 11 have removed every link that class (a) and (b) can
read without it. Until then, the honest statement is: an operator watching
its own network edge can still reconstruct much of the graph by timing.

### Phasing

Each phase lands with a test per finding that asserts the server-side data
or request no longer carries the link.

1. **Sealed sender (section 1).** `lakko-contacts` (mailbox envelope, request
   and ack inside it, reach-grant wire), both FFI surfaces, the three clients'
   mailbox send and poll, `services/ds` (claim stores `mailbox_pk`).
   Tests: a stored mailbox row decodes to no account id and no identity key
   (DS integration test over a full contact add); `ds_mailbox_access` has no
   identity key column; a fuzz target for the envelope.
   This phase first: it is the graph from one database.
2. **Calls (section 2).** `lakko-call-jwt`, `services/auth` (anonymous
   route, `call-join` context, spent set, quota, TURN), `services/sfu`,
   `lakko-sfu-proto`, the three clients. Tests: a minted JWT has no `sub`;
   `call_admission_events` is gone; the SFU's peer type has no account
   field; the probe route needs no JWT.
3. **Push (section 3).** `services/push-gw`, `services/ds` (wake forward,
   binding removal), `lakko-contacts::push_routing` (wake and unlock key
   derivations), the three clients. Tests: push-gw tables hold no token in
   the clear and no device pseudonym; the DS schema has no push binding; a
   wake not signed by the wake key is refused.
4. **Knocks (section 4).** `lakko-contacts::group_invite`, the clients' knock
   paths, admin key distribution. Test: a stored knock row decodes to no
   account key.
5. **Welcome timing (section 5).** Clients. Test: the Welcome send path
   applies the delay; size buckets are equal across kinds.
6. **Deterministic ids (section 6).** `lakko-mls` (`DeviceGroup::group_id_for`,
   `publisher_group_id_bytes`); invariant #3 applies: property tests. Test:
   the id is not computable from the account id or channel id alone.
7. **Discovery sweep (section 7).** Clients. Test: no scheduled caller of the
   by-id discovery routes remains.
8. **Tokens, reach keys, ledgers, logs (sections 8 to 11).** Tests: no
   ledger table has an owner or issuer column; the log lint or capture test
   (decision point 5) is in CI; the token pool refills on a timer, not on a
   low-water mark.

## Alternatives

- **Pad and batch only, keep the clear identifiers.** Rejected: class (a)
  reads the identifiers directly; timing defences are moot while names are
  stored.
- **Encrypt the database at rest under a server key.** Rejected: the key is
  in the environment class (a) holds, and class (b) sees the plaintext.
- **Signal-style sealed sender with server-issued sender certificates.**
  Rejected as unnecessary: anti-abuse already rests on reach proofs and
  anonymous tokens, and the inner request is signed by the identity key the
  recipient verifies.
- **Move the graph-revealing services to a TEE / enclave.** Deferred: it
  changes class (b), not (c), and ties the trust model to a vendor; it can be
  revisited beside the ADR-0049 enclave follow-up.
- **Accept the current state and document it.** Rejected by the
  requirement.

## Consequences

- **Positive:** a seized database, backup or log no longer contains the
  contact graph, call participation, group membership or channel
  publishers; most class (b) links go too; class (c) is named as the
  remaining work instead of being hidden behind claims.
- **Negative:** flag days on the mailbox envelope, the call JWT, the wake
  header, the reach-grant wire, the `/g/` capability, the DeviceGroup and
  PublisherGroup ids; more client work (key pools, delays, in-group
  announcements); per-group reach-key revocation weakened if section 9 is
  taken; a Welcome reaches a new member later by up to `T`; the ADR-0033
  throttle loses its per-account precision; account deletion no longer
  sweeps invite rows by owner.
- **Neutral:** invariant #2 is unchanged (no key material moves to a
  server); invariant #3 is touched only by section 6 (group id derivation),
  with property tests; invariant #4 is unchanged, every new decode is
  fail-closed.

### Documentation this ADR implies, to apply when accepted

- **ADR-0005:** a dated correction note: the request carried the sender's
  identity key and account id outside the seal from its first version and
  the ack has never been sealed, so the "pseudonym -> pseudonym" and "cannot
  be reconstructed even on full compromise" lines were false; pointer here.
- **ADR-0021, 0033, 0042, 0050, 0053, 0054, 0057, 0060, 0064, 0055:**
  "amended by ADR-0078" notes at the sections named in the Amends line.
- **Invariant index:** C-60 sub-invariant 1 corrected (the DS sees
  `push_gid` since ADR-0054 and stores a peppered binding since ADR-0057);
  C-56 / C-57 unaffected; a new C-entry per section as each phase lands.
- **Known gaps:** one G-entry per open finding (or one umbrella entry with
  the sixteen findings and the two found while verifying), each closed by
  its phase; class (c) as a standing gap.
- **Invariant #5 text** (CLAUDE.md, policy - the maintainer's edit): extend
  it to say "neither at rest nor in requests a service processes", and name
  class (c) as out of reach of server-side changes.
- **`docs/PRODUCT-READINESS.md`:** a privacy row stating what is true today.

## Open questions (decision points for the maintainer)

1. **ADR-0073 continuity after a key reset.** Peers reach a reset account
   through a mailbox derivable from account id and generation. Under
   section 1 the new mailbox address needs another carrier: the in-group
   reset announcement, or accept that the continuity mailbox is linkable
   during its window and retire it after.
2. **Fresh-account signal for calls.** A separate issuer key family for
   young accounts makes the throttle precise again at the cost of one bit at
   spend; or keep one family and rely on quota alone.
3. **Welcome delay bound `T`.**
4. **Per-group reach keys:** one key per class (unlinkable, weak revocation)
   or per scope (revocable, labelled).
5. **Log enforcement:** a source lint, a runtime capture test, or both.
6. **ADR-0055:** withdraw the single subscription socket, or keep it and
   accept the per-connection journal set until the class (c) phase.
7. **R.2 on a signal only** (section 7): whether an in-group announcement
   replaces the scheduled identity check, or a rare randomised check stays.
8. **How many of sections 1 to 11 must land before any build reaches users**
   (a release gate like ADR-0046), or whether phase 1 alone is the gate.

## References

- RFC 9420 (MLS), sections 6 and 12.4.3.1
- RFC 9458 (Oblivious HTTP)
- RFC 9576 / 9578 (Privacy Pass), as used by ADR-0054
- Signal, "Sealed sender" (2018), for the sender-hiding precedent
