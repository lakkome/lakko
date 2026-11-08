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

Aligned the same day on the three clients: a contact request's
`from_pseudonym` is 32 random bytes (no client matches an ack against it; an
ack is paired with its request by the accepter's key); a `/c/` link grant is
checked by one core call, `lakko_calls::link_invite::verified_link_grant_mailbox_pk`
(`CallInviteCapability.verifiedLinkGrantMailboxPk`), against the owner's
account key from the one-time KeyPackage (iOS also refuses such a link on its
landing screen; web and Android at accept); and the ADR-0073 continuity reach
key is no longer registered on the resetting account's mailbox, with its
marker and wipe step, since no peer can address that mailbox. The continuity
Welcome stays a recorded no-op until the open question is decided.

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

#### Phase 2 as built (2026-10-16)

Built in the core, `services/auth` (migrations `0034`, `0035`),
`services/sfu`, both FFI surfaces and the three clients; host-tested only,
not deployed, no call placed against it.

- **Room.** `lakko_sfu_proto::room_id(call_id)` =
  `BLAKE3-derive-key("lakko/sfu/room/v1", call_id)`, 32 bytes, sent as 64
  lower-case hex; golden-pinned (`084b368a...` for the call id `00..0f`). On
  UniFFI and wasm as `sfuRoomForCall`.
- **Token.** `lakko-call-jwt` claims are exactly `iss`, `aud`, `room`,
  `peer_id` (32 lower-case hex, random per token), `role`, `iat`, `exp`, and
  the claim set is closed (`deny_unknown_fields`): a token carrying `sub` or
  `call_id` does not decode. `mint_test_jwt` takes `--peer-id` and derives
  the room from `--call-id`.
- **Auth.** `call-join` is `TokenContext` wire byte 12. `POST /call/jwt`
  has no session: body `{room, role}` (anything else refused before a token
  is spent), then one `call-join` token from `x-lakko-anon-token` (the
  shared gate over the spent-set migration `0034`, byte for byte the shared
  crate's schema), then the room cap, then the mint. The answer is
  `{jwt, expires_at_unix_secs, turn}`. The issuance quota is 100 a day, and
  8 for an account younger than a day (`LAKKO_ANON_TOKEN_QUOTA_CALL_JOIN`,
  `LAKKO_ANON_TOKEN_QUOTA_CALL_JOIN_YOUNG`,
  `LAKKO_AUTH_YOUNG_ACCOUNT_AGE_SECS`); all three chosen, not measured. The
  token is spent before the cap is asked, so a refused admission still
  costs it.
- **Room cap.** In auth, in memory: at most 100 admissions per room per 60
  s (`LAKKO_AUTH_ROOM_ADMISSION_LIMIT`, `0` disables;
  `LAKKO_AUTH_ROOM_ADMISSION_WINDOW_SECS`); over it, `429
  room-admission-limit`. Chosen, not measured. It is per auth process: a
  restart forgets it and two instances do not share it. Migration `0035`
  drops `call_admission_events` and `throttled_calls`; `throttle.rs` is
  gone.
- **TURN.** `POST /call/turn-credential` is gone. A credential rides the
  `/call/jwt` answer with a username tag of 16 random bytes. A one-to-one
  call, which uses no SFU, mints a call token for its own call's room only
  to obtain the credential, so it too spends a `call-join` token.
- **SFU.** `Peer` is `{id, room, outbound}`; the peer id is the token's
  `peer_id` and a second live socket with the same id is refused. `Welcome`
  carries `peer_id`, `room`, the roster (each entry the peer id alone) and
  `server_ts`; `PeerJoined` carries the peer id. `peer_tag`,
  `account_pseudonym` and the account prefix in logs are removed; the
  admission and roster log lines name neither the room nor a peer, while
  the media loop's logs still carry the random peer id and the room (left
  to section 11, phase 8). `GET
  /v1/rooms/{room}` answers with no token and `400` for anything that is
  not 64 lower-case hex; `GET /v1/calls/{call_id}` is gone. nginx routes the
  new path in both configurations.
- **Relay.** `lakko_calls::relay`: `RelayKey` =
  `BLAKE3-derive-key("lakko/call/relay/v1", call_seed)`; a sealed payload is
  `version(0x01) || nonce(12) || ChaCha20-Poly1305` with AAD
  `"lakko/call/relay/v1\0" || version`, plaintext at most 1024 bytes. A peer
  announcement is `"lakko/call/peer-announce/v1\0" || 0x01 || call_id(16) ||
  peer_id_len || peer_id || signer_key(32) || sig(64)`, the signature over
  `"lakko/call/peer-announce/sig/v1\0" || call_id || peer_id_len ||
  peer_id`. `verify_peer_announcement` requires the call id, the relay
  `from` to equal the announced peer id, the signer to be a leaf of the
  group, the signature, and an account binding on that leaf (ADR-0050); it
  returns the leaf, its key and the account key. The camera media state is
  sealed under the same key; an unsealed one is no longer accepted. Fuzz
  `lakko-calls/relay_open`. FFI: `CallRelayKey`,
  `classifyCallRelayPlaintext`, `composeCallPeerAnnouncement`,
  `verifyCallPeerAnnouncement` on UniFFI (refusals as the appended
  `FfiError::CallRelay`) and wasm.
- **Clients.** Each keeps the current and the previous epoch's relay key,
  announces itself after the Welcome, on every rekey, when a peer joins and
  every 15 s (chosen, not measured: an announcement sealed under a key a
  peer does not hold yet is lost to it), and names a tile only from a
  verified announcement; the peer-tag resolvers are deleted (web
  `peer-tag-map.ts`, iOS `GroupCallPeerTags.swift`, Android
  `PeerTagLabels.kt`). The probe computes the room and sends no token. In a
  link call the committer election binds a peer to a member only from its
  announcement or from the KeyPackage the committer admitted, no longer
  from whoever relayed an MLS message; a member's sealed profile names a
  peer only when that peer announced the leaf that sealed it; and its
  account claim with the username certificate is kept only for a profile
  the owner sealed naming the capability's owner account
  (`boundProfileClaim`), since no server ties a joiner to an account any
  more, so another joiner's username reads as claimed, never verified.
- **Residuals.** A joiner holds no group before its Welcome, so before it
  it cannot see whether the owner is present. Until a peer's announcement
  arrives its tile is unnamed. The SFU still learns the set of peer ids in
  a room and their timing (class (c)). Nothing was run against a deployed
  SFU, auth or TURN.
- **Amended 2026-11-07 (maintainer): a name for the call.** Any participant
  may change the name the others see on their tile, for this call only; the
  profile name does not change (`docs/ux/calls.md` section 4.1). The name is
  a fourth relay payload, `lakko_calls::call_name`:
  `"lakko/call/name/v1\0" || 0x01 || call_id(16) || peer_id_len ||
  peer_id || seq(8 BE, >= 1) || name_len || name(0..=255, empty =
  withdrawn; otherwise the ADR-0048 display-name rule as it stands) ||
  signer_key(32) || signature(64)`, signed by the sender's MLS leaf key over
  its own domain (`lakko/call/name/sig/v1\0`), sealed under the relay key
  like the announcement. `verify_call_name` takes the relay `from` AND the
  leaf key that peer's verified announcement named (the screen-share
  shape), so an SFU colluding with one member cannot put that member's name
  on another member's tile. A sender numbers its names per peer id and
  re-sends the current one after every announcement, so a late joiner
  learns it; a receiver keeps the greatest `seq` per peer
  (`call_name_is_newer`, strictly greater). No server sees the name, and
  nothing is stored: invariants #2 and #5 are unchanged. Fuzz:
  `lakko-calls/relay_open` now also drives `decode_call_name`. FFI:
  `composeCallName`, `verifyCallName`, `callNameIsNewer` and the
  `CallName` relay kind (appended to the UniFFI `CallRelayPayload`) on
  UniFFI and wasm. Built in the core, both FFI surfaces and web; iOS and
  Android not yet. A one-to-one call (peer to peer, no relay) carries no
  call name: its carrier would be a call-signalling body and is not built.
- **Amended 2026-11-07 (maintainer): a chat inside a group call.** People in
  a group call (one placed in a conversation, and a link call) write text to
  each other for as long as the call lasts; only those in the call when a
  message is sent see it, and nothing is kept (`docs/ux/calls.md` section
  4.1.4). The message is a fifth relay payload, `lakko_calls::call_chat`:
  `"lakko/call/chat/v1\0" || 0x01 || call_id(16) || peer_id_len ||
  peer_id || seq(8 BE, >= 1) || text_len(2 BE) || text(1..=800 bytes UTF-8,
  not white space only, no control character but LF and TAB) ||
  signer_key(32) || signature(64)`, signed by the sender's MLS leaf key over
  its own domain (`lakko/call/chat/sig/v1\0`), sealed under the relay key.
  `verify_call_chat` takes the relay `from` and the leaf key of that peer's
  verified announcement, as `verify_call_name` does; a receiver shows a
  message once per `(peer id, seq)`. The longest message (64-byte peer id,
  800-byte text) is 1007 bytes, inside the relay's 1024. The server forwards
  ciphertext and keeps nothing: invariants #2 and #5 are unchanged. Fuzz:
  `lakko-calls/relay_open` also drives `decode_call_chat`. FFI:
  `composeCallChat`, `verifyCallChat`, `checkCallChatText`,
  `clampCallChatText`, `callChatMaxTextBytes` and the `CallChat` relay kind
  (appended to the UniFFI `CallRelayPayload`) on UniFFI and wasm. A
  one-to-one call has no chat.

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

#### Phase 3 as built (2026-10-16)

Built in `lakko-contacts` (`push_routing.rs` derivations, new `push_wake.rs`),
`lakko-ds-proof` (the `push-binding` statement and its kind byte 6 retired;
the byte is refused), `services/push-gw` (migration
`0006_sealed_subscriptions.sql`), `services/ds` (migration
`0011_drop_push_bindings.sql`), `services/auth` (the `push-register` quota),
UniFFI and wasm (`push_wake.rs`; the NSE surface is unchanged, see below) and
the three clients including their background paths (web service worker, iOS
NSE and VoIP handler, Android FCM service). Host-tested only; nothing
deployed or walked.

- **Derivations.** `derive_push_wake_signing_seed` = keyed-BLAKE3(secret,
  `lakko/push/wake-sign/v1`) as an Ed25519 seed; `derive_push_wake_unlock_key`
  = keyed-BLAKE3(secret, `lakko/push/wake-unlock/v1`); both golden-pinned in
  the crate and on both FFI surfaces (`derivePushWakePublicKey`,
  `derivePushWakeUnlockKey`).
- **The sealed wake** is `version(1) = 0x01 ‖ ephemeral X25519 key(32) ‖
  ChaCha20-Poly1305` over a 185-byte plaintext `push_gid(32) ‖ kind(1) ‖
  nonce(16) ‖ timestamp_ms(8 BE) ‖ unlock_key(32) ‖ wake_pk(32) ‖ sig(64)`,
  234 bytes, sent as 312 base64url characters. The box is the
  `lakko-contacts::envelope` construction under its own labels
  (`lakko/push-wake/v1/key`, `/nonce`, AAD = domain ‖ version). The signature
  is by the wake key over a domain-separated encoding of the other fields, so
  a wake names exactly one group, kind, time and nonce. `open_push_wake` is
  fail-closed and fuzzed (`lakko-contacts/fuzz` target `push_wake_open`).
- **push-gw's key** is generated at first boot and stored in its own
  database (`push_wake_key`), not taken from the environment (decided here:
  a key in the env file would be one more secret to provision and rotate by
  hand, and the database already holds everything push-gw needs). Published
  at `GET /push/wake-key`. Clients pin it at build time
  (`VITE_LAKKO_PUSH_WAKE_KEY`, Info.plist `LakkoPushWakeKey`,
  `BuildConfig.LAKKO_PUSH_WAKE_KEY`) or fetch it once per process; a wake
  that cannot be sealed is simply not sent (the message is still delivered
  on the next poll).
- **A subscription row** is `(sub_id, push_gid, wake_pk, sealed)`, `sealed`
  = ChaCha20-Poly1305 under the unlock key over a list of the device's
  tokens `(provider, token)`, zero-padded to 256-byte steps (AAD binds the
  `push_gid` and the `sub_id`). It carries a LIST because an iOS device holds
  two tokens (alert and PushKit) and push-gw's delivery rule (a call goes to
  the call token when there is one) needs the whole set. `sub_id` is 16
  random bytes the device keeps per `(account, push_gid)` beside its route
  row; nothing else names the device. `POST /push/subscribe` upserts by
  `sub_id`; `POST /push/unsubscribe` deletes by `sub_id`; both spend one
  `push-register` token.
- **Delivery.** The DS forwards the header blob unread (it checks only the
  version byte and the length, and caps wakes per journal). push-gw opens
  the wake, verifies the signature, refuses a timestamp more than 10 minutes
  off its clock and a nonce it has seen (an in-memory set, so a restart
  re-admits a wake replayed within the skew window), caps wakes per
  `push_gid`, selects the rows whose `(push_gid, wake_pk)` match, opens each
  with the unlock key and sends. A token the provider reports invalid is
  removed by re-sealing the row without it (or deleting the row when it was
  the last), conditional on the row not having changed meanwhile. The
  delivered payload carries the subscription id as `s` instead of the device
  pseudonym `d`; every background path handles a wake only when the bound
  account's route row for `g` holds that `s`.
- **Per-device wakes removed.** `/push/register` and `/push/unregister` are
  gone with `push_tokens`: nothing used a device-addressed wake, and keeping
  one would have kept the device pseudonym. The raw-group-id fallback of the
  pre-ADR-0042 wake is removed on every client.
- **Re-subscribe.** A token change (APNs, PushKit, FCM, Web Push
  subscription change), a sign-in and a second account signing in on the
  same device send every subscription of the signed-in account again (the
  previous account's rows are removed first). auth's default `push-register`
  quota goes from 100 to 400 a day for that reason (chosen, not measured).
  At sign-out the subscription ids are read before the route rows are wiped
  and every row is removed by id.
- **The NSE surface is unchanged.** The extension never seals or opens a
  wake; it reads `s` from the payload and checks it against the route row the
  app wrote, in Swift.
- **Residuals.** The class (b) residual above stands, and three more are
  specific to this design: (1) a live DS and push-gw that compare notes can
  link a journal to its `push_gid` by timing (a forwarded wake arrives at
  push-gw the moment the publish lands); (2) an evicted member keeps the
  routing secret until the next rotation (ADR-0043) and can sign wakes for
  the group until then, rate-capped by push-gw; (3) an operator who is also a
  member of a group can decrypt that group's subscription rows (it holds the
  unlock key), so it learns the tokens of that group's members; (4) a device
  re-sending all its subscriptions after a token change is one burst of
  writes from one address, a class (c) link.
- **Found while building:** the Android FCM handler required a `v` key that
  the FCM provider never sent, so no FCM wake parsed on Android before this
  change; the check is gone.

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

#### Phase 6 as built (2026-10-16)

Built in `lakko-mls`, `lakko-contacts`, `lakko-attest`, both FFI surfaces,
`services/ds`, `services/auth` and `services/discovery` (tests and docs
only there) and web / iOS / Android, together with G-50 (the channel
PublisherGroup commit record, which touches the same group). Host-tested;
not walked live, not deployed. Indexed as C-79 (ids) and C-82 (G-50).

- **DeviceGroup id.** `DeviceGroup::group_id_for(&account_seed)` =
  `BLAKE3-derive-key("lakko/device-group/id/v2", account_seed)`, golden-pinned;
  `create_founder` and `load` take the seed, `load_by_group_id` opens a group
  joined by Welcome under its own id, and `join_from_welcome` binds no id (it
  cannot: a fresh device does not hold the seed yet). The seed every client
  passes is the **lineage seed**: the oldest own predecessor after an ADR-0074
  rotation, else the current account seed. A rotation therefore keeps the
  DeviceGroup and its journal, and the rotation's root move is a plain
  `root` transfer from the old root to the new one. An ADR-0073 key reset
  clears the own predecessors (web already did; iOS and Android now do), so
  the new seed names a new journal, which the resetting device founds and
  claims with an ordinary claim. FFI: `deviceGroupIdFor(accountIdentity)`,
  `loadDeviceGroup(identity, accountId, accountIdentity)`,
  `loadDeviceGroupById`, `DeviceGroup.createFounder(identity, accountId,
  accountIdentity)`.
- **The key-reset attestation lost its journal id** (`lakko-attest`
  `KeyResetAttestation` version 2, 145 bytes, version 1 refused). Decided
  while building, not in the ADR: auth mints it, and naming the journal
  would need auth to learn the seed-derived id, which is exactly what this
  section takes away. With it went the DS's attested reset arm, the ADR-0074
  root endorsement (`sign_ds_root_endorsement`, the `root_endorsement_b64`
  field, both client implementations) and `ds_group_owner.key_generation`
  (migration `0012`); the DS no longer depends on `lakko-attest` or reads
  `LAKKO_DS_ATTEST_KEY`. The attested root transfer of a rotation became a
  plain transfer. Discovery's key-change rule is unchanged.
- **PublisherGroup id.** `publisher_group_id_for(owner_account_seed,
  channel_id)` = `BLAKE3-derive-key("lakko/pubgrp/id/v3", owner_account_seed ‖
  channel_id)`. Decided while building: the input is the owner's account seed
  (the one the channel provider runs on: the lineage seed above), not the
  journal root seed the section names, because the journal root seed is
  itself derived from the journal id, which would be circular. `create` /
  `load` / `open_or_create` derive it from the identity they are given;
  `load_joined(identity, group_id, channel_id)` opens a co-published group
  under the id the link conveyed and never creates. `channel_id_from_group_id`
  and `channelIdFromPublisherGroupId` are removed.
- **The co-publisher link.** The `/g/` capability's sealed preview has a
  second kind (`lakko-contacts` preview kind 2): a channel preview carrying
  the 16-byte channel id and an optional channel name, never an inviter or a
  member count. `composeCoPublisherInviteCapability(seed, groupId, expiry,
  channelId, channelName)` mints it; the joiner opens the preview under the
  link secret it already holds, so the co-publisher needs nothing else from
  the owner: the group id is in the capability, the channel id in the
  preview, and the joiner records the channel -> group pair (web the channel
  log, iOS and Android the per-account co-published marks) to reopen the
  group later. Tests: the channel-preview round trip, a damaged channel
  preview refused, a channel preview with an inviter or a count refused; the
  existing `group_invite_preview_decode` fuzz target reaches the new branch.
- **Tests the phasing asks for** (invariant #3): proptests
  `group_id_is_not_computable_from_the_account_id` and
  `group_id_is_not_computable_from_the_channel_id` (the id an identifier
  alone gives, by the retired derivation, never matches; two different seeds
  never give the same id; a PublisherGroup id does not contain the channel
  id), goldens for both, and
  `publisher_group_ids_do_not_collide_with_other_group_ids` in
  `tests/group_id_width.rs`.
- **Found while building, on iOS:** the channel PublisherGroup provider was
  never durable (each screen founded a fresh group); it now is
  (`ChannelPublisherIdentity`), see G-50.

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

#### Phase 8 (sections 8 to 11) as built (2026-10-16)

Built in the core, the services, both FFI surfaces and the three clients;
host-tested only, not deployed, not walked live.

- **Tokens (section 8).** All three pools refill on a schedule that does
  not look at spending: when a session is bound each context is topped up to
  its target once at a random moment within 20 s, then at independent random
  intervals of 30 to 90 minutes (web `anon-token-pool.ts`, iOS
  `AnonTokenPool.swift`, Android `AnonTokenPool.kt`, the same numbers on all
  three). Targets per context, chosen, not measured: `discovery-lookup` 16,
  `push-register` 8, `ds-knock` 8, `vault-claim` 8, `invite-consume` 4,
  `channel-subscribe` 4, `directory-search` 4, `blob-upload` 4, `blob-read`
  4, `mailbox-write` 4, `discovery-register` 2; the daily quotas in
  `services/auth` are above them. A take never issues: an empty context
  waits only for a refill the schedule runs (this session's first, or one in
  flight), else fails with the network copy until the next scheduled refill.
  The one exception is the detached take a sign-out's push unregister makes
  after the session is gone (one token, never pooled), kept because the
  session it was issued to is the one ending. Issuance and spending travel
  on separate connections: on web the issuance is a credentialed `fetch`
  and every spend `credentials: "omit"` (the browser keeps them in separate
  connection pools; a page cannot do more); on iOS the issuance rides the
  bearer `URLSession` and every spend an ephemeral one; on Android the
  issuer has its own Ktor client and every spend goes out on `AnonymousHttp`.
  `services/auth` was already pruning `anon_token_issuance` below the
  previous epoch (checked, no change).
- **Reach keys (section 9).** `lakko_contacts::reach`: every class but
  `link` and `reply` is account-wide, so a `group`, `member` or `channel`
  derivation with a scope is refused (`ScopeIdNotEmpty`); flag day, the
  per-scope keys are no longer derivable. The clients register one key per
  class (`member` while in any group, `group` while the setting allows any
  joined group, `channel` while it allows any channel) and rotate:
  `group` + `member` when a group shrinks (kept) and when the account leaves
  or is removed from a group, `channel` when it leaves a channel, `group` +
  `channel` when the setting is changed on this device (a setting adopted
  from another device arrives with that device's counters). A departure is
  a set difference between the groups / channels at the last registration
  and now, an empty current list read as "not loaded yet" (web
  `departuresOf`, iOS `ReachRules.departures`, Android `reachDeparted`). A
  rotation republishes the profile in every remaining group (and, on the
  next upload, the channel member card); a refused write is retried when the
  new grant arrives (the existing grant-kept hook). The `channel` key no
  longer follows a channel's seed epoch. Version recovery after a restore
  reads the account-wide slots only.
- **Ledgers (section 10).** `services/discovery` migration
  `0031_ownerless_invite_ledgers.sql` drops and recreates the three ledgers
  and their consumer tables (flag day): a row is `blob_id`, the sealed
  capability, `revocation_pk BINARY(32)`, expiry, `max_uses`, `used_count`,
  `created_at`; no `owner_account_id`, no `issuer_pubkey`, and also no
  `label` and no `bootstrap_referral` (both existed only for the owner list,
  which is gone; decided while building, not by this ADR). Upload takes
  `revocation_pubkey`; a revoke verifies against it. `POST
  /discovery/invite/by-owner` is deleted, and with it the owner-list signer
  on both FFI surfaces (`signOwnerListQuery`); `linkRevocationPublicKey(seed)`
  is new on both. Account deletion no longer deletes invites by owner. Each
  client mints a random 32-byte seed per link and keeps it beside the link
  secret (web IndexedDB `minted-links`, iOS Keychain `OwnLinkStore`,
  Android EncryptedSharedPreferences `OwnLinkStore`); the "Invite links"
  list is built from those records, so it shows the links minted on this
  device and states each one's limit instead of the uses left (decided while
  building: nothing on the server counts uses for an owner any more). Every
  revoke, the ADR-0074 rule 5 spend after a key rotation included, signs
  with the link's own seed; a link this device has no record of is another
  device's to revoke. Deleting the account revokes every live recorded link
  before `DELETE /account`.
- **Logs (section 11).** New crate `lakko-log-hygiene`: `request_span`
  (every service's `TraceLayer` records the method and the route template
  from `MatchedPath`, never the URI), a source lint (`tests/source_lint.rs`
  walks `services/*/src` and the two shared server crates and fails on any
  info, warn or error macro or span naming a denied field component, and on
  a `TraceLayer` built with its default span), and, behind the `capture`
  feature, a capture subscriber with an identifier scanner (hex of 32
  characters or more, unpadded base64 of 16 or 32 bytes, UUIDs, 16- or
  32-element byte arrays) that a test asserts empty against a reviewed
  allow-list. Capture tests drive the account and link routes in
  `services/discovery` and the journal, knock and mailbox routes in
  `services/ds`; both allow-lists are empty. CI: `just log-lint` in `just
  ci`, and a `log-hygiene` job in `.github/workflows/ci.yml` running the
  lint and both capture tests against MariaDB. Every violation the lint
  found was fixed (auth, discovery, push-gw, storage, sfu); the SFU names a
  peer in its logs by a per-process 24-bit tag (`PeerId::log_tag`) that
  does not survive a restart, and no longer logs call ids.

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
