<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0098: Reporting, blocking and moderation

> Numbered ADR-0089 until the merge with `main` on 2026-11-20, which had taken
> 0089 for the camera background. The merge also moved this ADR's invariants
> C-90..C-92 to C-95..C-97, its auth migrations `0037`-`0039` to `0042`-`0044`,
> channels `0013` to `0014`, storage `0004` to `0005`, the `report` token
> context byte 16 to 17, the block register tag 20 to 23, the moderation
> service port 8094 to 8100, auth's internal listener 8095 to 8101, and the
> discovery and storage moderation listeners' bind variables to
> `LAKKO_DISCOVERY_MODERATION_BIND` / `LAKKO_STORAGE_MODERATION_BIND`. The text
> below uses the new numbers.

- **Status:** Accepted (2026-11-08). The maintainer decided:
  - reports are anonymous;
  - clients honour a suspension;
  - moderator keys sit under an offline root;
  - every action is published in a separate, witnessed moderation log;
  - the legal side is deferred.

  Steps 1 to 5 are built and host-tested only (see "Step 1 as built" to
  "Step 5 as built").

  Later the same day the maintainer also chose:
  - a sender signature on every chat message (section 1);
  - the reason only as a salted commitment in the log (section 4);
  - target and reason in the clear in a report (section 2);
  - collapsed group messages from a blocked person (section 6).

  The open questions are not decided.
- **Date:** 2026-11-08
- **Context:**
  - UX spec [`docs/ux/reporting-and-moderation.md`](../ux/reporting-and-moderation.md);
  - the existing channel takedown route
    ([ADR-0059](0059-channel-discover.md) section 7,
    `services/channels/src/internal.rs`);
  - anonymous tokens ([ADR-0054](0054-anti-abuse-without-client-work.md));
  - servers cannot link accounts
    ([ADR-0078](0078-servers-cannot-link-accounts.md));
  - per-device leaves and their account binding
    ([ADR-0050](0050-per-device-conversation-leaves.md));
  - the KT log and its witnesses
    ([ADR-0019](0019-client-authorised-log-leaf.md),
    [ADR-0020](0020-witness-cosigning.md),
    [ADR-0039](0039-kt-log-witness-quorum.md)).

## Context

Today Lakko has no block, no report and no moderator. The only moderation
surface is the channel listing takedown of ADR-0059 section 7. It sits on an
internal listener behind one shared secret, and nothing records who used it
or when.

Moderation under end-to-end encryption can act on three things only:

1. **What a person chooses to show.** Moderators read only what a reporter
   includes. A report is worth something only if the moderator can check
   that the reported account really wrote it. Otherwise any account can be
   framed with made-up text.
2. **What the server already holds in the clear for everyone:**
   - accounts at `services/auth`;
   - usernames and `@names` at `services/discovery`;
   - public channel listings and posts at `services/channels`;
   - blobs at `services/storage`, readable by whoever holds the key.
3. **What clients do on their own.** Clients block, leave and remove, and
   they show a suspension.

A suspension at auth cannot take an account out of a conversation it is
already in. The Delivery Service admits a write by a journal key proof
(ADR-0053) and knows no account (ADR-0078). This limit is stated in the UX
spec, not hidden.

Three facts, checked in the source, shape the design:

- **OpenMLS 0.8.1 does not expose a message's signature after decryption.**
  `ProcessedMessage` (`openmls-0.8.1/src/framing/validation.rs:276`) carries
  the sender, the credential and the content. The signature accessor on the
  authenticated content (`framing/mls_auth_content.rs:227`) is `pub(crate)`.
  The signature covers the serialized `GroupContext` of its epoch
  (`mls_auth_content.rs:96-112`). Using it as evidence would need a fork of
  OpenMLS. It would also need the reporter to hand over the epoch's group
  context, which carries the tree and transcript hashes.
- **The witness co-signature names no log.** `lakko-transparency`
  `witness.rs` signs `lakko/transparency/witness/v1 ‖ version ‖ tree_root ‖
  log_size`, and `services/witness` keeps one high-water row
  (`witness_state`). A second log co-signed by the same witnesses would let
  a co-signature from one log be presented as one from the other.
- **An access token is checked statelessly outside auth** (`lakko-auth-token`
  `v3`; ADR-0073 section 3). If a suspended account could still refresh, its
  access tokens would keep working at discovery and storage.

## Decision

Six parts, one per surface:

1. Each chat message carries a signature by the sender's device, which a
   report hands over as evidence.
2. Anonymous reports are sealed to the moderators.
3. Moderator keys are security-key credentials, listed under an offline
   root.
4. Each service applies a moderator-signed action itself.
5. A separate moderation log, witnessed, with a periodic snapshot that
   clients query by prefix.
6. Blocking is a synced register on the client.

### 1. Message attestation: evidence a moderator can check

The chat envelope (`lakko-attachments`) gains a sender signature. The
plaintext of an MLS application message that carries a chat envelope
becomes:

```
"lakko/attested-message/v1\0" ‖ version(1) ‖ sig(64) ‖ envelope
```

- `sig` is an Ed25519 signature by the sending device's conversation leaf
  key (the ADR-0050 `DeviceIdentity`) over:

  ```
  "lakko/attested-message/sig/v1\0" ‖ group_id_len(1) ‖ group_id ‖ BLAKE3(envelope)
  ```

- `envelope` is the existing JSON envelope, byte for byte.
- `group_id` binds the message to its conversation, so a message cannot be
  presented as one sent in a different conversation.

**What signs and what doesn't.**

- Signed: chat messages, edits, deletes and reactions (every `MessageBody`
  except typing and read receipts).
- Not signed: service envelopes (`lakko/...`) and typing and read signals.
  They are never evidence.
- Contact requests are already signed by the requester's identity key.
  Channel posts are already signed by the publisher (`lakko-channels`
  `post.rs`). Neither changes.

**The receiver checks it at receipt.** The receiver verifies the signature
against the MLS sender leaf it resolves at receipt (C-68).

- A message whose signature fails is shown as today and recorded as
  unattested.
- An unattested message cannot be included in a report with the mark
  `Written by this account` (UX section 5.3).
- A missing or failed signature drops nothing. It is a decision about
  evidence, not about delivery.

This check runs at receipt so that what the reporter saw and what the
moderator later checks are the same bytes.

**What the evidence carries.** For each message in a report:

- the attested plaintext;
- the group id;
- the sender's leaf signature key;
- the sender leaf's ADR-0050 device binding (the `BasicCredential` identity
  bytes, signed by the account key).

The moderator checks four things:

1. The signature under the leaf key.
2. The binding names the leaf key, and its signature verifies under the
   account key.
3. The account key is the case's target.
4. The attachment references inside the signed envelope match the blobs
   fetched with them. A blob id is the SHA-256 of its ciphertext.

**What the evidence does not prove.** It does not prove that the message
was delivered, when it was sent, or in what order. The envelope's own time
is the author's claim.

**Deniability.** A message is already signed inside MLS. This adds a
signature that can be checked without the group context. It makes a
message easier to attribute outside the group, which is the point. Stated
as a cost under Consequences.

**Cost per message:** 64 bytes, one Ed25519 sign, one verify per receiver.

This is a wire change of the chat plaintext, made as a flag day (no
deployment exists). It does not touch the `lakko-mls` state machine; the
sender leaf key and the binding are already read there.

### 2. Reports

**New service `services/moderation`** (`lakko-svc-moderation`).

- `POST /moderation/report` takes no session. It spends one anonymous token
  of a new context, `report` (wire byte 17; 16 before the merge with main), through the shared gate. auth's
  issuance quota for `report` is 10 per account per day (chosen, not
  measured). The gate's `429` and an unconfigured service's `503` are what
  the client shows as its "limit reached" and "not available" states.
- The body has a clear part and a sealed part.

**The clear part:**

- `target_kind`: account, group handle, channel or name;
- `target`:
  - an account: BLAKE3 over `lakko/moderation/account-target/v1` and the
    account key;
  - a channel: the channel id;
  - a handle: its OPRF lookup key, as discovery stores it;
- the reason code (section 2.1);
- `conversation_kind`;
- `evidence_count`.

The target and the reason are in the clear so the queue can group, count
and rank cases without decrypting everything. The service learns that some
token holder reported this target for this reason at this time. It does not
learn who.

**The sealed part:** the comment, the evidence of section 1, and attachment
references (blob id, key, nonce). It is sealed to the current moderator
list (section 3):

- one random content key;
- the content AEAD-sealed under it;
- the content key wrapped to each listed moderator's X25519 key with the
  `lakko-contacts` sealed box under labels of its own.

A server compromise therefore yields the clear part only. The bound on the
sealed part is 256 KiB (chosen).

**The receipt.** The response carries a random 16-byte `receipt_id`. The
client keeps it in its local `Your reports` list.

`GET /moderation/report/{receipt_id}` (no session, under the per-IP limit)
answers `received`, `action` or `none`. It does not say what action or
against whom. One report's receipt cannot be tied to another's except by
timing and IP, which ADR-0078 class (c) leaves open.

**Attachments.** The moderation service fetches a blob through a storage
internal route and decrypts it in the moderator's browser, never on the
server. While a case is open it keeps the blob alive (ADR-0065 `keep`).

The reporter's client does not re-upload content. Child-safety evidence
needs a legal answer before it is handled differently (UX section 9).

#### 2.1 Reason codes

A closed list, frozen bytes, defined once in a new crate `lakko-moderation`
and shared by the client sheet, the report format, the panel, the log and
the suspended person's screen:

| Code | Byte |
|---|---|
| spam | 1 |
| harassment | 2 |
| hate | 3 |
| violence | 4 |
| sexual | 5 |
| child-safety | 6 |
| impersonation | 7 |
| other | 8 |

An unknown byte is refused.

### 3. Moderator keys

- **The root.** An Ed25519 root key is generated and kept offline by
  `tools/moderation-root`, the `tools/release-manifest` model (ADR-0038).
  Clients and services pin its public key at build time
  (`LakkoModerationRootKey`, `LAKKO_MODERATION_ROOT_KEY`,
  `VITE_LAKKO_MODERATION_ROOT_KEY`, `LAKKO_*_MODERATION_ROOT_KEY` per
  service). Unpinned, the client shows no suspension and the services
  refuse every action. Nothing about moderation works without a pin.
- **The moderator list.** The root signs a list `version ‖ issued_at ‖ count
  ‖ entries`, domain `lakko/moderation/moderators/v1`. Each entry holds:
  - a moderator id;
  - a display name (panel-only);
  - a WebAuthn credential public key (COSE: ES256 or EdDSA) with its
    credential id and the panel's RP id;
  - an X25519 public key for report sealing;
  - added-at;
  - an optional revoked-at.

  The list is monotone by `version`, and a service refuses a lower one. It
  is served by the moderation service and appended to the moderation log
  (section 5), so a split list is a log fork.
- **Signing.** A moderator signs with their security key through WebAuthn:
  - The challenge is `SHA-256("lakko/moderation/action/v1\0" ‖ statement)`.
  - A verifier checks the assertion over `authenticatorData ‖
    SHA-256(clientDataJSON)` with the listed COSE key, the challenge, the
    RP id, the origin, and the user-presence and user-verification flags.
  - The verifier is a minimal one in `lakko-moderation`, not the vendored
    `webauthn-rs-core`. That fork is pinned to auth's ceremonies (ADR-0014),
    and a service only needs to verify an assertion against a fixed key.
- **Decrypting reports.** The X25519 private key is derived in the browser
  from the security key's WebAuthn PRF output (HKDF, info
  `lakko/moderation/report-key/v1`). It never leaves the browser and is
  never stored. A security key without PRF cannot be a moderator.
- **Adding or removing a moderator** is a new signed list made offline.
  Reports sealed before a removal stay readable by the removed key. Their
  content is already disclosed to that person.

### 4. Actions, applied by each service

A moderator decision in the panel produces one statement:

```
"lakko/moderation/statement/v1\0" ‖ version ‖ statement_id(16) ‖ action(1)
‖ target_kind(1) ‖ target ‖ reason_commitment(32)
‖ until_unix_ms(8, 0 = no end) ‖ issued_at_unix_ms(8) ‖ moderator_id(16)
‖ list_version(8)
```

It is signed through WebAuthn (section 3). The moderation service:

1. appends it to the moderation log (section 5);
2. then sends it, with the assertion and the log index, to the internal
   listener of the service that applies it.

That service verifies the statement itself against the pinned root and the
moderator list. A compromised moderation service therefore cannot forge an
action.

The ADR-0059 shared-secret route is replaced by this. The shared secret
stays only as transport authentication between hosts.

| Action | Applied by | Effect |
|---|---|---|
| suspend / lift | `auth` | see below |
| take back username | `discovery` | Deletes the name, its terms and the public record. The name is held for the suspension or 180 days (chosen), unclaimable by anyone |
| hide from search | `discovery` | Removes the account's Everyone record and terms. The account keeps its name |
| channel delist / block handle / remove | `channels` | `remove` and `block` as ADR-0059 section 7. `remove` serves `410 {"code":"channel-removed","log_index":n}` on every channel route, the ADR-0087 tombstone with a different code |
| group: remove from search / block handle | `discovery` | ADR-0087 handle row: delisted, or refused for claim |
| blob delete | `storage` | Deletes a blob named in evidence, by id. No proof of ownership needed under a moderator statement |

**Suspension at auth.**

- A new table `account_suspensions` (migration):
  - `account_id`;
  - `until` (NULL = no end);
  - `reason`, `reason_salt`;
  - `statement_id`, `log_index`;
  - `appeal_state` (none, sent, declined) with the sealed appeal.

  The account is found by `accounts.identity_pubkey` from the account
  target. A suspension follows the account, not the key.
- **Sign-in.** Every route that lands a session refuses a suspended account
  only **after** it has authenticated (login, recovery login, password
  reset, passkey, TOTP, device redeem, recall):
  - it answers `403 {"error":"account-suspended", ...}`;
  - it issues a **restricted session**, a new credential kind `s1` in
    `lakko-auth-token` that stateless verifiers refuse by version.

  An unauthenticated caller cannot use the refusal to tell a suspended
  account from a wrong password. The refresh route does the same for a
  suspended account's `r2`.
- **What a restricted session can do.** auth accepts `s1` on
  `GET /account/suspension`, `POST /account/suspension/appeal`,
  `DELETE /account` (with its usual grants) and `POST /auth/session/logout`.
  Every other route refuses it.
- **Tokens and keys.** Token issuance refuses a suspended account. So do
  `POST /account/keys/reset/*` and the anonymous recall routes, so a
  suspended account cannot reset its way out.
- **Appeal.** The appeal text is sealed to the moderator list like a
  report and kept with the suspension row. The panel reads it through
  auth's internal listener.

**The reason stays private.** The signed statement, which is what the log
holds, carries only `reason_commitment = BLAKE3-derive-key(reason ‖
reason_salt(16))`. The reason and the salt travel beside the statement to
the applying service, which checks them against the commitment. The suspended person sees the reason from auth (UX
section 4). Anyone else learns only that the account was suspended, which
is what clients show. Publishing reasons per account would attach "child
safety" or "hate" to a key that every contact can recognise, and a wrong
decision would make that permanent. The commitment lets an audit or an
appeal open it later. The transparency numbers count reasons in aggregate.

### 5. The moderation log and the suspension snapshot

- **The log** is a second instance of `services/transparency`'s log code,
  in a database of its own, with its own operator key. It has a log id,
  `lakko/moderation-log/v1`.
  - A leaf is a section-4 statement, or a moderator list, or a snapshot
    root, together with its WebAuthn assertion. It is never a report's
    content and never a reporter.
  - Appends come only from the moderation service.
  - Roots are co-signed by the same witnesses as the KT log.
- **Witnesses need one change.** The co-signature domain becomes
  `lakko/transparency/witness/v2`, carrying a log id, and the witness keeps
  one high-water row per log id. This is a flag day for the KT-log
  co-signature too. It is the only change to the witness service, and it
  touches C-57.
- **Clients check suspensions without a lookup per contact.** Once a day
  (`SNAPSHOT_PERIOD`, chosen) the moderation service builds a snapshot of
  every active suspension and removal:
  - Entries are sorted by target, split into 65,536 buckets by the first two
    bytes of the target. Each entry is `target ‖ until ‖ log_index`.
  - The snapshot carries a Merkle tree over the buckets.
  - Its root is appended to the log, so it is witnessed.

  A client that needs an account's standing asks
  `GET /moderation/snapshot/{period}/bucket/{prefix}` with no session.

  - The server learns a 16-bit prefix, not the account.
  - The answer is the bucket, its Merkle path to the snapshot root, and
    the root's log entry with the witness co-signatures.
  - A client caches a bucket per snapshot period. Its cost grows with the
    accounts it displays per day, not with its contacts.

  At 10 million active suspensions a bucket holds about 150 entries of 48
  bytes plus a 512-byte path. That is arithmetic on an assumed count, not a
  measurement.
- **When a client asks.** On opening a one-to-one or a card, a contact
  request's arrival, a search result, an invite link's issuer, and a group
  member list. A hit shows the UX section 3 marks. A missing, unverifiable
  or stale bucket shows nothing. A suspension is shown only if proven, and
  failing open toward no mark is safe: the mark is a warning, not access
  control.
- **What the snapshot does not prove:** that it contains every suspension
  the log contains. Anyone can rebuild a snapshot from the public log and
  compare roots, so a dropped entry is detectable after the fact, not by
  the client at the time.

### 6. Blocking (client only)

- A `lakko-sync` register keyed on the blocked account's key:
  `Blocked { at_ms }` / `Unblocked { at_ms }`. HLC last-writer-wins, carried
  as a DeviceGroup service envelope `lakko/sync/blocked/v1\0` and in the
  growth snapshot and the backup.
- Every receive path consults it, as the UX spec section 1.2 lists. Blocking
  also performs `Remove contact`, so the contact key rotates (ADR-0060
  section 1, 2026-11-06) and their way into this account's inbox closes.
- One shared rule, `lakko_ui_rules::block_verdict(kind)`, decides
  drop / collapse / show per path, and `InfoSection` gains `Block`,
  `Unblock` and `Report`. No server change.

### 7. The panel

- **App.** `apps/moderation`, a SolidJS app using the web client's tokens
  and components, built separately. It is served by the moderation service
  on an internal hostname behind mTLS client certificates (the `/metrics`
  pattern) and is never linked from the product.
- **Backend routes** (`/panel/*`), behind the moderator's WebAuthn sign-in.
  - Read: the case queue (grouped by target), the sealed reports, the
    account age band and current standing (from auth's internal listener),
    the log.
  - Write: decisions (section 4) and case locks ("in review by").
- **The browser does the cryptography.** It decrypts reports, checks
  evidence (section 1), opens blobs and signs statements. The panel backend
  holds no key that decrypts or signs.

### 8. What is deferred to the legal phase

As UX section 9. This ADR keeps the evidence format and the reason codes
open to the obligations that phase may add, and records nothing it would
have to delete.

## Alternatives

- **Fork OpenMLS to expose the signature.** Rejected:
  - the evidence would need the group context of its epoch (tree hash,
    transcript hash);
  - the verifier would have to rebuild `FramedContentTBS`;
  - a second vendored crypto fork joins `webauthn-rs-core`.

  The application signature costs 64 bytes and keeps `lakko-mls`
  untouched.
- **Server-side franking (Facebook's scheme: a commitment per message,
  tagged by the server).** Rejected. The Delivery Service knows no sender
  (ADR-0078), so a server tag proves delivery to a journal, not authorship.
  Authorship still needs a sender signature.
- **Reports with a session.** Rejected by the maintainer (anonymous). A
  token gives the same per-account daily bound without naming the account.
- **Target sealed too.** Rejected for now. The queue could not group or
  count cases without decrypting every report, and at scale that is the
  moderators' browser doing all the work. The clear target reveals that
  someone reported an account, which the log reveals anyway once an action
  follows.
- **One shared moderation secret (the ADR-0059 route, widened).** Rejected:
  - anyone with server access could act;
  - nothing would record which person decided.
- **Moderator keys held by the moderation service.** Rejected. A server
  compromise could then suspend anyone, and decrypt every report.
- **Clients fetch the whole log, or a filter of all suspensions per
  period.** Rejected:
  - suspensions of spam accounts arrive in volumes no phone should
    download daily;
  - a filter still costs megabytes a day at that volume.

  The bucket query costs a few kilobytes per displayed account per day.
- **Reason in the public log.** Rejected as above. Recorded because the
  earlier discussion assumed it.
- **Block as a server-side feature (refuse their writes at the DS).**
  Rejected. The DS knows no accounts (ADR-0078), and a server-side block
  list would be a social graph on the server (invariant #5).

## Consequences

- **Positive:**
  - A report's messages can be checked as written by the reported account.
    Forged reports are recognisable.
  - Reporters are not named to the server or to moderators.
  - No server alone can suspend, take back a name or remove a channel; a
    moderator's security key is needed for each.
  - Every action is in a witnessed public log. A client never shows a
    suspension the log does not hold, and a lift is a new entry, never a
    deletion.
  - The ADR-0059 shared-secret takedown gets an author and a record.
- **Negative:**
  - Every chat message carries a signature that can be checked outside the
    group. A recipient can prove to a third party that the sender wrote it,
    without the group's cooperation.
  - The clear report target tells the server which accounts are reported
    and when.
  - Suspension does not remove an account from its existing conversations.
  - A bucket query reveals a 16-bit prefix and the time, per displayed
    account; repeated queries from one IP narrow it.
  - Witness co-signature format change (flag day on the KT log too).
  - One more service, one more log, one more pinned key on every client.
  - The moderator role depends on security keys with PRF.
- **Neutral:**
  - Invariant #2 holds: the report's content is sealed to moderators; the
    server holds the clear part only.
  - Invariant #5 holds: no block list or contact relation on any server.
  - Invariant #3: no `lakko-mls` state-machine change; the attestation
    rides the application plaintext.

  New invariant-index entries when built:
  - C-95: a report's evidence verifies to the target account or is shown
    as not verified;
  - C-96: no service applies an action without a moderator signature under
    the pinned root;
  - C-97: no client shows a suspension without log inclusion.

  G-39's surface grows by the attestation, the report seal and the WebAuthn
  verifier. `lakko-security` should review before merge (auth, transport of
  reports, witness format).

## Open questions

1. **Retention.** How long a report's sealed part, a closed case and an
   appeal are kept. This is legal-phase.
2. **The rest of the numbers.** The quota of 10 reports a day, the 256 KiB
   bound, the 20-message cap and the snapshot period are chosen, not
   measured.
3. **Reports against a group without a handle.** Nothing on any server
   names such a group. The report can only feed actions against its
   members' accounts.
4. **A key reset after a suspension.** auth refuses it. A recall
   (ADR-0074) that runs while a suspension exists is also refused. This
   needs a check that a hostile reset cannot be used to have the real owner
   suspended in the attacker's place.
5. **Panel hosting** on the single VPS (ADR infra) versus a separate host
   is an ops decision.
6. **A suspension that misses its account.** Auth finds the account by the
   identity key it recorded at the genesis mint, which the client asserts.
   Unless the KT log requires the auth-attested genesis (ADR-0036, off by
   default), an account can record one key at auth and publish another, and
   a Suspend keyed on the published key finds no account. Requiring the
   genesis attestation, or keying the suspension on something auth already
   binds, is undecided.

## Build order

Step 1 is built (2026-11-08, host-tested only). Step 2 is built
(2026-11-08, host-tested only); see "Step 2 as built". Step 3 is built
(2026-11-19 and 20, host-tested only); see "Step 3 as built". Step 4 is
built (2026-11-20, host-tested only); see "Step 4 as built". Step 5 is
built (2026-11-20, host-tested; the panel walked once against a local
moderation service with a software key); see "Step 5 as built".

1. `lakko-moderation` (reason codes, statement, moderator list, WebAuthn
   assertion verifier, report seal) and the message attestation in
   `lakko-attachments`, both FFI surfaces, fuzz targets.
2. The `report` token context; `services/moderation` intake and receipt;
   the client report sheet and block on web, desktop, iOS and Android.
3. The witness v2 format; the moderation log; auth suspension (`s1`,
   migrations); the suspended-owner screen and appeal on the clients.
4. Snapshot and bucket query; the client suspension marks.
5. The panel; discovery, channels and storage action routes; retire the
   ADR-0059 shared secret as authority.

## References

- UX spec [`docs/ux/reporting-and-moderation.md`](../ux/reporting-and-moderation.md)
- ADR-0014, ADR-0020, ADR-0039, ADR-0050, ADR-0053, ADR-0054, ADR-0059,
  ADR-0060, ADR-0065, ADR-0078, ADR-0087
- RFC 9420 (MLS) section 6.1, `FramedContentTBS`
- W3C WebAuthn Level 3, the PRF extension
- Grubbs, Lu, Ristenpart, *Message Franking via Committing Authenticated
  Encryption* (CRYPTO 2017)

## Step 1 as built (2026-11-08)

Host-tested only. No client, service or panel calls any of it yet; nothing
was walked or deployed. `cargo-fuzz` was not installed, so the fuzz targets
were built (`cargo check`) and not run; a proptest drives their entry points
on random and domain-prefixed bytes instead.

- **`lakko-attachments::attested`.** `attest(leaf_seed, expected_leaf_key,
  group_id, envelope)`, `split`, `verify`, `is_attested` and
  `parse_attested_body(plaintext, group_id, signer_key) -> (MessageBody,
  Attestation)` with `Attestation::{Verified, Failed, Absent}`. A wrapper
  whose framing is malformed classifies as a service envelope (dropped)
  with `Failed`; a well-formed wrapper with a bad signature is shown and
  `Failed`. A service envelope or an already attested plaintext is refused
  for signing. The wrapper opens with `lakko/`, so a parser that does not
  know it drops it as a service envelope, never shows it as text: until the
  clients call `unwrapAttested`, no client may send attested messages.
  11 tests, two of them proptests; fuzz `attested_message_open`.
- **`lakko-moderation`**, a new crate (`lakko-crypto`, `lakko-attachments`,
  `serde_json`, `base64`, `p256`; `p256` 0.13 was already in the lock file
  through openmls, so no new crate entered it):
  - `reason`, `target` (`TargetKind` with exact lengths: account 32, group
    handle 64, channel 16, name 64, blob 32; `account_target_hash`);
  - `seal`: `seal_for_moderators` / `open_for_moderator` with a `Purpose`
    (report, appeal) in every derivation, `moderator_report_secret(prf)`;
  - `report`: `ReportClear`, `ReportContent`, `EvidenceItem` (message,
    contact request, channel post, listing), `seal_report`, `split_sealed`,
    `open_report` (refuses a clear evidence count that differs from the
    content's);
  - `moderators`: the root-signed list, canonical-form check on decode,
    `authorised_at(id, at_ms)`, `recipients()`;
  - `webauthn`: `verify_assertion` for Ed25519 and P-256 (DER signatures),
    checking type, challenge, origin, `crossOrigin`, the RP id hash and the
    user-presence and user-verification flags;
  - `statement`: `Statement` (14 actions, each with one target kind;
    `until` only on a suspension and after the issue time), `challenge`,
    `reason_commitment`, `SignedStatement::verify(list)`;
  - `evidence`: `verify_message_evidence(item, resolve)`, with the ADR-0050
    resolver passed in so the crate does not link `lakko-mls`.

  The reason is not in the signed statement (section 4 was corrected): only
  its commitment is. 29 unit tests and one end-to-end test
  (`tests/report_end_to_end.rs`: a bound device sends an attested message
  in a real MLS group, the recipient reports it, the moderator opens the
  report and checks it was written by the sender's account). Fuzz
  `moderation_decode`, `report_open`, `signed_statement_verify`.
- **`lakko-mls`**: one read-only accessor, `Group::device_binding_at(leaf)`
  (the leaf's credential identity bytes). No state-machine change.
- **FFI.** UniFFI `moderation.rs`: `attestMessage(identity, groupId,
  envelope)`, `unwrapAttested`, `messageAttestation -> FfiAttestation`,
  `moderationAccountTarget`, `reportReasonCodes`, `reportReasonName`,
  `sealModerationReport(..., evidence: [FfiEvidenceItem], moderatorList,
  rootKey)`, `Group.deviceBindingAt`, and `FfiError::Moderation` appended
  last. wasm `moderation.rs`: the same client functions with a
  `ReportBuilder` class, plus the panel's `verifyModeratorList`,
  `moderatorReportPublicKey`, `openModerationReport -> OpenedReport`
  (`evidenceAuthor`, `evidenceIsByTarget` with `lakko-mls`'s resolver),
  `buildModerationStatement`, `encodeSignedStatement`, and
  `Group.deviceBindingAt`. The NSE surface is unchanged.
- **Changed from the text above while building:**
  - the UX switch "Include photos and files" is removed: attachment keys sit
    inside the signed envelope, so leaving them out would break the
    signature;
  - the moderator list carries the panel's RP id and origin once, not per
    entry, and moderator keys in raw form (32-byte Ed25519 or 65-byte
    uncompressed P-256), so no verifier parses CBOR;
  - contact-request, channel-post and listing evidence are carried but not
    yet checked; they report "not checked" in the panel.


## Step 2 as built (2026-11-08)

Host-tested only. Nothing was deployed, no report has crossed a network.

- **Token context.** `lakko-anon-token` `TokenContext::Report` (wire byte
  16, name `report`); auth's issuance quota 10 a day
  (`LAKKO_ANON_TOKEN_QUOTA_REPORT`); listed by `anonTokenContexts()` on both
  FFI surfaces.
- **`services/moderation`** (`lakko-svc-moderation`, lib + bin,
  `127.0.0.1:8093`, moved to `8094` in step 3 and to `8100` at the merge with main): `GET /moderation/moderators` serves the signed list
  verbatim; `POST /moderation/report` checks the body before spending the
  token (a well-formed report, not a blob target, sealed to a list version
  the service knows and to at least one moderator authorised now: else
  `400 bad-report` / `bad-target` / `unknown-list-version` /
  `no-current-moderator`), then spends one `report` token through the shared
  gate, then stores the row and answers `201 {"receipt_id"}`;
  `GET /moderation/report/{receipt_id}` answers `received` / `action` /
  `none` or `404 unknown-receipt`. The list and its root key come from
  `LAKKO_MODERATION_LIST_PATH` and `LAKKO_MODERATION_ROOT_KEY`, verified at
  startup (a list that does not verify, or only one of the two set, refuses
  startup; neither set answers `503 reporting-unavailable`; a new list takes
  a restart). The row (`moderation_reports`, migration `0001`) keeps the clear
  part, the list version, the whole report and the day of arrival, not the
  time; the spent-set is migration `0002`, byte for byte the shared one. A
  `ReportStore` trait with a MariaDB and an in-memory implementation (the
  `services/channels` pattern). Both report routes are behind an in-memory
  per-network limit (IPv4 address, IPv6 /64; burst 30, one every 2 s,
  100 000 networks, chosen, not measured) that also refuses a request with no
  source address. Nothing logs a target, a receipt or an address. 9 route
  tests over the in-memory store.
- **`tools/moderation-root`** (`lakko-moderation-root`): `keygen` (the root
  seed, mode `0600`), `sign-list` (a JSON file of moderators to the signed
  list, checked back), and `dev-moderator`, a test-stack moderator whose
  report key is a random file instead of a security key's PRF output (never
  for a real deployment). `just gen-moderation-root`,
  `just sign-moderator-list`.
- **Infrastructure** (written, not deployed): `infra/systemd/lakko-svc-moderation.service`,
  the target, nginx `/api/moderation/` and `api.lakko.me/moderation/` with
  access and error logs off (a report has no session; the log would tie the
  reporter's address to the report and its receipt), body limit 300 KiB, the
  test stack's `/api/moderation/` and `/native/moderation/`, its database and
  env line, the deploy scripts' service lists, `lakko.env.example`.
- **The block register** (section 6, built differently in one respect):
  `lakko-sync::synced::blocked`, one register per blocked account key
  (`Blocked` / `Unblocked` and the time), entry tag 23 (20 before the merge with main), envelope
  `lakko/sync/blocked/v1\0 || 0x01 || count(1..=16) || count x (hlc ||
  account_key(32) || state(1: 1 unblocked, 2 blocked) || changed_at_ms)`,
  canonical and fail-closed; on an exact stamp tie a block beats an unblock.
  It is NOT in the growth snapshot: it travels in its own envelope and is
  re-sent on growth, the way the device-acknowledgement register is; it is
  in the persisted state and therefore in the backup. `set_blocked`,
  `is_blocked`, `blocked_accounts`, `blocked_envelopes`,
  `apply_blocked_envelope`; on both FFI surfaces; driven by the
  `synced_state_decode` fuzz target.
- **The shared rules** (`lakko-ui-rules`): `block::block_verdict(path,
  blocked)` over eight paths (one-to-one message, group message, contact
  request, incoming call, call tile, notification, outgoing signal, incoming
  signal) to show / drop / collapse / decline silently / label / withhold;
  `InfoSection::{Block, Unblock, Report}` appended; `PersonInfoCapabilities`
  gains `is_blocked` (a blocked person's card offers no Call and no Add
  contact), `GroupInfoCapabilities` gains `has_handle`. A group is reported
  only by its `@name` (open question 3: nothing else names a group), by
  anyone but its owner; a channel by a subscriber; a person from any card of
  a conversation this device is still in.
- **FFI additions.** UniFFI `moderatorListVersion(list, rootKey)` (wasm has
  `verifyModeratorList(...).listVersion`). The NSE surface's
  `parseBodyEnvelope` unwraps an attested message before parsing (no
  signature check: a preview is not evidence), so the iOS notification path
  needs no change.
- **Catalog.** The `block.*` and `report.*` keys of UX section 7 plus
  `report.title`, `report.why`, `report.included_none`,
  `report.select.title`, `report.select.limit`, `report.select.done`, in
  English and machine Russian.

### The clients (2026-11-08)

Built on web (and so the desktop client), iOS and Android; host-tested only
(web vitest 7754, iOS LakkoTests 4957, Android host JVM 5320, the last
without the native core). Never run in a browser, on a device or against a
server.

- **Attestation.** Every chat, edit, delete, reaction, reply, forward, held
  and re-sent message is wrapped with `attestMessage` before the MLS
  encrypt; typing, read receipts, service envelopes, call and channel traffic
  are not. Each receive path unwraps before parsing and keeps, with each
  message from someone else, the verdict, the attested bytes, the sender leaf
  key and the leaf's binding read at receipt (web and Android in the message
  log row; iOS in a separate device-only store of the newest 400 messages per
  conversation, because its message type is rebuilt field by field in many
  places). Background previews unwrap without checking.
- **Block.** Routed like the device-acknowledgement register on all three.
  The background notification paths honour it: web's service worker reads
  the synced row it already reads for mutes, Android's background decrypt
  reads the persisted state, and iOS's NSE reads a list of blocked account
  keys the app mirrors into the App Group (it never opens the synced state).
- **Report.** The sheet, the in-sheet "Choose messages" list (changed from
  the UX draft's timeline select mode on every client; the spec is
  updated), Your reports, Blocked people. A channel-info report picks from the
  channel's locally logged posts (iOS and Android keep each logged post's
  wire for it; web lists the posts of the open channel view, from which its
  channel info opens). A Discover report carries the listing only. A group is
  reported by the OPRF lookup key of its `@name`, obtained the way each
  client looks up a name.

## Step 3 as built (2026-11-19)

Host-tested only. Nothing was deployed; no statement has crossed a network
between two real services.

- **Port fix (a defect of step 2).** `services/moderation` was given
  `127.0.0.1:8093`, which is also the default of the channels service's
  internal listener (ADR-0059 section 7). It was then `127.0.0.1:8094` (`127.0.0.1:8100` since the merge with main) (unit,
  nginx, test stack, justfile, the iOS and Android dev endpoints, and a
  `/api/moderation` entry in the web dev proxy, which step 2 had not added).
- **Witness v2 (section 5).** `lakko_transparency::LogId { Kt = 1,
  Moderation = 2 }` (names `lakko/kt-log/v1`, `lakko/moderation-log/v1`). The
  co-signature signs `lakko/transparency/witness/v2 || 0x02 || name_len ||
  name || tree_root || log_size`. The blob keeps its 105 bytes with version
  byte `0x02`: the log is in the signed input, not in the blob, so a
  co-signature for one log never verifies for the other and the clients'
  length constant stays valid. Version 1 is refused (flag day on the KT log
  too). `sign_witness_cosig`, `verify_witness_cosig` and
  `verify_witness_quorum` take the log; the two FFI functions keep their
  signatures and pass `LogId::Kt` (no client source change, rebuilt
  libraries only). `services/witness`: `POST /witness/cosign` requires
  `"log": "kt" | "moderation"` (else `400`); one high-water row and one lock
  per log (migration `0002`, which drops `witness_state`); the KT operator key
  stays required, `LAKKO_WITNESS_MODERATION_OPERATOR_PUBKEY` is optional and
  a moderation co-sign without it answers `404 unknown-log`.
  `services/transparency` sends `"log": "kt"`. C-57 holds per log.
- **The moderation log (section 5)** lives in `services/moderation`, not in a
  second `services/transparency` process: the KT log's leaves and gates are
  key-binding specific, and what is shared is the Merkle code
  (`lakko_transparency::merkle`, `SignedRoot`). Its own operator key, made
  on first boot (migration `0003`), logged at startup and served at
  `GET /moderation/log/operator-key`. A leaf is `kind || bytes`: 1 a signed
  statement with its assertion, 2 a signed moderator list, 3 reserved for
  the step-4 snapshot root. Appends are serialised and idempotent by a dedup
  key (the statement id, or BLAKE3 of the list); the same statement id with
  other bytes is `409 statement-id-conflict`. The loaded moderator list is
  appended at startup when it is not in the log. Roots are co-signed with
  `LogId::Moderation` by `LAKKO_MODERATION_WITNESS_URLS` /
  `_COSIGN_SECRETS` (the transparency fan-out: best effort, never a stale
  co-signature, each witness proved from its own high-water). Public routes,
  behind the per-network limit: `/moderation/log/root`,
  `/moderation/log/entry/{index}?size=`, `/moderation/log/consistency?from=&to=`.
  The whole log is held in memory and one instance is its only writer.
- **Statement intake.** An internal listener (`LAKKO_MODERATION_INTERNAL_BIND`,
  default `127.0.0.1:8096`, `x-lakko-internal-secret` = base64 of
  `LAKKO_MODERATION_INTERNAL_SECRET`, constant-time; unset: not started)
  serves `POST /internal/statements` with the signed statement, the reason,
  the salt and, for an account action, the account key. It verifies the
  statement against the loaded list, the reason against its commitment and
  the account key against the target, appends to the log, then forwards a
  Suspend or Lift to auth (`LAKKO_MODERATION_AUTH_INTERNAL_URL` + `_SECRET`)
  with the list and the log index; auth's 4xx becomes
  `422 apply-refused` carrying auth's code, an unreachable auth
  `502 apply-failed`; the entry stays in the log either way and a resend
  re-forwards. Every other action is logged and answered `applied: false`
  (its service is step 5). The panel (step 5) will call the same function;
  until then the only signer is the test tool below.
- **`tools/moderation-root dev-sign`** signs a Suspend or Lift with a
  software Ed25519 "security key" (a seed file) and prints the
  `/internal/statements` body; `dev-moderator` now also writes that seed and
  lists its public key. The software authenticator is
  `lakko_moderation::software_authenticator`, behind the non-default
  `software-authenticator` feature (tests and the tool only).
- **The restricted session.** `lakko-auth-token` gains `s1` (the access
  layout, tag under `BLAKE3-derive-key("lakko/suspended-session/v1",
  secret)`, the access TTL); every other verifier refuses it by version and it
  refuses theirs.
- **Suspension at auth.** Migration `0042` (`0037` before the merge with main): `account_suspensions`,
  `moderation_applied_statements` (replay), `moderation_list_state` (the
  newest list version seen), `moderation_account_decisions` (the issue time
  of the newest decision applied per account) and an index on
  `accounts.identity_pubkey`. The internal listener (`LAKKO_AUTH_INTERNAL_BIND`,
  default `127.0.0.1:8101` (`8095` before the merge with main), `LAKKO_AUTH_INTERNAL_SECRET`,
  `LAKKO_AUTH_MODERATION_ROOT_KEY`; either unset: not started) takes
  `POST /internal/moderation/statement` and itself verifies the list under
  the pinned root (an older list is `409 stale-list`), the statement, the
  action (Suspend or Lift only), the reason commitment and the account key.
  Decisions apply in the order moderators made them: a statement older than
  the newest one already applied to an account changes nothing for it
  (`applied: false, superseded: true`), so a Suspend resent after a later
  Lift does not suspend again. A suspension applies to every account whose
  recorded `identity_pubkey` is the key (the column is client-asserted and
  was not unique; an account that recorded someone else's key is only
  suspended alongside). A suspended account:
  - at a landing route (password, recovery login, password reset, passkey,
    TOTP recover, device redeem, the recovery-email password finish) and at
    refresh, after it authenticated and before any change, gets `403
    account-suspended` with an `s1`, an ordinary `r2` (refresh keeps
    answering the 403 until the suspension ends) and, on the password and
    passkey sign-ins, a fresh-auth grant, so a person with no passkey can
    still delete the account;
  - on every other bearer route gets `403 {"error":"account-suspended"}`, and
    on the anonymous recall routes the same after the anchor check;
  - may call with the `s1` only `GET /account/suspension`,
    `POST /account/suspension/appeal`, `DELETE /account`,
    `POST /auth/session/logout`, and `/auth/action/{start,finish}` for the
    delete-account action alone (a passkey account deletes with an action
    grant).
  A device redeem refuses after consuming the one-time grant (the account is
  known only through it). An `s1` left over after a lift, and a suspended
  account's `v3`, keep opening those routes until they expire; the stateless
  verifiers (discovery, storage, gif-proxy) accept a suspended account's
  `v3` for up to the access TTL. An hourly sweep deletes ended rows.
  Token issuance (`POST /auth/tokens`) takes the `s1` too, for the
  `blob-read` context alone (built differently from section 4, which
  refused issuance outright): a suspended person reads their chats, and an
  attachment cannot be opened without a read pass. Every other context
  answers `403 account-suspended`.
  Discovery's `DELETE /discovery/account` and storage's `DELETE /backup`
  accept the `s1` too, and nothing else there does: deleting a suspended
  account must also clear its username, identity bundle, KeyPackages and
  backup. On a device that holds no chats (signed in afresh), the suspended
  screen offers Delete account in place of "Read my chats" (UX section 4).
- **Appeal.** `lakko_moderation::appeal`: `seal_appeal` (trimmed, 1 to 1000
  characters, no control characters but line feed and tab, `Purpose::Appeal`,
  domain `lakko/moderation/appeal/v1\0`, sealed to the list's current
  moderators) and `open_appeal`; `sealModerationAppeal(text, list, rootKey)`
  on UniFFI and wasm. Auth stores the sealed appeal once per suspension
  (`409 appeal-already-sent`), at most 16 KiB. Reading appeals and declining
  one are the panel's (step 5).
- **Tests run:** lakko-transparency 84; lakko-svc-witness 7 (with its DB
  test, both logs); lakko-svc-transparency 37 + 1; lakko-svc-moderation 23
  (with its DB store test); lakko-moderation 36 (38 with the feature);
  lakko-auth-token 23; lakko-svc-auth 218 + 2, the DB-gated ones run against
  a local MariaDB, including 8 suspension tests; the FFI witness and appeal
  tests. Not run: the fuzz targets, the dockerised recovery e2e.

### The clients (2026-11-20)

Built on web (and so the desktop client), iOS and Android; host-tested only
(web vitest 7806, iOS LakkoTests 4998, Android host JVM 5354, the last
without the native core). Never run in a browser with a real suspension, on
a device, or against a server; the web suspended screen was seen in a stub
preview only.

- **Entering and leaving.** A landing's `403 account-suspended` is read whole
  (the body is longer than the clients' error truncation) and lands the
  account in suspended mode with its `s1` and `r2` kept per account; an auth
  bearer route's bare 403 makes the token provider refresh once and take the
  `s1` from the refresh's 403. Neither is a sign-out or a wipe. In suspended
  mode the clients refresh at open, on foreground and every 10 minutes
  (chosen); a normal pair leaves the mode silently.
- **The screen** as UX section 4: the reason from the report sheet's reason
  text, the end as date and time, Appeal (then "Appeal sent" or "declined"),
  "Read my chats" or, on a device without the account's keys, Delete account,
  and Sign out (logout with the `s1`, then the usual local sign-out).
- **Read-only.** Every composer is the muted line; Reply, Forward, Edit,
  Delete, reactions and the shared pin are hidden; calls are not offered
  (no ring, no Join, no call-link Join); adding contacts, members and invite
  links is not offered; the chat-list row reopens the screen. Each client
  also refuses locally every write to the DS and to channels while
  suspended, and storage writes except the file-read pass and the backup
  delete (web and iOS gate storage; Android leaves storage open). Android
  also stops the poll tail (registrar, sweep, beacon) and typing and read
  receipts; iOS withholds signals.
- **Files.** The anonymous-token pool draws `blob-read` alone with the `s1`
  and treats a suspended 403 as "not now".
- **Appeal sheet.** One field, 1,000 characters with a counter in the
  locale's digits, `sealModerationAppeal` to the verified moderator list,
  sent once; "already sent" counts as sent.
- **Delete account** works with the `s1` from Settings and from the keyless
  screen: auth, then discovery, then storage's backup, then the local
  cleanup. Found while building: none of the three clients deleted the
  backup on account deletion, for any account; all three do now.

Not built: device redeem as a suspended landing (the server cannot mint a
pairing grant for a suspended account); the clients' marks of other people's
suspensions (step 4).

Found while building, not fixed: auth's `identity_pubkey` is asserted by the
client at the genesis mint and, unless the transparency service requires the
genesis attestation (flag-gated, off by default), an account can record a key
other than the one it publishes. A Suspend keyed on the published key then
answers `404 unknown-account` and the account is untouched (open question 6).

## Step 4 as built (2026-11-20)

Host-tested only. Nothing was deployed; no snapshot has been served to a
client over a network, and no mark has been seen on a screen.

- **Scope.** Account suspensions only. The snapshot entry carries the target
  kind, so channel removals, taken-back names and group handles join in step
  5 without a format change, once their services apply them.
- **The snapshot** (`lakko_moderation::snapshot`, section 5). An entry is
  `kind || target || until_ms(8 BE, 0 = no end) || log_index(8 BE)`. A bucket
  is `prefix(2) || count(4) || entries`, canonical (strictly sorted, every
  target under the prefix, at most 16,384 entries). The 65,536 buckets, empty
  ones included, are the leaves of an RFC 6962 tree (perfect, path length 16).
  The snapshot root is log leaf kind 3: `"lakko/moderation/snapshot/v1\0" ||
  version || period || period_secs || built_at_ms || covered_log_size ||
  bucket_root || entry_count`. `build_snapshot` reads the first
  `covered_log_size` leaves of the public log: for each account target the
  Suspend or Lift with the latest issue time decides (ties by log index, the
  rule auth applies), and a Suspend is active while it has no end or its end is
  after `built_at_ms`. Anyone can rebuild a snapshot from the log and compare
  the root.
- **The service.** `services/moderation` builds a snapshot at startup and
  when the current period has none (checked every 60 s, built off the request
  path, appended only if the log did not grow during the build, else retried),
  keeps the current and the previous one, and at a restart rebuilds the two
  newest from the log and drops one whose root differs.
  `LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS` (default 86,400, at least 60;
  chosen). Public `GET /moderation/snapshot/{period|latest}/bucket/{prefix}`
  answers the bucket, its path, the snapshot leaf's inclusion in the
  witnessed head, and that head; `Cache-Control: public, max-age=300`.
- **The client check** (`check_account`, on UniFFI and wasm as
  `moderationAccountStanding`). The log root under a build-time pinned
  moderation-log operator key, the witness quorum under `LogId::Moderation`
  with the KT witness keys, the snapshot's inclusion, the bucket's inclusion
  and prefix, freshness within two periods (ten minutes of clock skew), the
  entry's end. Indexed as C-97; C-96 (the statement check at auth, step 3)
  is indexed with it. C-95 waits for the panel, where evidence is shown.
- **Pins.** `VITE_LAKKO_MODERATION_LOG_KEY` / Info.plist
  `LakkoModerationLogKey` / `BuildConfig.LAKKO_MODERATION_LOG_KEY`, empty by
  default: no request and no mark. The operator key is made on the
  service's first boot and served at `/moderation/log/operator-key`; a
  build that should show marks pins it.
- **The clients** (web and so desktop, iOS, Android). One standing service
  each: the raw answer cached in memory per prefix until the snapshot's reuse
  time, one request in flight per prefix, ten minutes of back-off after a
  failure (chosen), cleared at sign-out and forget device, never persisted,
  never in a background or notification path, never about this device's own
  account. Unknown reads as not suspended everywhere. The marks, as UX
  section 3: the banner at the top of a one-to-one (a block banner wins), the
  card line (a block line wins), the member-list marker, search results
  dropped as standings arrive, the invite page refusing Add (Add waits up to
  3 s for the inviter's standing, chosen), and contact requests from a
  verified-suspended sender dropped unseen on every route, including a
  request carrying an invite-link answer, checked when it arrives and again
  when a held one is decided; held requests are hidden while the standing is
  suspended.
- **Tests run:** `lakko-moderation` 49 + 1; `lakko-svc-moderation` 28 with
  its DB store test against a local MariaDB; `lakko-ffi-uniffi` 452,
  `lakko-ffi-wasm` 314; web vitest 7829; iOS LakkoTests 5028 (one
  unrelated device-link test failed once and passed in the following runs);
  Android host JVM 5374, without the native core. Not run: the fuzz targets.
- **Found while building, fixed:** the web stylesheet used `--space-1..4`
  and `--text-xs` / `--text-sm` from the design system without defining them,
  so those margins and sizes were silently dropped; they are now defined in
  `:root` from the design system's table.
- **Residuals.** A mark appears up to one period after a suspension and can
  stay up to one period after a lift (the snapshot is periodic). A bucket
  query reveals a 16-bit prefix and the time per displayed account. On iOS a
  request carried inside a group is filed before its sender's standing is
  known and hidden once it is; on Android a held invite-link request waits up
  to 3 s each when decided. The signed-out invite page shows no mark (the link
  is checked again after sign-in).

## Step 5 as built (2026-11-20)

Host-tested only, nothing deployed. The panel was walked once in a browser
against a locally run moderation service (in-memory store, a dev moderator
list signed for `localhost`, a software Ed25519 key standing in for the
security key and its PRF); no hardware key, no nginx client-certificate
gate, no running auth behind the appeals screen.

- **One statement check** (`lakko_moderation::apply`): `check_for_service`
  (list under the pinned root, decode, moderator and assertion, action
  belongs to this service, reason commitment) with stable codes, and the
  shared internal body `StatementBody { signed_statement_b64,
  moderator_list_b64, log_index, reason, reason_salt_b64 }`. auth, discovery,
  channels and storage each verify a forwarded statement themselves and keep
  their own replay table, newest list version and per-target issue order
  (C-96 now covers all four).
- **Accounts by target.** auth (`accounts.identity_target`, migration
  `0043`) and discovery (`identity_bundles.account_target`, `0040`) keep
  `BLAKE3-derive-key("lakko/moderation/account-target/v1", key)` beside the
  key, so a statement needs no account key; the `account_key_b64` field is
  gone.
- **auth:** the statement route; `POST /internal/moderation/account`
  (age band day / week / month / older, suspended, end);
  `GET /internal/moderation/appeals` and `POST
  /internal/moderation/appeals/decline` (a decline is not a log entry).
- **discovery** (new internal listener `127.0.0.1:8097`): TakeBackName
  deletes the username and its public record and holds the name for 180 days
  as a moderator claim (kind 3), so no registration, rename or handle claim
  takes it; ReleaseName ends the hold; HideFromSearch removes the public
  record and an Everyone publish then answers `403 hidden-from-search`;
  group delist / block / restore on the handle row (`handles.moderation`):
  delisted cannot publish or be replaced, blocked cannot be written at all and
  is never pruned. A panel read names the account's username lookup keys.
- **channels:** the ADR-0059 shared-secret route is removed. Delist, block
  the `@name`, remove and restore by statement (migration `0014`); a removed
  channel answers `410 {"code":"channel-removed","log_index":n}` on every
  route, before any handler.
- **storage** (new internal listener `127.0.0.1:8098`): BlobDelete removes the
  row and the file and keeps a tombstone forever (an upload of that id
  answers `410 blob-removed`); `GET /internal/moderation/blob/{id}` hands the
  panel the ciphertext of an evidence file without a read pass and without
  counting as an access.
- **Forwarding** (`services/moderation`): one forwarder per service, chosen
  by action; an unconfigured service leaves the entry in the log and answers
  `forwarded: false`. `tools/moderation-root dev-sign` signs every action.
- **The snapshot** also holds channel removals (target kind Channel); clients
  check them with `moderationChannelStanding`.
- **The panel backend** (`services/moderation`, a third listener
  `127.0.0.1:8099`, `LAKKO_MODERATION_PANEL_DIR` for the built app): sign-in
  by a WebAuthn assertion over a single-use challenge, an 8-hour bearer token
  under a key made at startup; cases created by reports and reopened by a
  new report after a decision; locks released after 30 idle minutes; the
  routes of section 7 plus a blob proxy. Migration `0004` adds cases, report
  checks, internal notes, a decision record (the reason in the clear and
  no-action decisions, which the log cannot carry) and an appeal record for
  the weekly numbers. A logged statement decides its case even when the
  forward fails; resending forwards again. The nginx vhost
  (`infra/nginx/sites-available/moderation-panel.conf`) puts
  `ssl_verify_client` at server level and is not enabled anywhere.
- **The PRF input** is frozen in the crate (`MODERATOR_PRF_INPUT`,
  `moderatorPrfInput()` on wasm): enrolment and the panel evaluate the PRF
  over exactly those bytes.
- **The panel app** (`apps/moderation`, SolidJS, English only, the web
  client's stylesheet and wasm): sign-in, queue with filters, case (reports
  opened in the browser, messages marked "Written by this account" or "Not
  verified", media blurred, files on a click), decisions behind a sentence
  naming what is signed and a key touch, appeals with the own-decision
  warning, log, moderators, numbers. C-95 indexed.
- **Clients** (web and so desktop, iOS, Android): a `410 channel-removed`
  no longer ends the channel. It stays in the list with the posts already on
  the device; writing, reacting, commenting and invites are not offered; the
  line is "This channel was removed for breaking Lakko's rules." only when the
  snapshot proves it, else "This channel isn't available."; the info keeps
  the `@name`, description, the find-me switch and the leave row, which the
  owner gets too (it ends the channel on all the account's devices, with no
  server call); a restore brings everything back. A `403
  hidden-from-search` makes Everyone unavailable in Who can find me with an
  explanation, without rewriting the person's setting.
- **Deviations from the UX spec:** the panel cannot show an `@username` (the
  server keeps only lookup keys and a contact request's profile is sealed to
  its recipient); it says "Has a username". Reports are ordered by day only.
- **Not built:** real moderator enrolment with a hardware key (the offline
  tool's `dev-moderator` writes a software key); the test stack has no panel
  listener or client CA.
- **Tests run:** `lakko-moderation` 58; `lakko-svc-auth` 221 + 2,
  `lakko-svc-discovery` 158, `lakko-svc-channels` 156, `lakko-svc-storage`
  51 and `lakko-svc-moderation` 49, each with its DB-gated tests against a
  local MariaDB; `lakko-ffi-uniffi` 453, `lakko-ffi-wasm` 315; web vitest
  7852, iOS LakkoTests 5048, Android host JVM 5398
  (without the native core); panel vitest 50. Workspace clippy, fmt,
  spec-check, log-lint, the invariant index and the catalog check are clean.
  Not run: the fuzz targets, the dockerised e2e suites.

## Security review and fixes (2026-11-20)

A `lakko-security` review of steps 1 to 5 found no critical or high finding
and no break of the E2EE path, and asked for changes; the maintainer chose
to fix everything. Host-tested only, nothing deployed.

- **Fresh statements and the list floor.** A statement whose issue time is
  more than 5 minutes ahead or 24 hours behind the service's clock is refused
  (`future-statement`, `stale-statement`; both chosen), at intake and at every
  applying service. A revoked or stolen moderator key therefore cannot sign a
  back-dated statement older than a day, and a far-future statement can no
  longer sit above every later Lift. Each applying service raises its
  moderator-list floor from any newer verified list it is shown, even when the
  statement is then refused, and offers `POST /internal/moderation/list`; the
  moderation service pushes its list to every service at startup and retries
  until each answers. A failed forward can be resent for 24 hours.
- **A mark needs a moderator's signature.** A snapshot entry is no longer
  enough: before a client marks an account, drops a contact request, hides a
  person from search, refuses Add or says a channel was removed, it fetches
  the log entry the snapshot names and checks it with `prove_standing` (the
  entry's inclusion under the same witnessed head, a Suspend or ChannelRemove
  signed by a moderator of a list verified under the pinned moderation root,
  the same target and end). The moderator list a client uses is never older
  than the list floor the device already keeps. C-97 is amended accordingly.
- **One account per identity key, proven, with history.** Every auth route
  that records the account's identity key (genesis attest, key reset finish,
  key rotate finish, recall finish) requires an Ed25519 proof by that key over
  `"lakko/auth/identity-proof/v1\0" || account_id || key`
  (`lakko_attest::identity_proof`, `signIdentityProof` on both FFI surfaces;
  sent by all three clients). `accounts.identity_target` is unique and
  `account_identity_targets` keeps every target an account ever had, so a
  statement naming a key from before a reset or rotation still finds the
  account, and a key can belong to one account only (`409
  identity-key-taken`). When an account is deleted its history goes with it,
  except while the account is suspended: then its targets stay taken until the
  suspension ends, so the person cannot delete and register again with the
  same key, and a Lift releases them. No backfill (no production data; the
  test stack is reset).
- **Keeping a suspension is a signed decision.** New `Action::KeepSuspension
  = 15` (target account, applied by auth, declines the pending appeal); the
  unsigned decline route is removed. A panel decision closes only the case of
  its own target (a TakeBackName closes the account case whose discovery read
  lists that name); undo actions, BlobDelete and KeepSuspension close none.
- **The panel's code is served by nginx** from a static root
  (`just deploy-moderation-panel`); the moderation service serves only
  `/panel/api/*` in production and serves files only for local development.
- **Smaller fixes:** the channel removal check reads the percent-decoded id and
  fails closed (`503 moderation-check-failed`); no blob id or URL reaches the
  logs; the panel's blob proxy allows 4 reads in flight; the sealed report and
  appeal bind the list version in their AAD (flag day); iOS keeps the block
  list sealed under the App-Group key, excluded from backup; Android attests
  chat messages by default.
- **Residuals, accepted:**
  - a security-key touch does not show what is being signed, so a compromised
    panel page could ask for a different statement than the one on screen;
  - auth alone can put a device into suspended mode (confirming against the
    snapshot would delay a real suspension by a day);
  - panel sessions cannot be revoked before their 8 hours; the sign-in
    challenge table is bounded but not per moderator; an anonymous log read may
    ask the witnesses to co-sign a new head;
  - a suspended account keeps discovery and storage access for the access-token
    lifetime and keeps anonymous tokens already in its pool;
  - two statements with the same issue time are ordered by arrival, which can
    differ between services;
  - reports, sealed content included, are never pruned (legal phase);
  - enrolment cannot require a hardware key, so a synced passkey with PRF puts
    the report key in a cloud keychain;
  - a revoked moderator can still sign statements dated within the day before
    its revocation was distributed.
- **Tests run after the fixes:** `lakko-svc-auth` 224 + 2, moderation 54,
  discovery 159, channels 161, storage 52, each with its DB-gated tests on a
  local MariaDB; `lakko-moderation`, `lakko-attest` and `lakko-transparency`
  225; `lakko-ffi-uniffi` 455, `lakko-ffi-wasm` 317; web vitest 7884, iOS
  LakkoTests 5074, Android host JVM 5429; panel vitest 58. Workspace clippy,
  fmt, spec-check, log-lint, fuzz-check (build only), the invariant index and
  the catalog check are clean. Not run: the fuzz targets, the dockerised e2e
  suites, anything on a device or against a deployed server.
