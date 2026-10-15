<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0078: Lakko's servers cannot link accounts: sealed senders, anonymous admission, unlinkable ids

- **Status:** Proposed (2026-10-14). The maintainer decided seven of the
  eight decision points on 2026-10-15 (recorded in "Decisions" at the end
  and folded into the sections); the eighth, how a person reaches their
  contacts, groups and history after losing the recovery phrase, stays
  open, and with it a new requirement the maintainer stated that day: the
  backup must stay available to its owner after the phrase is lost.
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
- **Related:** [ADR-0055](0055-websocket-delivery.md) (proposed; kept by
  decision 6, its per-connection journal set accepted as a class (c)
  residual, section 12), [ADR-0065](0065-attachment-lifetime-and-access.md)
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
  address; this is the open question at the end. (ii) An open (Everyone) mailbox takes
  proofless writes today; under sealed sender nothing about the writer is
  visible, so such writes should spend an anonymous token (new context).

#### Phase 1 as designed (2026-10-15)

A recon of the tree on 2026-10-15 found the mailbox side worse than the
inventory above: the claim route `PUT /ds/mailbox/{hex}/access` takes no
proof and stores the identity key in the clear (`ds_mailbox_access.identity_pk`);
the request carries the sender's identity key and account id outside its
seal; the ack (185 bytes) is not sealed at all; a Welcome is a bare MLS
message behind a clear kind byte (`0x01` request, `0x02` Welcome, `0x03` ack,
defined in each client, not in Rust); device mailboxes are still claimed but
no client reads them; and no background reader (the iOS NSE, Android's
background decrypt, the web service worker) touches a mailbox. The call
Welcome ferry and the KeyPackage forward ride the SFU signal relay, not a
mailbox, and are out of this phase. The maintainer decided three points the
same day (marked).

- **The mailbox key.** `mailbox_seed = BLAKE3-derive-key("lakko/mailbox/v1",
  account_seed)`, the account seed being the 32-byte input the reach seeds
  take. The Ed25519 key pair from that seed signs the claim, every read and
  every reach-key registration; the X25519 key a writer seals to is its
  birational conversion (`lakko-crypto` `to_x25519_secret` /
  `to_x25519_public`, already used for channel subscribers). No generation
  input: the account seed changes on an ADR-0073 reset and an ADR-0074
  rotation, and the mailbox with it. Every device of the account derives
  the same key.
- **The address.** `BLAKE3-derive-key("lakko/mailbox/address/v1",
  mailbox_pk)`. The claim body is `{mailbox_pk_b64}`; the DS checks that the
  key hashes to the address (first claim wins, as now) and stores
  `mailbox_pk`. No column, route or log line of the DS holds the identity
  key any more.
- **The binding (decided: an identity-key signature).** A writer has to
  learn that a mailbox key belongs to the person it means to reach, else a
  member of a shared group could hand out its own mailbox under that
  person's name and swallow their Welcomes and requests. The binding is
  `version(1) ‖ mailbox_pk(32) ‖ sig(64)` = 97 bytes, the signature by the
  account identity key over `"lakko/mailbox/binding/v1\0" ‖ mailbox_pk`,
  and a writer verifies it against the account key it holds pinned before it
  writes. It travels with every mailbox key, whatever the carrier.
- **The reach grant grows by the binding.** `class(1) ‖ version(4 BE) ‖
  seed(32) ‖ binding(97)` = 134 bytes (was 37), in every carrier: the
  contact request's reply grant, the profile envelope, the `/i/` and `/c/`
  capabilities, the join attribution, the introduction record. The ADR-0060
  public record of a person in Everyone carries the binding instead of the
  identity-derived address. A committer delegation names the added
  account's mailbox address and carries its binding, verified against the
  added account's key in the roster (it named `mailbox_pseudonym(account key)`).
- **The mailbox envelope.** Every mailbox row is `version(1)=0x01 ‖
  eph_x25519_pk(32) ‖ AEAD(plaintext)` built with the `lakko-contacts`
  sealed-box construction, AAD `"lakko/mailbox-envelope/v1\0" ‖ version ‖
  address`. The plaintext is `kind(1) ‖ body_len(4 BE) ‖ body ‖ zeros`,
  padded to a size bucket (powers of two from 1 KiB to 64 KiB, then 64 KiB
  steps up to the DS message cap; chosen, not measured). Kinds: `0x01`
  contact request, `0x02` Welcome, `0x03` contact ack, `0x04` Welcome
  rendezvous (the co-publisher join). An unknown kind is dropped. The inner
  request and ack wires are unchanged, so their signature checks do not
  move; the request keeps its own inner seal.
- **Writes to an open mailbox (decided: an anonymous token).** A write with
  no reach proof, to a mailbox that is open (Everyone) or not yet claimed,
  spends one token of a new context `mailbox-write` (wire byte 11). A write
  with a reach proof spends none, as now.
- **Device mailboxes are removed.** No client claims or reads one.
- **The reply-key pool is moved to phase 5 (decided)** with the Welcome
  delay: both are timing defences. Until it lands the DS can still match a
  reply-key registration to the request row it precedes by time, but no
  longer reads either row.

#### Phase 1, the Rust half as built (2026-10-15)

Built in `lakko-contacts` (`mailbox.rs`, the 134-byte reach grant in every
carrier, introduction payload v3 and committer-delegation envelope v2, each
carrying the mailbox binding), `lakko-ds-proof` (no identity-derived
mailbox left), `lakko-anon-token` (`mailbox-write`, wire byte 11; auth's
default quota 50 a day, `LAKKO_ANON_TOKEN_QUOTA_MAILBOX_WRITE`, chosen, not
measured), `services/ds` (migration `0010_sealed_mailbox.sql`), `services/discovery`
(migrations `0029`, `0030`: wider public-record and sealed-link columns) and
the three FFI surfaces; host-tested only, no client built against it yet.
Two points differ from the design above:

- **A proofless write to a closed mailbox spends a token too,** before the
  silent drop. Otherwise a writer could tell a closed mailbox from an open
  one by whether its token came back unspent.
- **The DS refuses a mailbox POST that is not a well-formed row** (version
  byte and a bucket-sized length) with `400`, before reading any state, so
  the refusal reveals nothing and spends no token.

A row is its bucket plus 49 bytes (smallest 1073); the largest body is
983035 bytes. The core verifies the binding where it holds the key to
verify against (the `/i/` capability's owner, the request's reply grant
against `from_pubkey`, the introduction and public-record payload against
its `identity_pk`); the other carriers leave it to the client: the `/c/`
link grant, the join-attribution reply grant, profile grants (against the
sender's account key resolved at receipt), and every write under a stored
grant.

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
  id. `call_admission_events` is dropped. **Decided (2):** one issuer key
  family for every account, quota only. A separate family for young
  accounts was rejected: it shows "young" at spend, beside the room, and
  while few accounts are young that bit nearly names the joiner. Cost
  accepted: the per-room cap must admit genuine large calls, so fresh
  accounts can fill a room up to it, and ADR-0033's "end the call on a
  burst of new accounts" goes; account creation stays rationed by the one
  signup proof of ADR-0054.
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
  not told apart by size. **Decided (3):** `T` is a few seconds, and clients
  also write **dummy Welcomes**: sealed rows of Welcome size, indistinguishable
  from a real one on the DS, which the recipient drops after opening. A delay
  of seconds alone blurs little when traffic is light; the dummies are what
  makes "a Welcome-sized row arrived" stop meaning "someone joined a group".
  How often a client writes one, and to which mailboxes, is not designed yet:
  a dummy needs a mailbox the client may write to (it holds a reach grant for
  it), and the rate costs DS storage and bandwidth. The constants are chosen,
  not measured, and are tuned against live traffic.
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
- **Decided (7):** the ADR-0073 R.2 scheduled identity check is replaced by
  a **broadcast of resets** that every client downloads alike, so no
  request names an account. A plain list does not scale: at a billion
  accounts and an assumed (not measured) 1% resetting per year, about
  27 000 entries a day at about 1 KB each with an inclusion proof (some 30
  hashes of 32 bytes) is tens of MB a day per client. The broadcast is
  therefore a **compact filter per period** (a binary fuse / xor filter over
  `account_username_hash(account_id)` of every entry flagged `is_reset` in
  that period, roughly 9 bits an entry, so tens of KB a day under the same
  assumption). The filter's hash is committed in the signed, witnessed root,
  so a server cannot drop a reset for one client without forking the log
  (ADR-0020 / ADR-0039). A client checks each roster person it knows against
  the filter locally and fetches the one bundle only on a hit (a real reset
  or a rare false positive), so the server learns interest in an account only
  when that account reset. The lazy R.2 triggers stay (opening a one-to-one,
  before a send in one). Needs a `services/transparency` route serving the
  filter for a period; a client back after a long absence downloads one
  filter per missed period.
- **The primary path, once the open question is settled:** the person who
  reset carries the proof. If the backup survives the reset (open question
  at the end), the reset device writes to each contact and knocks on each
  group itself, and puts its `key_reset_attest`, the KT inclusion proof and
  the witnessed root inside the sealed message; the peer verifies them
  locally and asks no server anything. The filter then remains as the
  fallback for a peer that never receives such a message.
- The in-group announcement first proposed here was rejected: a key changes
  only through a reset, a reset happens only when the person has lost
  everything, and such a person is in no chat to announce anything.
- Bucketing the reset list by hash prefix was rejected: a client's set of
  buckets is stable from day to day and fingerprints its contact list.
  Keyword PIR scales without a broadcast but is heavy on the server; it is
  kept as a later option.
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
  every remaining group). **Decided (4):** one key per class. The client
  rotates the class key automatically when the person leaves a group or
  changes the ADR-0060 setting, and republishes the profile in every
  remaining group. Messages inside groups travel through group journals and
  are not affected; until a member receives the new profile, its write to
  the mailbox (a contact request, a Welcome for a new device of the person)
  is refused and retried when the profile arrives. The person's own devices
  hold the account seed and derive the current key themselves.

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
  on any 16- or 32-byte identifier in output. **Decided (5):** both. The
  capture layer carries an allow-list for values that are not identifiers
  (hashes and nonces in diagnostics), reviewed like code.
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
  subscribing to every journal of a device hands the DS the device's
  journal set per connection. **Decided (6):** ADR-0055 is kept and this is
  accepted as a class (c) residual: polling from one address already lets
  the DS infer the same set, and the socket turns that inference into a
  fact inside the DS process. The network-phase ADR has to address it
  (subscriptions on separate connections through a relay, or another
  shape), and nothing in the DS may persist a connection's journal set. SFU media exposes every
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
or request no longer carries the link. **Decided (8):** phases 1 to 8
(sections 1 to 11) are a release gate in the sense of ADR-0046: no build
reaches users while any of them is open. Class (c) (section 12) is a later
phase with its own ADR, and until it lands the product documents state
plainly that an operator watching its own network edge can still link
devices by address and timing. The gate sits beside G-39, and these phases
should land before the external review so it examines the final shape.

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
5. **Welcome timing (section 5) and the reply-key pool (section 1, moved here 2026-10-15).** Clients. Tests: a contact request uses a reply key registered earlier from the pool; the Welcome send path
   applies the delay; size buckets are equal across kinds; a dummy Welcome
   is indistinguishable on the DS from a real one and is dropped by the
   recipient without effect.
6. **Deterministic ids (section 6).** `lakko-mls` (`DeviceGroup::group_id_for`,
   `publisher_group_id_bytes`); invariant #3 applies: property tests. Test:
   the id is not computable from the account id or channel id alone.
7. **Discovery sweep (section 7).** Clients, `services/transparency` (the
   reset-broadcast route). Tests: no scheduled caller of the by-id discovery
   routes remains; the filter request names no account; a filter whose hash
   is not in the witnessed root is refused.
8. **Tokens, reach keys, ledgers, logs (sections 8 to 11).** Tests: no
   ledger table has an owner or issuer column; the log lint and the capture
   test (decision 5) are both in CI; the token pool refills on a timer, not
   on a low-water mark; leaving a group rotates the class reach key and
   republishes the profile.

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

## Decisions (2026-10-15, maintainer)

Seven of the eight decision points of the first draft, as answered; each is
also written into its section above.

2. **Fresh-account signal for calls:** one issuer key family, quota only
   (section 2).
3. **Welcome delay:** a few seconds, plus dummy Welcomes (section 5).
4. **Reach keys:** one per class, rotated automatically on leaving a group
   or changing the setting (section 9).
5. **Log enforcement:** both the source lint and the runtime capture test
   (section 11).
6. **ADR-0055:** kept; the per-connection journal set is accepted as a class
   (c) residual (section 12).
7. **R.2:** a per-period compact filter of resets every client downloads
   alike, committed in the witnessed root, plus the lazy triggers; once the
   open question is settled the reset person carries the proof and the
   filter is the fallback (section 7).
8. **Release gate:** sections 1 to 11; class (c) later, stated in the
   product documents (Phasing).

## Open question: losing the phrase must not lose the conversations

The first draft asked how peers reach a person after an ADR-0073 key reset,
since under section 1 they can no longer compute the new mailbox address
from the account id. The options it carried were a continuity mailbox
derivable from account id and generation (linkable to the account for its
window) or no automatic return at all.

The maintainer rejected the premise on 2026-10-15: **the backup must stay
available to its owner after the recovery phrase is lost**, so that a
contact, group or chat that was quiet for a long time but matters is not
lost with the phrase. Today it can be: the backup envelope key is
`argon2id_kek(phrase, salt)` (ADR-0072 fact 3) and the ADR-0064 vault keys
derive from the account seed, which comes from the phrase. A mail-protected
account (ADR-0075, the default: no phrase saved) gets the phrase back from
the email vault, at the cost of the stated exception to invariant #2. The
gap is for the other accounts: a phrase-protected account whose owner lost
the phrase, every device and a key-carrying passkey, and an account with
neither a saved phrase nor an email that lost its devices and passkey. Such
a person can only reset (ADR-0073) and keeps none of their contacts,
groups, channels or history.

If the backup survives the reset, the reach question answers itself: the
reset device reads the contact list, its reach grants and the group ids
from the backup, writes to each contact and knocks on each group itself,
and no mailbox derivable from the account id is needed.

What opens the backup without the phrase is not decided. Three candidates
were put to the maintainer and none was accepted:

- a synced passkey enabled by default (ADR-0072 decision 6, opt-in today);
  the operator learns nothing, but it rests on Apple / Google passkey sync
  and fails a person who lost that account too;
- a second wrap of the backup key under a key derived from the OPAQUE
  export key (the password); the server never sees the key, but whoever
  holds the auth database can guess the password offline at the cost of
  one Argon2id per guess, so the backup is only as strong as the password;
- the same password wrap over the social part only (contacts, groups,
  channels, history keys, no identity seed), so a cracked password exposes
  the conversations but cannot become the person.

Three more were put the same day and rejected too: trusted contacts holding
shares of the backup key with the person's PIN as a required extra share
("guardians do not work in the real world"), a server-side enclave with a
guess counter in the manner of Signal's SVR, and a password hardened by an
OPRF split across independent operators. Handing history over from peers on
re-admission was put beside them and is not chosen either.

The constraint any answer has to meet: to open the backup without the
phrase, the person must still hold something the operator does not, in
their head, at a third party, with other people, or in hardware the
operator cannot open; anything the person still holds that the operator
also holds lets the operator open the backup.

An email address cannot be a key factor without the server holding the key.
Whatever is chosen must keep invariant #2 (no key material the server can
use) and invariant #5 (the server must not read the contact list), amends
ADR-0072 and ADR-0073, and is designed in its own ADR before this one is
accepted.

## References

- RFC 9420 (MLS), sections 6 and 12.4.3.1
- RFC 9458 (Oblivious HTTP)
- RFC 9576 / 9578 (Privacy Pass), as used by ADR-0054
- Signal, "Sealed sender" (2018), for the sender-hiding precedent
