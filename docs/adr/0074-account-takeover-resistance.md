<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0074: Account takeover resistance: recall after a reset, device removal that revokes, and a key rotation that keeps the other devices

- **Status:** Accepted (2026-10-07; the maintainer accepted D1 to D11 as
  recommended). Sprint T1 (core + server: credentials that name a device,
  the account statement, the recorded identity key, device session floors,
  logout / bind-device / revoke, discovery's device gate and tombstones) is
  built and host-tested; see "T1 as built". Sprint T3 (server: recall
  anchors, the capture rule, the three recall routes, the deletion refusal)
  is built and host-tested, with deviations from this text that matter to
  the clients; see "T3 as built". Sprint T5 (core + server: the key-rotation
  envelope, `rotate/{start,finish}`, the session upgrade, discovery's proof
  verification and proven-rotation arm, the DS root endorsement and attested
  root transfer, the channel owner handover, two recall follow-ups) is built
  and host-tested; see "T5 as built". The client sprints are tracked in
  `docs/PRODUCT-READINESS.md`, not here; T7 and T8 are not built. The recon
  was done by reading the tree.
- **Date:** 2026-10-07
- **Amended by [ADR-0078](0078-servers-cannot-link-accounts.md) phase 6 (2026-10-16):**
  the root endorsement (`root_endorsement_b64`, `sign_ds_root_endorsement`)
  and the attested DeviceGroup reset and root transfer are removed. The
  DeviceGroup id is derived from the lineage seed (the oldest own
  predecessor), so a rotation keeps the journal and moves its root with a
  plain `root` transfer signed by the old root.
- **Context:** account recovery, Part 5 (after
  [ADR-0072](0072-recovery-model-simplification.md) and
  [ADR-0073](0073-account-key-reset.md))
- **Amends (when built):** [ADR-0073](0073-account-key-reset.md) (section 3:
  the credentials gain a device id and an issue time; section 3.5: the
  signed-out device no longer wipes everything at once; "per-device session
  tokens" leaves the rejected list in a narrower form; R.2: a proven
  rotation is told apart from a reset; R.3 + section 5: one race, below),
  [ADR-0016](0016-identity-supersession-transport.md) (the emit half
  returns for one explicit action; discovery verifies the proof),
  [ADR-0036](0036-auth-attested-genesis.md) (the genesis mint is bound to a
  recorded key), [ADR-0050](0050-per-device-conversation-leaves.md)
  (section 2.10 session revocation on removal; the re-admission arm for the
  own account), [ADR-0057](0057-journal-ownership.md) and
  [ADR-0059](0059-channel-discover.md) (a root and an owner key move with a
  rotation). Supersedes none.
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
  section 18 (added with this ADR).

Renumbered from 0066 on 2026-10-13: the main branch had taken 0064 to 0071 for
other decisions. Commit messages before that date name this ADR as 0066, and so do the
migration files of this work, whose bytes are checksummed and were left as written.

## Context

### The maintainer's decision (2026-10-07)

Three things are built together. In all three the recovery phrase is
assumed lost. (A device that still holds the keys can show the phrase again
behind the password, so "phrase lost" means "no device with keys is in the
person's hands, or the person never opens that screen".)

The three scenarios the maintainer walked:

1. **One phone, and a new one.** The person knows only the password. They
   sign in on the new phone and reset the account keys (ADR-0073). The
   server cannot tell them from someone who learned the password: both get
   the new phrase, can set a new password with it, and the other party is
   locked out. A delay before the reset takes effect was rejected: it
   punishes the commonest honest case (the phone was stolen yesterday and
   still looks active). **Decision: the reset stays instant, and an owner
   who still holds a device can take the account back.**
2. **One phone, stolen.** Removing a device in Settings > Devices takes it
   out of the DeviceGroup and the conversations and shows "This device was
   removed" on it. Its server session stays valid, its data is wiped only
   on a tap, and nothing was checked about it putting itself back.
   **Decision: removing a device really removes it, and logging out revokes
   the session.**
3. **Several devices, one stolen.** Removing it does not change the account
   seed it holds. The only way to change the account keys today signs out
   and wipes every other device and loses history. **Decision: "this device
   was stolen": remove it and change the account keys, keeping the other
   devices and the chats.**

Contacts see nothing for any of the three (the rule of ADR-0073 "Phase 2
revised").

### Facts about the code (read on 2026-10-07)

Line numbers are of this worktree on that day. "(0073)" marks a fact
carried from ADR-0073's own recon without a re-read.

**Auth: credentials and what is stored.**

1. OPAQUE envelopes are one row per `(account_id, kind)` in
   `opaque_registrations` (`services/auth/migrations/0002`, `0004:30-34`),
   kinds `KIND_PASSWORD = 1`, `KIND_RECOVERY = 2`
   (`services/auth/src/opaque.rs:139,142`). Every replacement is an
   in-place upsert; **no previous envelope is retained anywhere**.
2. `/password/reset/finish` upserts the password row
   (`opaque.rs:1362-1370`) and is not in a transaction. It is anonymous and
   is authorised by the recovery credential, which is derived from the
   account seed. So **any device that holds the seed can set a new password
   with no other proof**; the fresh-proof-or-phrase gate in front of it is
   client-side only (web `auth/fresh-auth.ts:587`, "the bare session would
   be enough"). There is no change-password route that asks for the old
   password.
3. `accounts` is `(id, created_at, identity_version, flags)`
   (`migrations/0001:19-27`), never altered. **Auth stores no identity
   public key and no device identifier.** The reset's
   `new_signature_pubkey_b64` is client-asserted and only bound into the
   attestation (`services/auth/src/key_reset.rs:331-337`); the genesis
   mint (`POST /auth/genesis/attest`) signs whatever key the bearer
   supplies and stores nothing (ADR-0036).
4. Credentials are `v2` / `r1` / `k1` + `account_id . key_generation .
   expires_at_ms . tag` (`crates/lakko-auth-token/src/lib.rs:13-17`). No
   session id, no device id, no issue time; "revoked by the generation,
   never individually" (`lib.rs:65-67`). No session table exists.
5. The fresh-auth grant is minted by the password login, the passkey login
   and signup (`opaque.rs:604,1029`, `webauthn.rs:724`), lives 600 s, and
   is required by the key reset, account deletion (no-passkey accounts),
   adding a device and adding a passkey. TOTP mints none.
6. The reset keeps the password envelope and the TOTP secret and deletes
   the passkeys (`key_reset.rs:95`, `:314-321`). TOTP is a way to sign in,
   not a second factor on the password login.
7. Storage and gif-proxy verify a token statelessly and have no database
   (`services/storage/src/auth.rs:67-72`,
   `services/gif-proxy/src/auth.rs:44-48`). Discovery verifies statelessly
   and applies the generation floor on two writes only
   (`services/discovery/src/key_reset.rs:187`, `keypackage.rs:150`,
   `identity_bundle.rs:660`).

**Removing a device (scenario 2).**

8. The remove flow on all three clients is: optional passkey ceremony,
   DeviceGroup Remove commit, then a fire-and-forget fan-out that retires
   the device's leaves chat by chat and deletes its directory row last
   (web `routes/Devices.tsx:283-349`, `devices/remove.ts:163,190`,
   `chat/BackgroundReceiveContext.tsx:877-916`; iOS
   `UI/Devices/DevicesScreen.swift:727`, `Chat/ChatRepository.swift:5325-5361`;
   Android `ui/devices/DevicesScreen.kt:916-925`,
   `chat/ChatRepository.kt:6689-6709`). **Nothing is sent to
   `services/auth`.** On web a chat the fan-out fails on "is logged and
   retried by nothing" (`Devices.tsx:336-337`).
9. **The removed device can put itself back, and nothing stops it.**
   - Discovery accepts a KeyPackage publish on any session of the account
     plus the generation floor (`services/discovery/src/keypackage.rs:137-160`);
     it keeps no record of removed devices (the delete is a plain `DELETE`,
     `:260`) and "never parses the bytes and never checks that `device_id`
     matches the KeyPackage's leaf key" (`:17-20`).
   - The delete route is authorised by any session of the account
     (`keypackage.rs:243-263`): **a removed device can also delete the
     other devices' rows.**
   - The re-admission arm admits a device that is listed in the directory,
     absent from the roster and not in the checking client's local
     "removed" set (web `chat/device-readmission.ts:55-83`; iOS
     `Chat/DeviceReadmission.swift:83-110`; Android
     `chat/ChatRepository.kt:6809-6935`). That set is **local to the client
     that observed the removal**, keyed on the leaf key, not synced
     (`device-readmission.ts:20-23,104,123-142`; UserDefaults /
     SharedPreferences twins), so another device of the owner, a contact's
     device, or a device paired later does not have it.
   - The core authorises any member to add a bound device of an account
     already in the group (`crates/lakko-mls/src/group_proposals.rs:396-399`),
     and a binding needs only the account seed
     (`crates/lakko-mls/src/device_binding.rs:96-100`,
     `identity.rs:316-328`), which the removed device holds. A fresh device
     key minted from the seed is not in anybody's removed set.
     ADR-0050 already records this (`docs/adr/0050-per-device-conversation-leaves.md:635-637`).
   - Web re-publishes its KeyPackage "on every session start, reload
     included" with no removed check (`auth/AuthContext.tsx:1805-1808`).
     Not traced end to end, nor on the natives.
10. The removed device wipes only on a tap, on all three (web
    `devices/RemovedDeviceScreen.tsx:40-49,77`; iOS `LakkoApp.swift:719-723`;
    Android `MainActivity.kt:1457-1460`); the automatic wipe exists only
    for the key-reset variant. Its session token is untouched.
11. Logout calls no auth route on any client (web `AuthContext.tsx:4157`,
    iOS `Auth/AuthRepository.swift:1815`, Android
    `auth/AuthRepository.kt:3927`): it drops the local token and
    unregisters push. `services/auth/src/main.rs` has no logout route.

**Rotation with the old key in hand (scenario 3).**

12. The supersession proof primitive is intact: 145 bytes, signed by the
    OLD key, at most 30 days valid
    (`crates/lakko-transparency/src/supersession.rs:27-36,78,130,230`), on
    both FFI surfaces (`lakko-ffi-uniffi/src/lib.rs:4960`,
    `lakko-ffi-wasm/src/supersession.rs:35-36`), with **no production
    caller** (web `AuthContext.tsx:4626-4634`: retained "for a future
    explicit 'rotate my identity' action"). The verify gate is live: a
    changed key with a valid proof is re-pinned with no prompt (web
    `contacts/client.ts:1884-1978`, `setPin` at `:1968`; iOS
    `Contacts/ContactsClient.swift:230-290`; the Android re-pin line was
    not read).
13. Discovery, on a key change with a proof: **presence only**, "it does
    NOT verify the proof bytes" (`services/discovery/src/identity_bundle.rs:585-587`);
    the generation is not bumped, the KeyPackage rows are not deleted, the
    cool-down does not apply, and the log append is `is_reset: false`
    (`:720`, `:743`, `:827-870`). The transparency append does verify the
    proof against the previous entry (`services/transparency/src/store.rs:334-351`),
    under a flag (`handlers.rs:168`); the log is not deployed (0073 R.1
    fact 4).
14. A conversation leaf is a per-device identity whose credential is a
    137-byte binding signed by the account key, with no expiry
    (`device_binding.rs`). `Identity::bind_to_account` can re-bind a
    device identity, and "leaves already in groups keep the credential
    they were added with" (`identity.rs:306-328`). But the device judges
    **its own** authority in a group from the identity's binding, not from
    its leaf: `own_role`, the owner check of a role change and of an
    ownership transfer, `authorise_own_proposal`, the committer rank and
    the sweep scope all read `identity.account_public_key()`
    (`crates/lakko-mls/src/group.rs:1173,1642,1760,1941,2099,2378`). A
    re-bound device would therefore read itself as a stranger in every
    group it is already in, while its peers still resolve its leaf to the
    old account key.
15. No path lets a member bring in a leaf of a **different** account key
    without being owner or admin (`group_proposals.rs:370-405`); `Update`
    proposals are refused for everyone (`:442`); the role table is checked
    against the account key (`:382`) and only the owner rewrites it
    (`:449-455`); `one_to_one_holds` refuses a commit that drops an account
    or reaches three (`:724-769`); `owner_survives` requires the owner key
    to hold a leaf after the commit (`:887-921`). An owner can add any
    account's device, transfer ownership to an account that is a member,
    and hand the journal root over (ADR-0057), all with existing calls.
16. Seed-derived state: the DS journal roots and registrar seeds
    (`crates/lakko-ds-proof/src/statement.rs:113,128`); the reach keys
    (`crates/lakko-contacts/src/reach.rs:18-19,221`); the mailbox
    pseudonym, from the public key (`pseudonym.rs:14,52`), with reads
    signed by the identity key; the channel owner key and the
    co-publisher key (`crates/lakko-channels/src/owner.rs:58,79`,
    `copublisher.rs:15`); the recovery credential; the backup KEK and the
    passkey vault blob (from the phrase). **`services/channels` has no
    route to replace an owner key** ("The owner key is fixed at creation",
    `api.rs:10`; `sql_store.rs:185`). A `RootTransfer` statement is heard
    only from the current root and may name any new root
    (`services/ds/src/ownership.rs:290,914`).
17. Not seed-derived: the DeviceGroup id (account id), the push routing
    secret, the login handle, TOTP, the username registry, the username
    certificate and the KT-log key (all on the account id).
18. No DeviceGroup carrier delivers a seed today. The pairing path seals
    `grant ‖ phrase_entropy` under a DeviceGroup exporter key into a
    rendezvous row (`apps/web/src/devices/recovery-payload.ts:31-44,76`).
    `ServiceEnvelope` (field 20 of `schemas/mls-app/devicegroup_v1.proto`)
    carries `lakko-contacts` envelopes, and the intake drops any kind it
    does not know (`apps/web/src/reach/service-envelope-intake.ts:9-18`).

**One race in ADR-0073 as designed (not built).** The DS hears an attested
`Reset` of the DeviceGroup journal "from ANY signer" carrying a valid
attestation at a strictly higher generation
(`services/ds/src/key_reset.rs:4-35`). R.3 has discovery serve the
attestation to anyone who fetches the bundle. Between the resetting
device's directory upload (client step 3) and its DS reset (step 4), and
for as long as an owed retry lasts, anyone who fetched the bundle can
present the same attestation and become the root of that account's
DeviceGroup journal. The damage is to availability of device sync (the
content is MLS-sealed), but it is a stranger holding a root. See decision
4.6.

### Hard constraints that follow

- The recall has to check a password that is no longer the account's and a
  key the server never stored (facts 1, 3).
- A password can be replaced by a seed holder alone (fact 2), so "the
  password as it was before the reset" is not necessarily the envelope that
  was current at the reset.
- Nothing addressed to one device can be revoked (fact 4), and three
  verifiers cannot look anything up (fact 7).
- For the account's own devices, the directory row plus a binding is the
  whole admission test, and the seed mints bindings (fact 9). So as long as
  the seed is the same, removal is only as strong as the gate in front of
  the directory.
- A leaf cannot change its account in place, and a re-bound identity
  misjudges itself (facts 14, 15).

## Decision

**Auth records the account's identity public key and keeps, for a bounded
window, what the account was before each credential change. Credentials
name a device. A key change that the previous key signed is a rotation; one
that it did not is a reset, and a reset can be taken back by the previous
key together with the previous password.**

Four shared mechanisms (section 0), then the three parts.

### 0. Shared mechanisms

#### 0.1 The recorded identity key (`services/auth`)

`accounts` gains `identity_pubkey BINARY(32) NULL`. Writers, and only
these:

- `POST /auth/genesis/attest`: when the column is NULL it is set to the
  attested key in the same statement; when it is set, the route signs
  **only that key** and answers any other with
  `409 {"error":"identity-key-mismatch"}`. This also narrows ADR-0036: a
  session can no longer obtain a genesis attestation for an arbitrary key.
  Clients call the mint on every landing until it has succeeded once (it is
  idempotent for the recorded key).
- `reset/finish`, `rotate/finish`, `recall/finish`: set it to the new key,
  inside their transaction, after the previous value has been captured
  (0.2).

A NULL column means "no recall, no rotation and no device revocation by
key for this account yet"; the routes that need it answer as below.

What this stores: one public key that discovery already publishes for the
same account id. No private material (invariant #2), no PII (#1).

#### 0.2 Recall anchors (`services/auth`)

```sql
CREATE TABLE recall_anchors (
  account_id            BINARY(16)      NOT NULL,
  seq                   BIGINT UNSIGNED NOT NULL,      -- per account, increasing
  identity_pubkey       BINARY(32)      NOT NULL,      -- the key that may recall
  password_registration VARBINARY(4096) NOT NULL,      -- the password envelope before the change
  created_at_ms         BIGINT          NOT NULL,
  expires_at_ms         BIGINT          NOT NULL,
  failed_attempts       INT UNSIGNED    NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, seq),
  UNIQUE KEY (account_id, identity_pubkey)
);
```

**Capture rule.** Every operation that replaces the password envelope or
the identity key (`/password/reset/finish`, `reset/finish`,
`rotate/finish`; **not** `recall/finish`) runs, in its own transaction and
before its change:

- if `accounts.identity_pubkey` is NULL: nothing;
- if an unexpired anchor exists for the **current** identity key: leave its
  contents alone; when the operation changes the identity key, set its
  `expires_at_ms` to now + window;
- otherwise insert one: the current identity key, the current password
  envelope, `expires_at_ms` = now + window.

So there is at most one anchor per identity key, and it holds the account
as it was **before the first change made while that key was current**. A
later change cannot overwrite it. This is what stops the party who makes
the change from choosing what the recall is checked against: the key was
recorded before they acted (0.1), and the envelope was the one in force
before their first act.

**Window:** `LAKKO_RECALL_WINDOW_DAYS`, default 30. Chosen, not measured.
At most 8 unexpired anchors per account (chosen); past that a change
captures nothing and the older anchors stand.

**Seniority.** Anchors are ordered by `seq`. A recall through anchor `i`
deletes anchor `i` and every anchor with a higher `seq`, and keeps the
lower ones. A recall captures no anchor. Consequences:

- The party a recall displaces cannot recall back: their key has no anchor
  (they were the current key), and they do not hold an older one.
- A party with an **older** standing than the one who recalled still can.
  This is deliberate; see "Where this departs from the brief" below.
- Every recall strictly shortens the list, so there is no ping-pong.

**What is stored and for how long:** an OPAQUE password registration record
(the same trust class as the live envelope: a verifier, not a password) and
a public key, for at most the window plus one extension per key change. A
pruner deletes expired rows hourly.

**Where this departs from the brief.** The brief said "a recall is itself
not recallable". Two simpler shapes were examined and found unsound or
weak:

- *One anchor, overwritten by each reset.* An attacker who resets twice
  makes their own first key the recall key, recalls, and the result is
  final: the owner is locked out by the very mechanism.
- *One anchor, write-once for the window.* Sound, but the honest sequence
  "I reset because my phone was stolen; ten days later someone who knows my
  password resets again" leaves the person with no recall, because the only
  anchor names the stolen phone's key. An honest reset is the commonest
  reason an anchor exists.

The ordered list is the smallest shape that is sound and covers the honest
sequence. It keeps the property the brief wants (the displaced party cannot
answer) and adds one: an older standing outranks a newer one. Maintainer
decision D1.

#### 0.3 Account-key statements (`lakko-attest`, client-signed)

Auth and discovery now each hold the account's identity public key, so a
request can prove "the caller holds the account key" without a session. One
fixed layout, in a new module `crates/lakko-attest/src/account_statement.rs`
(the crate's Ed25519 template; this is its first kind signed by a client
and verified by a service):

```text
wire   = version(1)=0x01 ‖ kind(1) ‖ account_id(16) ‖ unix_ms(8 BE)
         ‖ nonce(16) ‖ body_hash(32) ‖ sig(64)            = 138 bytes
signed = "lakko/account-statement/v1" ‖ 0x00 ‖ wire[0..74]
```

`body_hash` is BLAKE3 over the kind's canonical body. Kinds:

| Kind | Name | Body | Verified by |
|---|---|---|---|
| 1 | `recall-status` | empty | auth, under an anchor's key |
| 2 | `recall` | `server_nonce(32) ‖ new_signature_pubkey(32) ‖ BLAKE3(password_upload) ‖ BLAKE3(recovery_upload)` | auth, under an anchor's key |
| 3 | `device-revoke` | `device_id(32)` | auth and discovery, under the current key |
| 4 | `session-upgrade` | `BLAKE3(refresh_token)` | auth, under the current key |

Verification order everywhere: layout, kind, account id, `unix_ms` within
120 s (the `lakko-ds-proof` window), signature, body hash. Kinds 2 to 4 are
bound to a server nonce or to a secret the caller must also present, so a
captured statement is useless alone; kind 1 is a read and may be replayed
inside the window. Bounded, fail-closed decode; fuzz target
`account_statement_open`; a domain-crossing test against the five
attestation kinds and the username certificate. FFI (both surfaces, sign
only): `signAccountStatement(accountSeed, kind, accountId, body)`.

#### 0.4 Credentials name a device (`lakko-auth-token`, flag day)

```text
v3.<account_id>.<key_generation>.<device_id>.<issued_at_ms>.<expires_at_ms>.<tag>
r2.<account_id>.<key_generation>.<device_id>.<issued_at_ms>.<expires_at_ms>.<tag>
```

`v2` and `r1` are refused (nothing is deployed). `device_id` is 32 bytes,
base64url: the device's conversation leaf signature key, which is already
the public `device_id` of discovery's directory (ADR-0073 fact 6). It is
**asserted by the client** at every session-landing route (a new body field
`device_id_b64`) and copied by the refresh. It is not proven: the thing it
protects against is a credential that was already issued, and whoever can
sign in afresh is the owner as far as the server can tell. The all-zero id
means "no device identity yet" (a sign-in before the device has keys); such
a session cannot publish a KeyPackage, and
`POST /auth/session/bind-device {device_id_b64}` (bearer) replaces the pair
once with one naming the device. `verify` returns `{account_id,
key_generation, device_id, issued_at_ms}`. The fresh-auth grant `k1` is
unchanged.

Auth gains:

```sql
CREATE TABLE device_session_floors (
  account_id    BINARY(16) NOT NULL,
  device_id     BINARY(32) NOT NULL,
  not_before_ms BIGINT     NOT NULL,
  reason        TINYINT UNSIGNED NOT NULL,   -- 1 logout, 2 removed
  PRIMARY KEY (account_id, device_id)
);
```

`require_session` and the refresh route read the floor with the generation
(one more primary-key read) and refuse a credential whose `issued_at_ms` is
below it: reason 2 with `401 {"error":"device-revoked"}`, reason 1 with the
existing `refresh-invalid` / plain `401`. A row is pruned once
`not_before_ms` is older than the refresh lifetime (every credential it
could refuse has expired). What auth now holds per account is a list of
device ids **that were logged out or removed in the last 30 days**, not a
registry of devices; discovery already publishes the live ones. This is the
narrower form of the alternative ADR-0073 rejected.

### 1. Recall: taking the account back after a reset

#### 1.1 What the old device keeps

On `key-generation-superseded` (ADR-0073 section 3.5 and landing rule 3),
the device does what it does today **except**:

- It wipes immediately, as today: the message log, contacts, every MLS
  snapshot, the DeviceGroup state, the outbox, drafts and synced maps, the
  session and refresh credentials, the phrase store, the passkey material,
  the backup floor, the push registration, anonymous tokens.
- It keeps, under the device key, a **recall record**: the account id, the
  32-byte Ed25519 seed of the old account identity (not the 64-byte phrase
  seed, not the phrase), the old public key, the username for the screen,
  the generation it was superseded at, and the time it learned. The device
  key itself therefore stays until the record goes.
- It shows "Was it you?" (UX 18.4) instead of today's signed-out screen.

The record is destroyed, and the rest of the wipe (the device key) runs,
when: the person answers **Yes**; a recall succeeds; the status route says
no anchor exists for this key; or the window has passed by the device's
clock. **Yes** is local: it sends nothing. A server-side "confirm" was not
added because the only party that could give it without the password is
whoever holds the device, and the window closing by itself costs nothing.

Cost, stated: a device in the wrong hands keeps the old account signing
seed for up to 30 days more than today, for a client that honours the wipe.
A thief who extracts the seed does not let the app wipe anyway.

#### 1.2 Routes (`services/auth`, anonymous)

The old device's session is superseded, so these take no bearer.

**`POST /account/recall/status`** `{statement_b64}` (kind 1) ->
`{anchor_seq, expires_at_ms, changed_at_ms}`. The anchor is the one whose
key verifies the statement. Any failure (no account, no anchor, expired,
bad signature, outside the window) is one `401 {"error":"recall-refused"}`.

**`POST /account/recall/start`** `{account_id_b64, anchor_seq, login_request,
password_request, recovery_request}` ->
`{login_response, password_response, recovery_response, server_nonce_b64}`.
`login_request` is an OPAQUE login start against the **anchor's**
password envelope (credential identifier as for the password kind);
`password_request` and `recovery_request` are OPAQUE registration starts
for the new password and the new recovery credential. An unknown account or
anchor takes OPAQUE's fake-credentials path with a stand-in, as the
password login does, so the answer has the same shape and timing class. The
login state is stored in `opaque_login_states` under a new kind tag
`KIND_RECALL = 4`, TTL as the login's.

**`POST /account/recall/finish`** `{account_id_b64, anchor_seq,
login_finish, password_upload, recovery_upload, new_signature_pubkey_b64,
device_id_b64, statement_b64}` (kind 2) -> the reset's answer:
`{key_reset_attest_b64, key_generation}` + the session pair + a fresh-auth
grant.

Checks, in this order:

1. Per-address bucket (`Category::Attempt`) and `Scope::Recall = 14`,
   keyed on the account id hash: default 5 per 15 minutes (the TOTP
   lockout's shape). Both fail closed.
2. The anchor exists and is unexpired, and the statement verifies under
   **its** key with the body bound to the `server_nonce` of the stored
   state, the new key and both uploads. Any failure here, the limiter
   apart: `401 {"error":"recall-refused"}`. A caller without the key learns
   nothing about the password and nothing about whether an anchor exists.
3. The OPAQUE login finish. Failure: `failed_attempts += 1`,
   `401 {"error":"wrong-password"}`; at 20 failures (chosen) the anchor is
   deleted. Only a holder of the anchor's key reaches this step, so only
   they can spend the 20.
4. One transaction, the reset's (ADR-0073 section 2) with these
   differences: no fresh-auth grant is asked for; the anchor is locked
   (`FOR UPDATE`) and it and every higher `seq` are deleted; **no anchor is
   captured**; the password envelope is replaced by `password_upload`; the
   TOTP secret is deleted; the login handle binding is kept; besides the
   passkeys, vaults, WebAuthn states, action grants, device grants and
   device requests the reset already deletes, every row of
   `device_session_floors` for the account is dropped (the generation
   supersedes everything). `identity_pubkey` becomes the new key, the
   generation is bumped, a `KeyResetAttestation` is minted, a pair and a
   grant are issued.

`Scope::KeyReset` is **not** charged: the attacker's resets must not spend
the owner's way back.

**The identity after a recall is new.** The recalling device generates a
fresh phrase and seed, exactly as a reset does; the old key authorises the
recall and does not return (invariant #6's second sentence). Its
conversation state was wiped at 1.1 and its peers have already moved to the
attacker's key, so the old key would buy nothing. From the reset's client
order (ADR-0073 section 2.3) onward the recall **is** a reset: the same
owed record, the attested directory upload, the DeviceGroup re-founding,
the new words shown last. To every peer it is a second attested key reset
at a higher generation, which "Phase 2 revised" already handles (R.8, "a
second reset before the first was handled").

#### 1.3 While a window is open

- `DELETE /account` is refused with `409 {"error":"recall-window-open",
  "until_ms"}` while any unexpired anchor exists. Without this the party
  who cannot keep the account can destroy it, and a deleted account cannot
  be recalled. Cost: a person who honestly reset cannot delete the account
  for 30 days. Maintainer decision D3.
- A second reset, a rotation, a password reset by the new phrase, a new
  passkey, a new TOTP secret, a new device: all allowed, none touches an
  existing anchor's contents. A recall undoes every one of them.
- A deletion by someone who knows only the password, **without** a reset,
  is not covered by this ADR: no anchor exists then. Named in "Not built".

#### 1.4 Who can do what

| Party | Holds | Outcome |
|---|---|---|
| Owner | a device from before the change, and the password from before the first change | Recalls. New identity, new password, new phrase; the attacker's device is signed out; TOTP and passkeys cleared. |
| Thief | the password only | Resets instantly, as today. Is the account toward contacts until the owner opens the app, then loses it. Cannot recall (holds no anchor key). Cannot delete the account inside the window. |
| Finder | a phone, not the password, app data not extracted | Sees "Was it you?" if the owner reset. `recall-refused` never; `wrong-password` at most 20 times, 5 per 15 minutes. Gets nothing. |
| Finder | a phone, the seed extracted, no password | Can set a new password today (fact 2), then sign in, then reset. The owner's other device recalls through the anchor captured at the password change, with the password from before it. With no other device the owner has lost the account: this is the seed, and part 3 is what answers it while the owner still has a device. |
| Attacker | a phone **and** the password | Total loss, in this scheme and in any: they are indistinguishable from the owner and can recall as well as reset. Whoever recalls first through the oldest anchor wins. |
| Operator | the attest key or the database | Unchanged from ADR-0073 R.9. The anchors add: the operator could keep or restore an old password envelope, which is a verifier it already had. |

**The cost, plainly.** Between the reset and the moment the owner opens the
app on an old device, the attacker is the owner toward every contact, for
new messages only (history is not theirs). What a contact sends in that
time the attacker reads and keeps; a recall does not take it back. If the
owner has no device left, or does not open one within the window, the
takeover stands.

#### 1.5 Enumeration and guessing, summarised

Without a signature by an anchor key the three routes answer one body, and
`start` answers the same shape for anything. With it, the caller is a
device of the account and learns only what the screen needs. Guessing the
old password online needs the anchor key and is capped at 20 per anchor;
offline guessing needs the database, as for the live password.

### 2. Removing a device, and logging out

#### 2.1 Routes

**`POST /auth/session/logout`** (bearer; no grant): upserts a floor for the
token's own `(account, device)` at now, reason 1. The client then drops its
credentials as today. A later sign-in on the same device issues credentials
above the floor.

**`POST /account/devices/revoke`** (bearer) `{fresh_grant, device_id_b64,
statement_b64}` (kind 3, body the device id) -> `204`. Checks, in order:
the fresh-auth grant (`403 fresh-auth-required`, before any limiter); the
target is not the token's own device (`400`); `accounts.identity_pubkey` is
set and the statement verifies under it (`403
{"error":"account-key-required"}`); `Scope::DeviceRevoke = 15`, default 20
per hour. Then upserts the floor, reason 2.

**Why the account key is required as well as the grant.** A revoked device
wipes itself (2.3). With the grant alone, someone who knows only the
password could sign in on a device without keys and wipe every device the
owner has, without even resetting, and those devices hold the key a recall
needs. Requiring a signature by the account key means only a device that
already holds the account's keys can remove another. Consequence: a
signed-in device that has not restored its keys cannot remove devices; it
can reset, which is recallable.

#### 2.2 Discovery (`services/discovery`)

- `POST /discovery/keypackage`: the body's `device_id` must equal the
  token's (`403 {"error":"device-mismatch"}`), and must not be tombstoned
  (`403 {"error":"device-revoked"}`). Discovery still parses nothing.
- `DELETE /discovery/keypackage/device/{id}`: for the token's own device,
  as today. For another device it requires the header
  `x-lakko-account-statement` (kind 3 for that id), verified under the
  row's own `identity_bundles.signature_pubkey`; else `403
  {"error":"account-key-required"}`. Every delete writes a tombstone
  `(account_id, device_id, deleted_at)` in a new table, kept 30 days, and
  the route comes under the generation floor (closing the ADR-0073
  residual, G-46's last clause).
- An attested key change (reset, recall, rotation) deletes the account's
  tombstones with its KeyPackage rows.

Effect on fact 9: the removed device's still-valid access token names a
tombstoned device, so it cannot re-publish; it cannot publish under a fresh
device key (the token does not name it and it cannot sign in for a new
token without a password or passkey); it cannot delete the others' rows.
This holds from the moment of the delete, not after the access lifetime.

#### 2.3 Clients

The removing device, in this order, from one owed record that is retried
at every landing until each step has an answer:

1. obtain a fresh proof (UX 17.1's block);
2. `POST /account/devices/revoke`. If it fails, stop: nothing else has
   happened and the screen says so;
3. `DELETE /discovery/keypackage/device/{id}` with the statement (first,
   not last as today: it is what closes the directory);
4. the DeviceGroup Remove commit (attested when the policy is on);
5. the per-chat leaf removal, now **retried** from the owed record until
   every chat has answered.

The removed device: on `401 device-revoked` from auth, or on its own
DeviceGroup turning inactive, it stops every writer, shows the removed
screen and **wipes by itself**, everything, with no recall record (a
removal is not a key change; nothing was taken that a recall could give
back). `DeviceGroup.isActive == false` is proof of removal by a device of
the account; a `401` from the DS is not and triggers nothing.

Own-account admission, all three clients: the re-admission arm and the
pairing fan-out add a device of the **own** account only when its leaf key
is in the current DeviceGroup roster. The DeviceGroup is the account's own
record of its devices, it is MLS-authenticated, and a removed device is not
in it. This replaces the local "removed" set for the own account (the set
stays for contacts' devices). It needs no core change: it is a filter in
front of a proposal the core would accept.

Logout: `POST /auth/session/logout`, best effort with one retry at the next
launch from a small owed marker, then the local drop as today.

#### 2.4 What the other verifiers do inside the access lifetime

| Service | A revoked or logged-out device, for at most 900 s |
|---|---|
| auth | Nothing. Every bearer route reads the floor. |
| discovery | Reads, and the writes not gated above (invite ledgers, channel name registration, public records). Not the directory, not the identity bundle. |
| storage | `GET` / `PUT /backup`. It can fetch the phrase-sealed blob (which it could already open, holding the phrase) and re-put a blob it holds; it cannot mint a head. |
| gif-proxy | Searches. |
| DS, push-gw, channels | Unchanged by tokens: it reads a journal until its leaf is removed there and its epoch key rotates out. |

No feed of floors to storage and gif-proxy is built; the bound is the
access lifetime, as in ADR-0073.

#### 2.5 What removal still does not do

The removed device holds the account seed. With it and the help of the
operator (a directory row served without a session), or of any member
willing to add it, it can be put back into a conversation: for a contact's
client the test is still "listed, and bound" (fact 9). It can set a new
password (fact 2) and then sign in. Removal revokes a device; it does not
revoke a seed. That is part 3, and the Devices screen says which one the
person needs (UX 18.1).

### 3. Rotating the account keys and keeping the other devices

#### 3.1 What is and is not achievable

**Achievable without changing `lakko-mls`:** the identity, the directory,
the KT log entry, every contact's pin (silently, by proof), the recovery
credential, the phrase, the sessions, the mailbox and reach keys, the DS
journal roots, the channel owner keys, the backup, and delivery of the new
seed to the kept devices with all their local data intact.

**Not achievable without it: moving a conversation leaf to the new account
key in place.** A leaf's account is its credential; a credential changes
only by an `Update`, which the core refuses for good reason (C-66); a new
leaf under the new key is a different account to every gate (fact 15); and
a re-bound device misjudges its own standing (fact 14). The in-place answer
is an MLS change: a leaf re-credential that carries the supersession proof,
with the group resolving the new key to the account it already knows. That
is a state-machine change under invariant #3 and G-39 and is **not**
designed here (see "Not built").

**So the rotation is built in two stages.**

- **Stage A, this ADR:** everything in the first list. Conversation leaves
  keep their bindings under the old key and keep working; peers treat the
  old key as the **proven predecessor** of the new one.
- **Stage B, after ADR-0073 "Phase 2 revised" is built:** the leaves move,
  by the mechanisms that design already has (remove and add in a role-table
  group, a successor for a one-to-one), with the old key cooperating where
  that makes a case repairable that a reset leaves unrepairable. Sketched
  in 3.6.

**What Stage A does and does not take from the thief.** After Stage A the
stolen seed no longer signs in (recovery credential replaced), no longer
sets a password (the reset route checks the new credential), no longer
publishes an identity or a device (the directory key changed, and discovery
and auth verify statements under the new one), no longer owns a journal or
a channel, and does not read the new mailbox. It still **mints bindings
that existing conversations accept as this person**, because those
conversations still name the old key. Getting such a device into a
conversation needs a directory row or a member's help, exactly as before
the rotation (2.5). Stage A therefore makes the conversation layer no
weaker and everything else safe; Stage B closes the conversation layer.

#### 3.2 Order on the rotating device

"The stolen device is out first" is an ordering rule with the same weight
as C-61: the new seed is sealed at a DeviceGroup epoch the stolen device
never held.

1. A fresh proof. A local owed record is written before anything is sent.
2. Remove the stolen device (2.3 steps 2 to 4; step 5 continues in the
   background). The DeviceGroup is now at an epoch the stolen device cannot
   read.
3. Generate the new phrase and seed. Sign the supersession proof with the
   old seed (`signSupersessionProof`, validity 14 days as before ADR-0017).
4. **Prepare:** send a `key-rotation` envelope in the DeviceGroup (3.3).
5. **Commit:** `POST /account/keys/rotate/{start,finish}` (3.4). From here
   the account is at the new generation.
6. Persist the new seed, phrase, credentials and attestation; then the
   directory upload with the proof **and** the attestation (3.5).
7. Re-key what hangs on the seed (3.5): journal roots, the new mailbox and
   reach keys, channel owner keys, the profile envelope with the new
   grants, the backup.
8. Show the new words (the reset's last step; withheld while the publish
   is owed, as today).

A failure before step 5 changes nothing on the server; the envelope of
step 4 names a key that never becomes the account's, and the kept devices
discard it when it expires (3.3). A failure after step 5 resumes from the
owed record with the same attestation, as a reset does.

#### 3.3 Delivering the seed: the `key-rotation` envelope (`lakko-contacts`)

A new envelope kind carried in the DeviceGroup's existing `ServiceEnvelope`
(fact 18), so neither the proto nor `lakko-mls` changes:

```text
"lakko/key-rotation/v1\0"(22) ‖ version(1)=0x01 ‖ prepared_at_ms(8 BE)
  ‖ new_entropy(16) ‖ supersession_proof(145)                 = 192 bytes
```

Fixed length, bounded fail-closed decode, dispatched as its own
`IncomingEnvelope` variant, fuzz target `key_rotation_decode`. It is an MLS
application message in the DeviceGroup, so it is readable by the devices
that are members at that epoch and by nobody else, the server included.

A kept device that receives it checks: the proof's old key is the account
key it holds; the proof verifies; the seed derived from `new_entropy` gives
the proof's new key. It then stores the envelope as **pending** and changes
nothing. It **adopts** when it sees the commit: the published bundle
carries the proof's new key, or its own refresh is answered
`key-generation-superseded`. Adopting is: store the new seed and phrase,
keep the old account signing seed in a "predecessor" slot (needed until
Stage B), obtain a session (below), and continue. Nothing local is wiped
and nothing is shown beyond one notice (UX 18.3). A pending envelope whose
key has not become the account's within 14 days is dropped.

**The kept device's session.** Its credentials are of the old generation.
`POST /auth/session/refresh` gains an optional `key_statement_b64` (kind 4,
body `BLAKE3(refresh_token)`): a refresh credential that is valid but of a
**lower** generation is exchanged for a pair at the current generation when
the statement verifies under `accounts.identity_pubkey` and the device is
above its floor. Only a device that holds the new account key can produce
it. After a reset or a recall no old device can (the new key is random to
them), so the route's meaning for those is unchanged.

**Order on a superseded device.** A device whose refresh is answered
`key-generation-superseded` and that holds no pending envelope first reads
its DeviceGroup journal to the head, once, before it decides anything
(the DS does not use session tokens, so it still can). An envelope found
there is adopted as above. Only when the journal has none, or cannot be
read, does the device treat the change as a reset (1.1). The server does
not tell it which it was.

**A kept device that never sees the envelope** (offline past the journal's
retention, or removed from the DeviceGroup meanwhile) is superseded like
any old device and lands on "Was it you?" (1.1). Its **Yes** leads to the
ordinary sign-in and "get your keys from another device"; it loses its
local data, as after a reset. Stated as a residual.

#### 3.4 `POST /account/keys/rotate/{start,finish}` (`services/auth`, bearer)

`start {fresh_grant, recovery_request}` as the reset's. `finish
{fresh_grant, recovery_upload, new_signature_pubkey_b64,
supersession_proof_b64, revoke_device_ids_b64[]}` -> the reset's answer.

Differences from `reset/finish`:

- **The proof is verified by auth**: `verify_supersession(proof,
  accounts.identity_pubkey, now)` and the proof's new key equals the
  body's. No recorded key, or a failed proof: `403
  {"error":"account-key-required"}`. So a rotation needs the old key, a
  reset does not: someone with the password alone cannot rotate.
- Passkeys are **kept**; `passkey_vaults` rows are deleted (they seal the
  old phrase). Device grants and device requests are deleted. TOTP and the
  password are kept.
- An anchor is captured by the ordinary rule (0.2).
- Each id in `revoke_device_ids_b64` (at most 8) gets a floor, reason 2,
  in the same transaction (a removal that raced the commit).
- Limits: `Scope::RecoveryReplace` on both calls, `Scope::KeyRotate = 16`
  on `finish`, default 3 per day.

The answer carries a `KeyResetAttestation` of the existing kind. No new
attestation kind: what tells a rotation from a reset is the proof beside
it.

#### 3.5 The other tiers

**Discovery.** `POST /discovery/identity` with both a proof and an
attestation is a third evidence arm, **proven rotation**: discovery
verifies the attestation as today and now also **verifies the proof**
against the stored key (it holds both; the proof is a public artefact, so
this parses nothing private). It stores both, sets `key_generation`,
deletes the account's KeyPackage rows and tombstones, does not apply the
cool-down, and appends to the log with `is_reset: false` and the proof as
`leaf_auth`. Separately, the proof-only arm of ADR-0016 also verifies the
proof from now on instead of testing for presence (fact 13): a session
alone must not be able to change the published key to something every
client then refuses.

**Peers.** A fetched key that differs from the pin, with a proof:

- the proof verifies under the pinned key, with `now` taken as the
  attestation's `signed_at` when a valid attestation at a higher generation
  is present (so a peer that was away for months still reads it as a
  rotation; `verifySupersession` already takes the time as an argument),
  else the real time;
- outcome: re-pin as ADR-0016 does, record the generation, and mark the old
  key a **proven predecessor**. Leaves bound to a proven predecessor stay
  what they are: the same person. Nothing is evicted, no successor is
  founded, nothing is shown.

This is the one place R.2's rule ("a previous key is never accepted for new
traffic") is narrowed: it holds for a key superseded by a **reset**; a key
superseded by a **proof** remains the binding of the leaves that exist,
until Stage B moves them. A later reset of the same account makes every
predecessor, proven or not, a superseded key in R.7's sense.

**Kept devices' directory rows.** A kept device re-publishes its
KeyPackage after adopting. Its binding is still under the old key (it does
not re-bind: fact 14). A peer adding the account's devices to a new
conversation accepts a binding under the pinned key **or a proven
predecessor of it**. Whether any client compares a fetched KeyPackage's
binding with the pin today was not read; build-time check, all three
clients.

**A device paired after the rotation** receives the new seed as any paired
device does, and, until Stage B, a binding under the old key minted by the
device that lets it in (which still holds the predecessor seed), carried
in the pairing bundle. So during the interim the conversation layer has one
account key, the old one, everywhere.

**DS.** For every journal the account roots (the DeviceGroup, each group it
owns, each PublisherGroup; enumerated locally from the role tables, as
`JournalOwnershipContext` does), the old root signs a `RootTransfer` naming
the root derived from the new seed (fact 16), then the new root replaces
the registrar set and re-sends the registrar seeds (ADR-0057's existing
steps after a transfer). The chain is kept, so a kept device that lags an
epoch is not cut off. The new mailbox (its pseudonym follows the new key)
is claimed and the reach keys registered; the old mailbox is read with the
old key for the journal retention window, then left.

**Channels.** A new route `PUT /v1/channels/{id}/owner` taking a statement
signed by the **current** owner key that names the new owner key
(`lakko-channels::owner`, one more signed body in that module's existing
shape, fuzzed). The rotating device calls it for every channel it owns. A
co-publisher key the account holds in someone else's channel is derived
from the old seed and can only be replaced by that channel's owner
re-appointing it; the client asks nothing of them and this is a residual.

**Reach.** The profile envelope is re-sent in every conversation with
grants for the new mailbox. Peers store grants under the contact, not
under the key. Whether each client's grant store is keyed that way was not
read; build-time check.

**Backup, vault.** The backup is re-sealed under the new phrase and
uploaded with a new head. Passkeys keep signing in; "let this passkey
restore keys" is off for each until the person turns it on again.

#### 3.5a The mailbox after a rotation (decided 2026-10-11)

The maintainer's decision: **the account moves to the new mailbox at once.** A mailbox's address and
the right to read it both derive from the account key, so the holder of the old seed can read the old
mailbox for as long as anything is delivered there; keeping it as the inbound address for all of
Stage A (the alternative considered) would leave the stolen key a live inbox, and would let its
holder rewrite that mailbox's reach-key set.

1. **Senders resolve before they address.** Wherever a client derives a mailbox from an account key
   (a Welcome for a leaf, a contact request, a call cold start, a committer delegation, "another
   device of my own account"), it first maps the key through the proven-predecessor resolver
   (predecessor -> current, own account included) and addresses the CURRENT key's mailbox with a
   grant issued for it. An unproven change never gets this treatment.
2. **Grants are keyed on the person, not on the key they arrived under.** A reach grant received in
   a profile envelope from a leaf bound to a predecessor key is stored for the current key of that
   account.
3. **The rotating account re-sends its profile** (and therefore its grants for the new mailbox) to
   its one-to-ones and groups after the reach re-key, through the existing throttled publisher.
4. **The old mailbox is read, never written to, and then abandoned.** The account's devices keep
   polling each predecessor mailbox for 30 days after adopting (the DS journal retention; chosen)
   so nothing already sent is lost, then stop. No new reach key is registered there.

5. **What was granted before the rotation is spent (decided 2026-10-11).** The rotating account's
   `/i/` and `/c/` links and the `reply` grants of its unanswered contact requests named the old
   mailbox and are not honoured through it: the holder of the old seed knows the same links. The
   rotating device revokes its own links at the ledger where a revoke exists, drops them from its
   lists, says so on the rotate sheet (`devices.rotate.effect_links`), and re-sends its unanswered
   contact requests from the new key as an owed step; the receiver shows one request per person.
   A contact request or ack that arrives in a predecessor mailbox is not acted on.
6. **A refused write re-checks the address once.** When a write to a person's mailbox is refused,
   the sender fetches that person's identity bundle through the fail-closed gate, re-resolves and
   retries once; a grant found under a predecessor key is kept beside the newer one until then.

Until a sender has observed the proven change (it fetches the bundle on its next lookup of that
account) it still addresses the old mailbox with an old grant; that write is accepted and is read
under rule 4, which is the window the 30 days cover.

#### 3.6 Stage B, in outline

Built on ADR-0073 R.5 to R.7 once those exist. The rotating account's
devices each take a second device identity bound to the new key (a new
device, as far as MLS is concerned) and are moved conversation by
conversation; the old key's cooperation changes three of R.7's cases:

- **A group the account owns:** repairable in place. The old-key owner adds
  the new-key devices, transfers ownership to the new key
  (`owner_survives` holds: the new owner is a member), hands the root over,
  and the old leaves leave. No successor group.
- **A group where it is an admin:** it adds its own new-key devices itself;
  the owner later puts the new key in the role table.
- **A one-to-one:** still a successor (fact 15), but either side can found
  it, and the rotating side knows its contacts and holds their grants, so
  the continuity reach key of R.4 is not needed for it.
- **A group where it is a plain member:** unchanged from R.7 case 2; an
  owner or admin acts.

When no conversation still holds an old-key leaf of the account, the kept
devices destroy the predecessor seed. Until then it is on them. Stage B is
not specified further here; it gets its own section when R.5 to R.7 are
built and the two-identities-per-device cost on each client has been read.

#### 3.7 Restoring from the phrase after a rotation (added 2026-11-01)

Found walking the base scenarios on two clients. Stage A leaves every
conversation leaf under the oldest key, new conversations included, while the
phrase and the directory hold the newest one. Two things followed:

- A device restored from the phrase held only the newest key, so its
  KeyPackage was bound to a key no conversation knew: a different account to
  every gate.
- A contact first met after the rotation pinned the newest key by first
  observation and held no link to the key the roster names, so its
  re-admission check did not follow the person. Worse, such a contact was
  talking to a key its pin said nothing about.

The visible result: the sender's message reads Sent, the restored person
receives nothing, and neither is told.

**The backup carries the own predecessors.** `BackupPayload` version 7 adds
the account's own predecessor seeds, oldest first, at most 8, each with its
adoption time (the fields of the pairing state bundle). The backup is sealed
under the newest phrase, whose holder already outranks every predecessor, so
this widens nothing. On restore a device stores them where a kept device does
and takes the path a paired device takes: it mints its binding under the
oldest own predecessor while it holds no conversation.

**A two-way conversation link.** `lakko-transparency::conversation_link`: 201
bytes, `version ‖ conversation_key(32) ‖ current_key(32) ‖ issued_at(8 BE) ‖
sig_by_conversation_key(64) ‖ sig_by_current_key(64)`, both signatures over
the same domain-separated bytes (`lakko/transparency/conversation-link/v1`).
No expiry: it states what the account's conversations are bound to.

A supersession proof is not enough here. It is signed by the OLD key alone, so
it is anchored by a pin of the old key, which is exactly what a contact first
met after the rotation does not have. Accepting one at first observation would
let a malicious directory attach a key of its choosing to a person, and a
KeyPackage bound to that key would then pass the check that a binding is under
the pin or a proven predecessor of it. The link is signed by the key the
contact pinned, so it cannot be attached without that key's consent.

The rotating device mints the link (it holds both seeds) and uploads it with
the identity bundle. `identity_bundles.conversation_link` is opaque, at most
256 bytes, never parsed: kept across a re-upload of the same key, replaced by
a rotation, cleared by a reset. A malicious server can strip it (the contact
then stays where it is today, an availability loss) and cannot forge it.

A client verifies it after the identity gate has passed, against the key the
gate left pinned, and records the conversation key as a proven predecessor of
that pin. A link that does not verify is ignored and changes nothing.

**One person, one contact row.** Found on the first live walk of this section:
the contact row of a person met after a rotation is under the key they asked
with, the conversation under the key their leaves are bound to, and the rule
that a one-to-one is a contact compared the key alone, so one person became two
rows. Every rule that writes a contact row now matches by the proven chain:
the conversation key is kept as an alias on the row, a removal of either key
holds for the person, and two rows of one person merge on load.

**As built, 2026-11-01.** Web, iOS and Android, walked on a local stack with
the vault flag set by hand: each client as the rotated account, restored from
the phrase, and iOS and Android as the contact who met them afterwards. The
letter path was not walked. Left open: a group member with no pin and no
binding stays unknown, so no link is fetched for them; a device that restores
after it has already landed has opened its DeviceGroup under the wrong lineage;
a pre-rotation contact has not been walked on this path.

Unchanged: Stage B, and everything in 3.1 about what the old seed can still
do. A person who rotated keeps two keys; 3.7 makes that survivable, not gone.
Most accounts no longer rotate at all (ADR-0075 section 3, amended the same
day).

### 4. How the three interact

1. **A recall is a reset to everyone else.** Peers follow it by attestation
   at a higher generation; nothing is shown.
2. **A rotation is a proof to everyone else.** No eviction, no successor.
3. **A reset during an open window** keeps every anchor and captures one
   for the then-current key if it has none.
4. **A rotation during an open window** likewise. A rotation needs the old
   key, so "the attacker rotated" means the attacker held a device with
   keys and the password: the total-loss row of 1.4. The owner's other
   devices are then either kept (the attacker did not remove them: they
   adopt the attacker's seed and are no worse off) or removed (they wipe).
5. **A removal inside a rotation** is one flow (3.2). A removal alone
   captures no anchor and bumps nothing.
6. **The race found in the recon.** The DS must not hear an attested
   `Reset` from "any signer". Decision: the `Reset` and `RootTransfer`
   statements that carry `key_reset_attest_b64` must also carry a 64-byte
   signature by the attested `new_signature_pubkey` over the statement's
   signer key (domain `lakko/ds/root-endorsement/v1`); the DS verifies it
   with the key in the attestation it is already checking. A served
   attestation is then useless to anyone but the new identity. A rotation
   uses the same field on its `RootTransfer` of the DeviceGroup journal,
   which also records the generation there so the attestation cannot be
   presented later.
7. **Fresh-auth grants** are of a generation; a recall and a rotation spend
   them as a reset does.

### Contract a build follows

Server and core, in dependency order:

- `lakko-auth-token`: `v3` / `r2` with `device_id` and `issued_at_ms`;
  `verify` returns both; `token_verify` fuzz updated.
- `lakko-attest::account_statement`: the layout, four kinds, sign and
  verify, fuzz, domain-crossing test; FFI sign on UniFFI and wasm.
- `lakko-contacts::key_rotation`: the envelope, dispatch variant, fuzz;
  FFI compose / decode on UniFFI and wasm, decode-only on the NSE surface
  (it must be suppressed there like every service envelope).
- `services/auth` migration `0030`: `accounts.identity_pubkey`,
  `recall_anchors`, `device_session_floors`. Routes: the genesis mint rule;
  `device_id_b64` on every landing route; `/auth/session/bind-device`;
  `/auth/session/logout`; `/account/devices/revoke`; the refresh's floor
  check and `key_statement_b64`; the capture rule in
  `/password/reset/finish` (moved into a transaction), `reset/finish`;
  `/account/recall/{status,start,finish}`; `/account/keys/rotate/{start,finish}`;
  `DELETE /account` refusal; scopes 14 to 16; pruners.
- `services/discovery`: the device gate and tombstones; the delete gate and
  floor; proof verification; the proven-rotation arm.
- `services/ds`: the root endorsement; `RootTransfer` with an attestation.
- `services/channels`: the owner-key replacement route.
- No change: `lakko-mls`, `schemas/`, storage, gif-proxy, push-gw,
  transparency, witness.

Error codes a client must tell apart: `device-revoked`,
`account-key-required`, `identity-key-mismatch`, `recall-refused`,
`wrong-password`, `recall-window-open`, `device-mismatch`, and the existing
`key-generation-superseded`, `fresh-auth-required`, `refresh-invalid`.

## Not built, deliberately

- **In-place re-credentialing of a conversation leaf** (3.1). An MLS
  change; its own ADR and G-39.
- **A delay on the reset.** Rejected by the maintainer.
- **A server-side "it was me".** 1.1.
- **Recall of an account deletion**, and any protection against a deletion
  by someone who knows only the password and does not reset first.
- **A change-password route that asks for the old password.** When it is
  built it must run the capture rule (0.2); it then needs nothing else from
  this ADR.
- **Proof of possession of the device id.** 0.4.
- **A floor feed to storage and gif-proxy.** 2.4.
- **Notifying contacts of anything.**
- **Re-appointment of the account's co-publisher keys in others' channels.**

## Security consequences, per invariant

- **#1.** New at rest in auth: an identity public key per account; up to 8
  rows of (public key, OPAQUE password registration) for at most the
  window; a list of device ids logged out or removed in the last 30 days.
  In discovery: tombstoned device ids for 30 days. None is PII; all are
  keyed on the account id only.
- **#2.** The server gains no key material. A retained password
  registration is a verifier of the class it already holds; retaining an
  **old** one means a database copy taken inside the window can be attacked
  offline for a password the person has since replaced, which matters where
  that password is reused elsewhere. The `key-rotation` envelope is
  MLS-sealed; the server relays it.
- **#3.** `lakko-mls` is not changed. The proven-predecessor rule is a
  client rule about which pinned key a binding may name; the own-account
  DeviceGroup filter narrows what a client proposes. Stage B and the
  in-place alternative are outside this ADR.
- **#4.** Every new check refuses: an unverifiable statement, a failed
  proof, a revoked device, a mismatched genesis key. The one automatic
  acceptance added is a kept device adopting a seed, and it requires an
  MLS-authenticated envelope from its own DeviceGroup, a proof by the key
  it holds, and the server-side commit.
- **#5.** Auth sees which device id a session belongs to; discovery already
  published the same ids per account. The DS, push-gw and channels learn
  nothing new. A recall and a rotation are visible to anyone who can fetch
  the bundle as a generation change, as a reset is.
- **#6.** The text has to change again (after ADR-0073 R.9's proposal).
  Proposed: "Identity rotation only via a supersession proof signed by the
  previous key, or via a server-attested key reset recorded in the KT log.
  Old keys do not come back as an account's identity. A key superseded by
  proof remains valid only as the binding of conversation leaves that
  existed before the rotation, until they are moved." Maintainer decision
  D8.
- **#7.** The recall record and the predecessor seed are stored under the
  hardware-backed device key like the seed they come from. Both lengthen
  the life of an old signing seed on a device: 30 days for the recall
  record, until Stage B completes for the predecessor.

Flag for `lakko-security` before any of it merges: auth gains three
anonymous routes and a stored key; an acceptance rule on every client
changes (invariant #6).

## Alternatives rejected

- **A delay before a reset takes effect.** The maintainer's decision; it
  punishes the honest lost-phone case.
- **Recall by device alone, or by password alone.** The first lets a phone
  thief take an account back from the owner who reset because of the theft;
  the second is what the attacker holds.
- **The recall key supplied at the reset, or taken from a genesis
  attestation the client presents.** The resetter is the attacker in the
  case that matters, and any session can mint a genesis attestation for any
  key today (fact 3).
- **Auth asking discovery for the previous key.** Auth cannot read
  discovery's database, and a cross-service call on an anonymous route is a
  new trust edge for something one column answers.
- **A recall that restores the old identity key.** The device wiped its
  conversation state, the peers moved on, and it breaks "old keys do not
  come back".
- **One anchor** (overwritten, or write-once). 0.2.
- **A device registry with per-session ids at auth.** More state than the
  floor, and the device id is already public.
- **Revocation by the fresh-auth grant alone.** 2.1: remote wipe by
  password.
- **A rotation by proof with no attestation and no generation bump.** The
  stolen device's session would survive, discovery's floor would not move,
  and a thief holding the old key could sign a competing proof that a
  lagging peer accepts from a colluding directory. With the attestation a
  proof alone moves nobody who has seen a higher generation.
- **A rotation that is a reset on the server and a hand-over of the new
  seed to the kept devices, with conversations rebuilt as after a reset.**
  It keeps local history but breaks every conversation until peers act and
  makes owned groups successors; the proven-predecessor rule costs peers
  nothing and breaks nothing.
- **Re-binding the kept devices' identities in place.** Fact 14: they would
  lose their standing in every existing group. Fixing that is a `lakko-mls`
  change for a state Stage B removes anyway.
- **Delivering the new seed under a key derived from the old seed.** The
  thief holds the old seed.
- **Re-rooting the DeviceGroup journal by the attested reset during a
  rotation.** It drops the epoch chain and cuts off a kept device that lags
  by one epoch; the old root can transfer instead.

## Open questions (the maintainer's)

- **D1.** Ordered anchors, an older standing outranking a newer one (0.2),
  instead of "a recall is final". Recommended: accept.
- **D2.** The password the recall asks for is the one from before the
  **first** change made while that key was current, which for a person who
  changed their own password inside the window is the older one.
  Recommended: accept; the screen says it (UX 18.4).
- **D3.** Account deletion refused while a window is open. Recommended:
  accept.
- **D4.** Numbers: window 30 days; 5 recall attempts per 15 minutes; an
  anchor destroyed at 20 wrong passwords; 8 anchors. Recommended as
  written; all chosen, none measured.
- **D5.** Removing a device needs the account key as well as the password
  or passkey, so a signed-in device without keys cannot remove devices.
  Recommended: accept.
- **D6.** A removed device wipes completely and keeps no way back; an old
  device after a reset keeps the recall record for the window. Recommended:
  accept.
- **D7.** The rotation ships as Stage A first, with conversations on the
  old key and the old seed kept on the owner's devices until Stage B, which
  waits for "Phase 2 revised". Recommended: accept; the alternative is to
  hold the whole of part 3 until then.
- **D8.** Invariant #6's text.
- **D9.** A rotation keeps passkeys and the password and turns "restore
  keys with this passkey" off. Recommended: accept.
- **D10.** After a recall the account has a new identity, not the old one
  back. Recommended: accept.
- **D11.** The root endorsement (decision 4.6) as the fix for the race in
  ADR-0073 R.3, to be built before discovery serves the attestation.
  Recommended: accept.

## Sprint plan

Each sprint leaves the tree shippable. Server and core first, then web
(the reference), iOS and Android in one pass each (the same-class rule).

| Sprint | Scope | Closes |
|---|---|---|
| T1 core + server | `v3` / `r2`; `account_statement` + FFI; `identity_pubkey` and the genesis rule; floors; logout, bind-device, revoke; discovery's device gate, tombstones, delete gate and floor. DB-gated route tests; bindings and native libraries rebuilt. | The server half of "log out / remove device revokes the session"; fact 9's directory hole; the ADR-0073 delete residual. |
| T2 clients | `device_id` at sign-in; the removal order and its owed record; automatic wipe on `device-revoked` and on an inactive DeviceGroup; logout; the own-account DeviceGroup filter; UX 18.1, 18.2. | Scenario 2 on all three, for an honest client and against a removed device acting through the server. |
| T3 server | Anchors and the capture rule (including `/password/reset/finish` in a transaction); the three recall routes; the deletion refusal; scope 14. | The server half of scenario 1. |
| T4 clients | The staged wipe and recall record; "Was it you?" and the recall form; recall as a reset from the client order on; UX 18.4. | Scenario 1 on all three. |
| T5 core + server | `key_rotation` envelope + FFI; `rotate/{start,finish}`; the refresh upgrade; discovery's proof verification and proven-rotation arm; the DS root endorsement and attested `RootTransfer`; the channels owner route. | The server half of scenario 3, Stage A; the R.3 race. |
| T6 clients | The rotate flow (3.2); adoption on kept devices; the proven-predecessor rule at every pin and binding check; re-keying of roots, mailbox, reach, channels, backup; pairing during the interim; UX 18.1, 18.3. | Scenario 3, Stage A. |
| T7 walk | Two accounts, three devices: remove one and see it wipe and stay out; reset from a keyless device and recall from an old one; rotate with one device removed and message a contact both ways before and after. `docs/PRODUCT-READINESS.md` from what was seen. `lakko-security` on request. | The claims above, observed. |
| T8 (later) | Stage B, after ADR-0073 R-S1 to R-S5. | The conversation layer of scenario 3. |

## T1 as built (2026-10-07)

Sprint T1 (core + server) is built: the `v3` / `r2` credentials, the account
statement and its FFI, the recorded identity key and the genesis rule, the
device session floors, `logout` / `bind-device` / `devices/revoke`, and
discovery's device gate, tombstones, delete gate and floor. Unit-tested and
DB-gated route-tested on the host; nothing was walked on a device, the
native libraries and the wasm package were not rebuilt, and no client calls
any of it yet (T2). The maintainer accepted D1 to D11 as recommended.

Every point where the build differs from the text above, or decides
something the text left open:

**Credentials (`lakko-auth-token`).**

1. `verify` / `verify_refresh` return `VerifiedToken { account_id,
   key_generation, device_id, issued_at_ms }`; the fresh-auth grant keeps its
   five-part `k1` shape and gets its own `VerifiedGrant { account_id,
   key_generation }`. Because the shapes now differ (seven segments against
   five), a grant presented as a bearer, or a bearer as a grant, is refused
   as malformed by the segment count before any version marker is read.
2. `issue` / `issue_refresh` take the device id and the issue time from the
   caller; the crate reads no clock.

**Floors (`services/auth`).**

3. A credential is refused when `issued_at_ms <= not_before_ms`, not `<` as
   0.4 says: nothing orders a credential and a floor stamped in the same
   millisecond, so the tie goes to the revocation. A sign-in in the very
   millisecond of a logout on the same device is refused once and works on
   the next attempt.
4. **The floor is read before the generation**, in `require_session` and in
   the refresh. A removed device whose account was also reset since is told
   `device-revoked` (wipe everything, no recall record), not
   `key-generation-superseded`. **Reversed in sprint T3** (the generation
   is read first): see "T3 as built", item 1.
5. A floor only moves up, and its reason is never softened: a logout on a
   device id that was removed leaves the reason "removed" (the upsert takes
   the greater of both columns). The table has a foreign key to `accounts`
   with cascade (account deletion drops the floors) and an index on
   `not_before_ms` for the hourly pruner.
6. `POST /auth/session/logout` on a session that names no device answers
   `204` and writes nothing: there is no id to key a floor on. **A session
   that never bound a device cannot be revoked per device at all**; it ends
   when its credentials expire, or at a key reset. An honest client binds
   before it can do anything worth revoking; a thief's stolen device is
   already bound and cannot unbind.
7. Migration `0030` carries `accounts.identity_pubkey` and
   `device_session_floors` only. `recall_anchors` is left to T3's own
   migration, against the contract's single-migration list, so that T1 ships
   no table nothing reads.

**`bind-device`.**

8. The body is `{device_id_b64, refresh_token}`, not the device id alone.
   The route issues a new 30-day refresh credential, and a bearer alone (15
   minutes) must not be exchangeable for one. The refresh credential must be
   the bearer's account's, at the current generation, and name the same
   device the bearer names.
9. Outcomes: both credentials unbound -> a pair naming the device; both
   already naming the requested device -> a fresh pair (a retry after a lost
   answer), subject to that device's floor; anything else -> `409
   {"error":"device-already-bound"}` (a code the text did not list). The
   unbound pair that was replaced is not revoked (see 6); the client drops
   it.

**Landing routes.**

10. `device_id_b64` (URL-safe base64, no padding, 32 bytes) is OPTIONAL on
    `/signup/finish`, `/login/finish`, `/recovery/login/finish`,
    `/password/reset/finish`, `/webauthn/auth/finish`, `/totp/recover` and
    `/device/authorize/redeem`; absent means the all-zero id. It is decoded
    before any single-use state is consumed, so a malformed id (`400`) costs
    the caller nothing. `/account/keys/reset/finish` takes it too and
    defaults to the device the caller's bearer names. The session pair's
    JSON keys are unchanged: the device rides inside the two credentials.
11. The refresh route's `key_statement_b64` (the kept device's upgrade after
    a rotation) is T5 and is not built.

**The recorded identity key.**

12. The genesis mint and the key reset both refuse a key that does not
    decode as an Ed25519 point or is of small order (`400`), and
    `verify_account_statement` refuses to verify under a small-order key: a
    signature under one can be forged for any message, and this key now
    authorises a remote wipe.
13. The genesis mint is no longer stateless: one conditional `UPDATE ...
    WHERE identity_pubkey IS NULL`, then a read-back. Two racing first mints
    record one key; the loser gets `409 identity-key-mismatch`.
14. **Residual, stated:** an account whose client never called the genesis
    mint has no recorded key. Whoever holds a session for it first (someone
    with only the password included) can record their own key and then
    revoke devices with it. The clients must mint at signup and on every
    landing until it has succeeded once (0.1); T2 owns that.
15. Auth's recorded key and discovery's published key are two columns in two
    services with no call between them. They differ between a
    `reset/finish` and the attested directory upload that follows it: in
    that window auth verifies a `device-revoke` statement under the new key
    and discovery under the old one. A reset deletes every directory row
    anyway, so the removal order (2.3) has nothing to do there; a client
    must not run a removal while a publish is owed.

**`POST /account/devices/revoke`.**

16. Two more `400`s than the text lists: the all-zero id, and a
    `device_id_b64` that does not decode. The server does not check that the
    id is a device of the account (it keeps no registry); a floor for an id
    no credential names refuses nothing.
17. No server-side nonce store for the statement. A captured `device-revoke`
    statement replayed inside the 120 s window still needs the bearer and
    the fresh-auth grant, and re-running a revoke only raises the same floor.

**The account statement (`lakko-attest`).**

18. All four kinds are defined and tested; only `device-revoke` has a
    verifier in a service so far. Besides `verify_account_statement` there
    is `AccountStatement::decode_unverified` (for the T3 routes that must
    read the account id before they know the key). The statement is
    base64url without padding both as the `statement_b64` body field and as
    the `x-lakko-account-statement` header.
19. FFI, sign only, on UniFFI and wasm: `signAccountStatement(accountSeed,
    kind, accountId, body)` as specified (`accountSeed` is the account
    identity's 32-byte Ed25519 seed; `kind` is an enum on UniFFI and the
    wire byte on wasm), plus `signDeviceRevokeStatement(accountSeed,
    accountId, deviceId)`, `accountStatementHeaderName()` and
    `accountStatementLen()`. The clock and the nonce are taken inside the
    call. One appended `FfiError::AccountStatement`; on wasm an `Error` whose
    message is `"<code>: <reason>"`. Not on the NSE extension surface (that
    process removes nothing).

**Discovery.**

20. A session that names no device cannot publish a KeyPackage at all
    (`403 device-mismatch`), and needs the account statement to delete any
    row, since it owns none.
21. **Every delete tombstones, the device's own included** (as 2.2 says).
    Consequence for the clients: a device that deletes its own row (forget
    this device) cannot publish under the same device id again for 30 days.
    It must mint a new device key when it is set up again; T2 has to confirm
    every client does.
22. The tombstone window is the constant `TOMBSTONE_RETENTION_MS` (30 days),
    not an environment knob. The delete and the tombstone are one
    transaction. An attested key change and the account erasure delete the
    account's tombstones with its rows.
23. The statement for deleting another device's row is verified under
    `identity_bundles.signature_pubkey`; an account with no bundle cannot
    delete another device's row (`403 account-key-required`).
24. `403 device-revoked` from discovery is not proof of removal for a client
    (2.3 names auth's `401` and the inactive DeviceGroup). A client that
    meets it should refresh at auth and act on that answer.
25. Proof verification on the identity upload and the proven-rotation arm
    are T5 and are not built; the supersession-proof arm is still
    presence-only.

**Other verifiers.**

26. `services/storage` and `services/gif-proxy` are unchanged in code and
    verify the `v3` token statelessly; a logged-out or removed device keeps
    them for at most the access lifetime (2.4). Discovery likewise for every
    route but the two KeyPackage writes and the identity upload.

**Tests.** Token crate: every refusal, on both session kinds, including each
signed segment rewritten. `lakko-attest`: layout, every kind, every signed
byte, window edges, the small-order key, a golden, and the crossing against
the six other kinds. Auth (DB-gated, through the router): logout revokes
only the caller's device; a revoke needs the grant AND a statement by the
recorded key, refuses every malformed or foreign statement with one code,
stops the target's bearer and refresh with `device-revoked` and nothing
else; the limiter is charged only after both proofs; the genesis mint signs
only the recorded key and the reset moves it; `bind-device`; a landing route
names the asserted device. Discovery (DB-gated): a session publishes only
its own device; a removed device cannot re-publish, cannot publish under a
fresh key and cannot delete another device's row; a tombstoned id stays dead
until the window passes; the delete is under the generation floor; a key
change clears tombstones.

Not done in T1: fuzzing against a budget (the targets compile; none was
run), a rebuild of the native libraries and the wasm package (the bindings
were regenerated), any client change, and the `lakko-security` pass.

## T3 as built (2026-10-07)

Sprint T3 (server) is built: `recall_anchors` and the capture rule,
`POST /account/recall/{status,start,finish}`, the refusal of
`DELETE /account` while a window is open, limit scope 14, a fifth account
statement kind and two FFI sign helpers. DB-gated route tests pass on the
host against the local MariaDB. Nothing was walked on a device, no client
calls any of it (T4), the bindings were NOT regenerated and no native
library or wasm package was rebuilt.

The sketches in section 1.2 are not what was built in four places that a
client must know (items 2 to 5). Everything else below is a decision the
text left open, or a defect of the text or of T1 found while building.

**Changes to T1 and ADR-0073 that the recall needed.**

1. **The generation is read before the device floor** in `require_session`
   and in the refresh route; T1 read the floor first (its item 4). A device
   answers `device-revoked` by wiping everything and keeping no recall
   record (2.3), and a floor is written by whoever holds the account's
   CURRENT identity key plus a fresh-auth grant. After a hostile reset that
   is the attacker: they sign in again with the password, sign
   `device-revoke` statements with their own key for the owner's device ids
   (which discovery lists), and every device of the owner would then have
   destroyed the key a recall needs. With the generation first, a
   credential of an older generation is always told
   `key-generation-superseded`, whatever floors exist. Cost: a device the
   owner removed and whose account was reset afterwards keeps a recall
   record for the window instead of wiping at once; it still needs the
   password from before. `device-revoked` is now only ever sent to a
   credential of the current generation.
2. **A landing route issues its pair at the generation it read BEFORE it
   consumed the credential**, not at one read afterwards. `/login/finish`,
   `/recovery/login/finish` and `/password/reset/finish` read it in the
   transaction that takes the login state; `/device/authorize/redeem` in
   the transaction that consumes the grant; `/totp/recover` and
   `/webauthn/auth/finish` before the row they depend on. A reset and a
   recall delete those rows in the transaction that bumps the generation,
   so a sign-in that straddles one either finds its row gone or comes out
   with credentials of the old generation. Before this, a sign-in in flight
   at the moment of a reset or a recall came out with a session of the NEW
   generation (a passkey sign-in with a fresh-auth grant as well). This was
   an ADR-0073 defect; it is fatal for a recall, whose point is that the
   displaced party is out.
3. **A generation fence around three bearer writes**
   (`session::begin_at_generation`: lock the account row, compare the
   generation with the bearer's, else `401 key-generation-superseded`):
   `/webauthn/register/finish`, `/device/authorize/start` and
   `/account/devices/revoke`. The middleware's check and the handler's
   write are two moments; a passkey or a device grant written just after a
   recall committed would have given the displaced party a way back in.
   Not fenced, by decision: TOTP enrollment (a start that straddles leaves
   an unconfirmed secret the next enrollment overwrites, and the confirm
   needs a bearer), the passkey vault (a blob without its passkey opens
   nothing), the login handle (a displaced session can still rebind or
   delete the handle in that instant; the owner's client binds it again at
   the next landing, 0054 S5). Stated as a residual.
4. `/password/reset/finish` is one transaction under the account row's
   lock, as the contract asks, and additionally **refuses a proof made
   under an older generation** (`401 invalid credentials`): the recovery
   credential it was checked against is replaced only by a reset or a
   recall, and a password set by a stale proof must not land after one.
5. Migration `0031` adds an index on `opaque_login_states(account_id)`. The
   reset's (and now the recall's) `DELETE ... WHERE account_id = ?` scanned
   and locked every in-flight login of every account until it committed.
6. **Every read inside the capture and the deletion check is a locking
   read.** The local MariaDB (11.8) runs with `innodb_snapshot_isolation`
   on; a transaction that takes a snapshot and then deletes a row another
   transaction touched since fails with error 1020. The first build of the
   capture used plain reads and the key reset then failed with a `500`
   whenever any login anywhere was in flight. Found by the existing
   key-reset tests. A new transaction in this service must not mix plain
   reads with later deletes. The locking reads then deadlocked in turn: at
   `REPEATABLE READ` a locking read of an account's (empty) anchor range
   takes a gap lock, and two resets of accounts with neighbouring UUIDv7
   ids each held the gap and waited for the other's insert (an
   intermittent `500` in the new tests). The four transactions that change
   an account's standing (key reset, password reset, recall, account
   deletion) therefore run at `READ COMMITTED`
   (`session::read_committed_conn`); what they rely on is the account
   row's lock, not a snapshot. The full auth suite then passed 12 runs out
   of 12.

**Anchors and the capture rule (0.2).**

7. As specified, with these additions: the table has `key_generation` (the
   generation at capture; nothing reads it yet) and the counter is named
   `attempts` (see 11). An account with no password envelope captures
   nothing. An anchor for the current key that has EXPIRED and was not yet
   pruned is deleted and a new one is inserted. A new anchor's `seq` is one
   above the highest row the account still has, so after a recall has
   emptied the list numbering starts at 1 again; a `seq` is only meaningful
   together with the key that verifies under it.
8. The capture runs in `/account/keys/reset/finish` (after the row lock and
   the grant comparison, before the generation bump) and in
   `/password/reset/finish`. `rotate/finish` is T5. Two concurrent changes
   capture one anchor because both hold the account row's lock; the unique
   key on `(account_id, identity_pubkey)` is the backstop.
9. **A password change under the attacker's key gives the attacker an
   anchor too** (seq 2, their key, the password from before THEIR change,
   which is the owner's old password when they had not changed it yet).
   That is the rule working: it is above the owner's, and the owner's
   recall deletes it. It does mean `status` answers the attacker's key
   while the window is open.
10. The window is `LAKKO_RECALL_WINDOW_DAYS` (default 30, whole days, read
    with the other limits). The cap of 8 and the 20 attempts are constants.

**The routes (1.2). The wire a client follows is in the sprint report and
in the module doc of `services/auth/src/recall.rs`; the differences from
the sketch:**

11. **`start` requires an account statement by the anchor's key, and
    `start` is where an attempt is counted.** The sketch left `start`
    unauthenticated and counted wrong passwords at `finish`. With OPAQUE
    the server's answer to a login start is what lets a client test one
    password guess, locally; a client with a wrong password never calls
    `finish`. So an unauthenticated `start` was an oracle for the old
    password to anyone who knew the account id, and the 20-failure cap
    would never have been reached by anybody. Built instead: `start` takes
    `statement_b64`, a NEW statement kind 5 `recall-start` whose body is
    `anchor_seq(8 BE) ‖ BLAKE3(login_request)`; every accepted `start` is
    charged to `Scope::Recall` (keyed on account id ‖ anchor seq, default 5
    per 15 minutes) and adds one to `recall_anchors.attempts`; at 20 the
    next `start` destroys the anchor. A `start` the limiter refuses is not
    counted. Section 1.2's fake-credentials path for an unknown account is
    not built: without the key the answer is `recall-refused` before any
    OPAQUE work.
12. **The statement is checked before every counter**, at `finish` too
    (the sketch charged `Scope::Recall` on the account id first, which let
    anyone who knew the id spend the owner's five attempts every fifteen
    minutes for the whole window). A caller without the anchor key spends
    nothing: tested with a budget of one.
13. `start` answers a `session_id` and `finish` takes it (the sketch had no
    way to find the stored state). The state lives in its own table
    `recall_states`, not in `opaque_login_states` under a kind 4: no login
    route can consume one, and the nonce and the anchor have columns. Its
    lifetime is 600 s, the signup's, not the login's 120 s: between the two
    calls the client runs three Argon2id stretches.
14. `finish` consumes the state only after the statement verified, so a
    tampered finish (another new key, another upload) is refused without
    costing the attempt. A wrong password at `finish` is `401
    wrong-password` and consumes the state; it adds nothing to `attempts`,
    which were counted at `start`.
15. `finish` refuses (`400`) a `new_signature_pubkey` equal to the anchor's
    key: the old identity does not return (D10).
16. `status` also answers `attempts_remaining`. It performs exactly 8
    signature checks whatever the account has (the missing ones against a
    stand-in key), and `start` / `finish` exactly one, so the time taken
    does not say whether an anchor exists. The database reads before them
    are not padded.
17. `finish` deletes EVERY in-flight login state of the account (password
    logins included, which the reset leaves), every recall state, the TOTP
    secret and the device floors, besides what the reset deletes. Its
    answer carries a fresh-auth grant, as specified.
18. Per-address: the three routes share `Category::Attempt` with the login
    routes.
19. Account ids, statements, the nonce, public keys and device ids are
    URL-safe base64 without padding; the OPAQUE messages are standard
    base64, as on every other route of the service.

**Deletion (1.3).**

20. `DELETE /account` answers `409 {"error":"recall-window-open",
    "until_ms"}` while any unexpired anchor exists, checked under the
    account row's lock, AFTER the proof (the fresh-auth grant, or the
    passkey action grant, which is spent by then). A bare session therefore
    does not learn whether a window is open.
21. An honest person who only changed their password with the phrase also
    has an open window and cannot delete for 30 days. The text says this
    for a reset; it holds for a password change as well.

**The account statement and the FFI.**

22. `lakko-attest`: kind 5 `RecallStart` and `recall_start_body`. The
    domain-crossing tests and the fuzz helper cover it.
23. FFI, sign only, on UniFFI and wasm, appended:
    `signRecallStartStatement(accountSeed, accountId, anchorSeq,
    loginRequestB64)` and `signRecallStatement(accountSeed, accountId,
    serverNonce, newSignaturePubkey, passwordUploadB64, recoveryUploadB64)`.
    They take the OPAQUE messages as the same standard-base64 strings the
    request carries and hash them inside, because no client has BLAKE3
    otherwise. UniFFI's `AccountStatementKind` gains a last variant
    `RecallStart`. The status statement is the existing
    `signAccountStatement(seed, RecallStatus, accountId, [])`. The OPAQUE
    side needs nothing new: `OpaqueLogin.startPassword(oldPassword)`,
    `OpaqueRegister.startPassword(newPassword)`,
    `OpaqueRegister.startRecovery(newSeed)`.
24. **`just regen-bindings` was not run**: it rewrites
    `apps/android/lakko-core/.../uniffi`, and three client agents were
    building in `apps/` at the time. The Rust tests of both FFI crates
    cover the two functions. T4 starts by regenerating.

**Residuals, stated.**

- An attacker who holds the old phone AND the password recalls as well as
  the owner (1.4's total-loss row); unchanged.
- A phone finder who holds the anchor key can spend the anchor's 20
  attempts and destroy it. In the case the ADR is about, that anchor names
  the stolen phone's key and the owner does not hold it; where the owner
  DOES still hold a device with that key (two devices, one stolen, and a
  reset by a third party), the thief can end the owner's recall in about an
  hour. Not addressed.
- The fence does not cover the login handle (item 3).
- A recall statement of kind 1 can be replayed for 120 s to read `status`.
- T1 residual 14 stands and matters more now: an account whose client never
  ran the genesis mint has no recorded key, so no anchor is ever captured
  and a reset of it cannot be recalled.

**Tests** (`services/auth/src/recall_tests.rs`, through the router with the
real OPAQUE client): six concurrent password changes capture one anchor
that holds the first password byte for byte, and neither a later password
change nor a key reset alters it; no recorded key and the cap capture
nothing; the password-only takeover (reset, password change, planted TOTP,
passkey, device grant and a floor on the owner's device) is recalled and
afterwards the attacker's bearer, refresh credential, grant, both
passwords, both phrases, an in-flight login, the TOTP row and the passkey
are dead and no anchor is left to anybody; the double reset; the honest
reset followed by a second takeover, with the old phone's key useless
without the password before and after; 27 refusals across the three routes
that are byte-identical and spend nothing; the wrong password at `finish`;
19 wrong attempts and then the owner's recall; 20 and the anchor is
destroyed; the default five per window; a tampered finish and a replayed
recall; the new key must be new; the expired anchor; deletion refused with
`until_ms` and allowed after the window; the fence. One T1 test was
rewritten for item 1.

Not done in T3: fuzzing against a budget, the bindings and native
libraries, any client change, `docs/security/*` (C-number, G-46's text),
`CLAUDE.md`, `docs/PRODUCT-READINESS.md`, and the `lakko-security` pass
this sprint needs before it merges (three anonymous routes, a stored
password verifier of the past, and a changed refusal order in the session
middleware).

## T5 as built (2026-10-07)

Sprint T5 (core + server) is built: the `key-rotation` envelope with its
FFI, `POST /account/keys/rotate/{start,finish}`, the session upgrade on
`POST /auth/session/refresh`, discovery's verification of supersession
proofs and its proven-rotation arm, the DS root endorsement and the attested
root transfer, `PUT /v1/channels/{id}/owner`, and two follow-ups to the
recall (a stale statement is told the server's time; a lost `finish` answer
can be asked again). Unit-tested and DB-gated route-tested on the host
against the local MariaDB. Nothing was walked on a device, no client calls
any of it (T6), and `lakko-mls` and `schemas/` are untouched.

**One change here breaks a flow that is already built on the clients.** The
DS now refuses an attested `reset` that carries no root endorsement (item
21). The ADR-0073 key reset and the recall both end with that call, so on
all three clients they stop at the DeviceGroup re-founding until the client
sends `root_endorsement_b64`.

Every point where the build differs from sections 3.2 to 3.5 and 4.6, or
decides something they left open:

**The envelope (`lakko-contacts::key_rotation`).**

1. The wire is section 3.3's, 192 bytes. The crate does not depend on
   `lakko-transparency`, so the proof is 145 opaque bytes there and the
   decoder checks shape only. The three checks a kept device owes (the proof
   is by the account key it holds; it verifies and has not expired; the
   identity derived from the entropy has exactly the proof's new key) are one
   FFI call, `verifyKeyRotationEnvelope`, on UniFFI and wasm, so no client
   re-derives them. It also refuses a proof of the held key over itself.
2. The 14 days after which a pending envelope is dropped are a constant of
   the crate (`KEY_ROTATION_PENDING_MAX_AGE_MS`) with a helper on both FFI
   surfaces. `prepared_at_ms` ages a pending envelope and does nothing else:
   which pending envelope a device adopts is decided by the key the server
   side shows as the account's, never by arrival order or by the stamp. A
   device may hold several, keyed on the proof's new key.
3. The decoded value redacts the entropy in `Debug`, and no error carries a
   payload byte.
4. **The NSE surface does not decode it.** The contract said "decode-only on
   the NSE surface". Built instead: `isKeyRotationEnvelope(plaintext) ->
   bool` and `keyRotationEnvelopeLen()`. Every sibling arm returns what it
   decodes; this envelope carries the account's new entropy and the
   notification extension has no use for it, so the bytes do not cross into
   Swift at all.
5. The ordering rule of 3.2 (the removed device is out and the epoch has
   advanced BEFORE the envelope is sealed, and the DeviceGroup is the only
   destination) cannot be checked by the crate or by any server. It is
   stated in the module doc as the sender's obligation. T6 owes a test on
   each client that asserts the epoch of the emitted message, as C-61 has.

**`POST /account/keys/rotate/{start,finish}` (`services/auth/src/key_rotate.rs`).**

6. `start` is the reset's start under another path: grant, body,
   `Scope::RecoveryReplace`, stateless. It does not ask for the proof.
7. `finish` checks in this order: the fresh-auth grant (`403
   fresh-auth-required`); the body (`400`); the supersession proof against
   `accounts.identity_pubkey` (`403 account-key-required`); the counters
   (`429`); the transaction. The proof is checked BEFORE the counters, so a
   session that holds the password but not the account key spends none of
   the owner's rotation budget (tested with a budget of one), and AGAIN
   under the account row's lock against the key the row holds then: a reset
   or a recall that committed in between has moved the key, and a proof by
   the key before it must not rotate the account they made.
8. "The proof verifies" is `verify_supersession` (signature by the recorded
   key, a validity window of at most 30 days, not expired) and the proof's
   new key is the body's, and the new key differs from the recorded one. One
   code, `account-key-required`, for every failure, a missing recorded key
   included.
9. **D9, as built: the passkey VAULTS are deleted, the passkeys are kept.**
   A vault is the old phrase's entropy sealed under a passkey's PRF output;
   after the rotation, opening one would hand a device a seed that derives
   the superseded identity and a recovery credential the server no longer
   accepts. The server cannot re-seal it (it never sees the entropy or the
   PRF output), so "turned off" is the row's deletion: the passkey still
   signs in, `GET /vault/passkeys` no longer lists it, and the person turns
   it on again from a device that holds the new phrase.
10. Also deleted, beyond section 3.4's list: the account's in-flight WebAuthn
    states and action grants. Kept: the password, TOTP, the passkeys, **and
    the device session floors** (a recall drops them; a rotation must not: a
    device removed before the rotation stays refused).
11. `revoke_device_ids_b64`: at most 8; a malformed id, the all-zero id and
    the caller's own device are `400`. Absent means none.
12. The pair `finish` returns names the device the caller's bearer names;
    there is no `device_id_b64` (a rotation changes no device key). The
    answer carries no fresh-auth grant: the grant the rotation used was of
    the previous generation and is spent, and a second rotation needs a
    second sign-in.
13. `Scope::KeyRotate = 16`, default 3 per day, `LAKKO_AUTH_LIMIT_KEY_ROTATE`.
14. The capture rule runs as specified. Its consequence, stated: the anchor
    a rotation captures names the OLD key, which the stolen device holds.
    Without the password its holder can only spend the anchor's 20 attempts;
    with it, this is the total-loss row of 1.4.

**The session upgrade (`POST /auth/session/refresh`).**

15. As section 3.3, with `key_statement_b64` read only when the credential
    is of a lower generation. Order: the credential itself; the generation
    and the identity key in ONE read; the statement (kind 4, body
    `BLAKE3(refresh_token)`, under the recorded key); then the device floor.
    A statement that does not verify is `key-generation-superseded`, exactly
    as without the field. A device whose statement verifies and whose floor
    says "removed" is told `device-revoked`: it holds the current key, so
    the T3 rule (a superseded device is never told it was removed) is not
    in play for it. After a reset or a recall no old device can sign the
    statement (tested), so those are unchanged.
16. The upgraded pair names the same device at the current generation and
    carries no grant. The statement covers the credential, so it upgrades
    that credential and no other. Auth keeps no nonce for it: replaying a
    captured statement needs the refresh credential it covers.
17. **`401 {"error":"statement-stale","server_time_ms":T}`, a new code.** An
    account statement is good for 120 s either side of the server's clock. A
    kept device with a wrong clock would have been told
    `key-generation-superseded`, read it as a reset, and wiped. A statement
    that is right in every way but its time is answered with the server's
    time instead, and ONLY when its signature verified, so a caller without
    the key never sees the code. The device signs again with
    `signAccountStatementAt(..., T)`. The same answer is given by the three
    recall routes (item 24). It is NOT given by `POST /account/devices/revoke`
    or by discovery's delete, which still answer `account-key-required` for a
    stale statement; left as it was.

**Discovery (`services/discovery/src/identity_bundle.rs`).**

18. A supersession proof on an upload is verified. On a key CHANGE: signed
    by the key the row holds (under the row lock), for the uploaded key,
    window within the format, not expired. On a re-upload of the same key,
    and on a first publish: it must open, name the uploaded key as the new
    key, and, when the row knows its previous key, name that as the old key;
    its expiry is not judged (a stored proof expires in place, and the peer
    that reads it judges that). Anything else is `400
    {"error":"supersession-proof-invalid"}` (a new code) and nothing
    changes, an attestation beside it or not. So a session can no longer
    replace a stored proof with bytes every peer refuses.
19. The proven-rotation arm (a proof AND an attestation): the generation
    must be strictly above the row's (`409 key-generation-not-newer`); both
    are stored; the cool-down is neither applied nor started; the KT-log
    leaf is not a reset; when the upload sends no `leaf_auth`, the proof is
    stored and forwarded as the leaf authorisation. **Every KeyPackage row
    of the account is deleted, and the tombstones are KEPT**, where section
    3.5 says both are deleted. The rows go because each was published by a
    device of the old generation, and the directory should list a device
    again only when it publishes with a session of the new one, which a kept
    device gets only by showing it holds the new key. Device ids are not
    affected: the bulk delete writes no tombstone, so a kept device (and the
    rotating device, whose own row goes too) re-publishes under the same id.
    The tombstones stay because a tombstone names a device that was REMOVED,
    and dropping it would let someone who signs in afresh on that device id
    list it again with a binding under the predecessor key, which peers
    still accept in Stage A. An attested RESET still clears them.
    The proof-only arm of ADR-0016 is unchanged apart from the verification.
20. **Residual at the directory.** Until the stolen device's session is
    revoked, it holds the account key and a session, and can publish a
    proof-only key change of its own. The owner's later proven upload is
    then refused (`supersession-proof-invalid`: the row no longer holds the
    key the owner's proof was signed by). The owner's client falls back to
    the same upload WITHOUT the proof, which discovery takes as an attested
    reset at the new generation; peers then follow it as a reset, not as a
    rotation. The removal comes first in 3.2 for this reason too, and the
    window is the access lifetime after the revoke.

**The DS (`services/ds/src/key_reset.rs`, `ownership.rs`).**

21. The root endorsement (4.6, D11): `root_endorsement_b64`, required with
    `key_reset_attest_b64` and refused (`400`) without it. The signed bytes
    are `"lakko/ds/root-endorsement/v1" ‖ 0x00 ‖ journal_id(32) ‖
    key_generation(8 BE) ‖ root_key(32)`: the journal and the generation are
    bound as well as the key, where 4.6 names the signer key alone. It is
    verified with the key inside the attestation, after the attestation and
    before the statement's own signature and any nonce; absent or failing is
    the uniform `401`. `root_key` is the key that BECOMES the root: the
    signer of a `reset`, the `new_root` of a transfer. Defined in
    `lakko-attest` (`sign_root_endorsement` / `verify_root_endorsement`).
22. The attested root transfer: `PUT /ds/group/{hex}/root` takes the same
    two fields. It is heard from the current root only (the attestation adds
    the generation, it does not widen who may transfer), needs a generation
    strictly above the stored one (`409`), records it, and keeps the chain,
    the registrars and the push binding. Afterwards the attestation re-roots
    nothing on either route. A plain transfer is unchanged.
23. Serving attestations from discovery (ADR-0073 R.3) is not built; the
    endorsement is what makes it safe to build.

**The recall follow-ups (`services/auth/src/recall.rs`).**

24. `statement-stale` on `status`, `start` and `finish` (item 17's rule):
    nothing is counted and nothing is consumed by a stale request, and an
    account with no anchor answers `recall-refused` to a stale statement as
    to any other. A device with a wrong clock no longer reads "no anchor"
    and destroys its recall record.
25. **A lost `finish` answer can be asked again.** A successful `finish`
    writes, in its transaction, a row of `recall_outcomes` (migration
    `0032`): the attempt's id, BLAKE3 over every field of the request (the
    statement and the OPAQUE finalization included), the generation the
    recall set, the device id and the attestation, kept until the attempt's
    own ten minutes are over. A second `finish` naming that attempt with
    exactly the same request is answered again: the same attestation, and a
    pair and a fresh-auth grant issued at that generation. Honoured only
    while the account is still at that generation. Any other second `finish`
    for the attempt is `recall-refused`. It changes no account state. Who
    can use it: whoever holds the exact request, which is the device that
    sent it, or someone who could read the first exchange in full and so
    already had its answer. A crash between consuming the attempt and the
    commit still loses the attempt; the device opens another.

**Channels.**

26. `PUT /v1/channels/{id}/owner`: the body is the new owner key, 32 raw
    bytes; the authorisation is the EXISTING owner request proof, by the
    current owner key, over the method, the path and `BLAKE3(body)`. Section
    3.5 asked for "one more signed body in that module's shape"; the
    existing proof already binds the channel and the new key, so no new
    signed layout was added (`owner_handover_body` /
    `parse_owner_handover_body`, fuzz target `owner_handover_body`). The
    swap is a compare-and-swap against the key the proof was verified under:
    of two handovers by one key, one lands and the other is `409
    owner-changed`. A handover naming the key that already owns the channel
    is a `204` that changes nothing, which is how the new owner asks whether
    its handover landed after a lost answer. A small-order key is refused.
    The new key does not prove possession. The co-publisher appointments and
    the listing are untouched (neither is signed by the owner key).
27. **No migration.** The owner key was already a column; replacing it is
    one `UPDATE`. Both `ChannelStore` implementations gained `replace_owner`.

**FFI (UniFFI and wasm unless said).** `composeKeyRotationEnvelope`,
`decodeKeyRotationEnvelope`, `verifyKeyRotationEnvelope`,
`keyRotationEnvelopeLen`, `keyRotationPendingMaxAgeMs`,
`keyRotationPendingExpired`; a `KeyRotation` kind and a `keyRotation` field
on the dispatched envelope; `signDsRootEndorsement`, `dsRootEndorsementLen`;
`signSessionUpgradeStatement`, `signAccountStatementAt`,
`recallStartStatementBody`, `recallStatementBody`,
`sessionUpgradeStatementBody`; one appended `FfiError::KeyRotation`. The NSE
surface: `isKeyRotationEnvelope`, `keyRotationEnvelopeLen`. The supersession
proof is signed with the existing `signSupersessionProof`, and the channel
handover with the existing `ChannelOwnerSigner` (the old seed's signer signs;
the body is the new seed's signer's `verifyingKey()`).

**Not built in T5, and why.**

- The peers' proven-predecessor rule, the adoption on kept devices, the
  rotation order and the re-keying of roots, mailbox, reach, channels and
  backup are client work (T6). Nothing in T5 needed a `lakko-mls` change and
  none was made.
- The reach-key and mailbox moves of 3.5 use routes that exist; nothing was
  added for them.
- `services/transparency` is unchanged: its append already verifies a proof
  against the previous entry.

**Residuals, stated.**

- Item 14 (the rotation's anchor names the stolen device's key).
- Item 20 (a proof-only change by the stolen device before it is revoked).
- Section 3.1's: after Stage A the stolen seed still mints bindings that
  existing conversations accept. T5 changes nothing about that.
- A kept device that never reads the envelope is superseded like any old
  device (3.3); unchanged.
- An account statement of kind 4 can be replayed for 120 s by someone who
  also holds the refresh credential it covers.

**Tests.** `lakko-contacts`: the envelope's layout, every other length, the
version before the length, the redaction, sibling isolation both ways, the
dispatcher. `lakko-attest`: the stale helper; the endorsement (what it
names, a weak attested key, the pinned bytes). Auth, through the router with
the real OPAQUE client (`key_rotate_tests.rs`): a password-only session
cannot rotate with any proof and spends nothing; the grant and a recorded
key are required; the whole rotation (generation, key, attestation, what is
kept and deleted, the anchor, the recovery credential, every old bearer
superseded), the kept device's upgrade against every statement that must not
work, the wrong clock told the time to the key's holder only, the stolen
device refused; a reset ends what the old key could do; the default budget;
the stale statement on the three recall routes; the retried finish, each
field of it changed in turn, the account moved on, the outcome expired; a
failed finish records none. One T3 test was rewritten for item 25, and
three cases of the T3 refusal test (stale and future-dated statements) are
now signed by a stranger's key, since by the anchor's key they are told the
time.
Discovery: the change rule; a proven rotation end to end (generation, old
sessions gated, rows gone, a kept device back under its id, the tombstone
kept, no cool-down, a retry, a replay); eight forged or wrong proofs with
and without an attestation; a re-upload cannot swap the stored proof. Two
existing tests that used opaque bytes as proofs now use real ones. DS: an
attested reset without, with a stranger's, with a copied, with another
generation's and with a malformed endorsement; the attested transfer. Three
existing helpers now send the endorsement. Channels: the handover against
both stores and through the router.

Not done in T5: fuzzing against a budget (the three new targets compile;
none was run), a rebuild of the native libraries and the wasm package, any
client change, `docs/security/*` (the ordering rule of item 5 has no
C-number; G-46's text), `CLAUDE.md`, `docs/PRODUCT-READINESS.md`, and the
`lakko-security` pass this sprint needs before it merges (a route that
moves the account's identity key, a route that exchanges a superseded
credential for a current one, and two verifiers that now parse a proof).

## T6 as built (2026-10-08): the clients, and where they disagree

Built on web, iOS and Android against the T5 server; unit-tested only (web vitest 4315, iOS
`LakkoTests` 2976 with 0 failures and the core rebuilt, Android 3370 on the host JVM without the
native core, whose `.so` was not rebuilt). Nothing was walked on a device or against a server.
**Stage A is not safe to walk end to end yet: the mailbox rule of part 3.5 is undecided, and web
differs from iOS and Android on reach keys after a rotation, listed below.**

Built on all three:

- **The breaking T5 change.** The attested DeviceGroup reset carries `root_endorsement_b64` on the
  key reset, the recall and the owed-publish retry.
- **`statement-stale`.** Recall status / start / finish and the session upgrade re-sign once at the
  server's time; a stale answer never destroys the recall record.
- **`recall/finish` retry.** The byte-identical request is resent on a lost answer. The pending
  request is held in memory only (web: 9 minutes; Android: until the process dies), so a reload or
  a process death loses it.
- **The rotating device.** Remove sheet with the reason pair; one owed, resumable operation in the
  order of part 3.2; the envelope is sealed only after the removal won the journal, behind one
  named seam with a test on the emitted message's epoch and an inverted-order companion (web and
  iOS through the real core). A lost `rotate/finish` answer is settled by the session upgrade only
  the new key can sign; without the attestation in hand the directory gets the proof alone and the
  DS a plain root transfer. `400 supersession-proof-invalid` falls back to the attestation alone.
- **A kept device.** The `KeyRotation` envelope is honoured only in the account's own DeviceGroup,
  verified, stored pending under the device key and dropped after 14 days; adoption on the
  published key or a superseded refresh; superseded with nothing pending reads the DeviceGroup
  journal to its head once before the T4 staged wipe; `device-revoked` is the full wipe; no answer
  retries and never wipes. Adopting swaps seed and phrase, keeps all local data and stores the old
  key as predecessor first. The three background filters suppress the envelope.
- **Proven predecessor.** The ADR-0016 gate re-pins silently on a valid proof and records the old
  key as a proven predecessor; the ADR-0073 phase-2 flag is set only by a refusal the log calls a
  reset, so a proven rotation shows contacts nothing. The device's own conversation binding stays
  under the predecessor key, and the predecessor's mailbox is read for Welcomes.

Where the clients differ (to be aligned before any walk):

| | web | iOS | Android |
|---|---|---|---|
| Owned group journals re-rooted | yes (ADR-0057 takeover) | yes | yes (aligned with iOS 2026-10-11) |
| Channel owner handed over | yes | yes (channels this device created) | yes (every channel it holds a publisher seed for; a co-published one ends as not-owner) |
| Reach keys after the rotation | restarted under the new key; grants name the new mailbox, which peers do not address | `rekey` under the new key; cannot write to the own old mailbox | re-keyed like iOS, with the same limit |
| Backup re-sealed | marked dirty | re-sealed and awaited since 2026-10-11 (an upload failure was swallowed before) | re-sealed and awaited; a failure keeps the step owed |

Android first answered "done" for the journal, channel, reach and backup steps without doing them
(a rotation reported success while the old seed still rooted the account's groups and owned its
channels); since 2026-10-11 each is a real step of the owed record that stays owed and is retried
on failure. On iOS and Android the journal step is recorded once the takeovers are durably
scheduled, not once they land.

Not built on any client:

- **A device paired after a rotation** bound under the new key and read as another account to the
  groups (closed on 2026-10-11 by state bundle format 2, see "Third pass").
- **The mailbox rule (decided 2026-10-11 as 3.5a; see "3.5a as built").** As first built: Leaves still name the old account key, so Welcomes go to the
  old mailbox for all of Stage A; nothing registers the new reach keys there or maps predecessor
  to current on the sender side. This is the open design decision.
- **A peer away longer than the proof's 14 days** is refused (no reset flag): discovery does not
  serve the attestation, so its `signed_at` cannot stand in for the clock.
- PublisherGroup journal roots are not transferred. The predecessor mapping is wired into names
  only; a contact first added after their own rotation is not mapped to their old-key leaves.

## 3.5a as built (2026-10-11): the mailbox rule on the three clients

Built on web, iOS and Android; unit-tested only (web vitest 4340, iOS `LakkoTests` 2997 with 0
failures, Android 3391 on the host JVM without the native core). Nothing was walked on a device or
against a server; no server or core change was needed. The same day the iOS rotation's backup step
stopped reporting success on a failed or empty upload, and web's reach step, which answered done
without registering anything, became a real owed step.

All three: every sender site resolves an account key through the proven-predecessor resolver before
deriving a mailbox (Welcomes incl. the sweep and the ADR-0040 replay, contact acks, committer
delegations, people search, channel member cards, "another device of my own account"); a received
grant is filed and found per person; the rotating device has an owed `profile` step after the reach
re-key; predecessor mailboxes are polled for 30 days after adoption and nothing is registered there.

Differences and gaps:

- **A contact request to a proven-predecessor key**: iOS re-fetches the current bundle and sends;
  web and Android refuse the send (the request is sealed to the X25519 key in hand).
- **A kept device's profile re-send**: durable on Android (a marker retried at each session start),
  the throttled effect on web, one best-effort attempt on iOS.
- **Predecessor mailboxes carry Welcomes only.** A contact request or ack that reaches the old
  mailbox inside the 30 days is not acted on (Android states why: the pending list dedupes by DS
  message id across mailboxes).
- **Web knows only the oldest own predecessor** in a session; after a second rotation the
  intermediate key's mailbox is not polled.
- **Proposer and committer may disagree** about a person's (or the committer's) current key until
  both observed the proof; the delegation then names the other mailbox and the Welcome stays owed.

### Second pass (2026-10-11): rules 5 and 6, and one behaviour on the three clients

Unit-tested only (web vitest 4381, iOS `LakkoTests` 3030 with 0 failures, Android 3422 on the host
JVM without the native core); nothing walked on a device or against a server. It supersedes the
"Differences and gaps" list above where the two disagree.

Now the same on all three: a contact request to a proven-predecessor key re-fetches the bundle
through the gate and is sealed to the current keys; a kept device's profile re-send is durable; own
predecessors are a chain, each mailbox with its own cursor and 30-day window; a refused write
re-checks the person's bundle once and retries once, with the displaced grant kept beside the newer
one until the address is known current; people search accepts a proven predecessor; a committer
finds a delegation naming any of its own keys; a predecessor mailbox delivers Welcomes only (web and
iOS had been taking contact requests from it); the rotating device revokes its `/i/` links under the
predecessor key, drops them locally, re-sends its unanswered contact requests from the new key as an
owed step and shows `devices.rotate.effect_links`; an incoming request from the proven successor of
a pending request's key replaces that row.

Still different, or not built:

- **`/c/` call links**: iOS and Android record the ledger id from this build on and revoke those;
  links minted earlier, and every web call link (web keeps no id), are spent but stay in the ledger.
- **The spend round on a kept device**: iOS runs it there too (over that device's own links and
  requests); web and Android run it on the rotating device only.
- **Sent requests**: re-sent without the intro text on web and iOS (never stored); Android's sent
  list is in memory, so a process death before the step leaves nothing to re-send and the step
  completes.
- **The address re-check needs the person's account id**: a group member who is not a contact is not
  re-checked on web, and only re-resolved from recorded links on Android. The re-check is throttled
  per person (iOS 5 minutes, Android 10; chosen, not measured).
- **iOS `ContactsClient.proveRequesterRotations`** records a proven edge for a requester this device
  never pinned, from the bundle's supersession proof verified against the pending request's key:
  the first write to the proven-predecessor store outside the ADR-0016 gate. Same evidence, another
  path; to be looked at in the security pass.
- A delegation naming the added person's old mailbox still leaves the Welcome owed until both sides
  observed the change. Spent `/g/` join reply scopes are dropped, not re-knocked (iOS).

### Third pass (2026-10-11): the remaining differences, and a device paired after a rotation

Unit-tested only (web vitest 4444, iOS `LakkoTests` 3049 with 0 failures, Android 3450 on the host
JVM without the native core; one unrelated Android test failed once on a leaked exception and passed
alone and on the re-run, unexplained). Nothing walked on a device or against a server. It supersedes
the "Still different" list of the second pass where the two disagree.

Now the same on all three: the spend round also runs, durably, on a kept device over that device's
own links and requests; `/c/` call links minted from this build on are recorded and revoked (web
signs and revokes them with the device identity, not the account key); unanswered sent requests are
stored under the device key with their intro and re-sent with it; the address re-check is throttled
to once per person per 5 minutes.

**A device paired after a rotation.** The ADR-0050 pairing state bundle is format **2** (any other
`v` refused): `ownPredecessors`, oldest first, at most 8, each `{seedB64 (64-byte phrase seed),
adoptedAtMs}`, and on each pin `provenPredecessorsB64` (32-byte keys, oldest first, at most 8). The
receiver stores the own predecessors where a kept device holds them (device-key protected, in the
wipe), counts each predecessor mailbox's 30 days from `adoptedAtMs`, mints its conversation binding
under the oldest own predecessor, and records a contact's predecessors as proven only when the pin
it ends up holding is the carried one (a second writer to the proven-predecessor store outside the
ADR-0016 gate, like iOS `proveRequesterRotations`).

A device that was already signed in when it paired had minted under the current key. Two rules
close that on all three: a sender does not admit an own-account device from a KeyPackage bound to
another own key than its own leaves in that conversation use (the stale row still counts as listed,
so no leaf is retired), and the receiver re-mints and re-publishes under the conversation key when
the bundle arrives, but only while it holds no conversation.

Left as it is:

- The bundle is best effort (a 20 s poll). A device it never reaches stays bound under the current
  key and out of the account's older chats until it is paired again while holding no conversation;
  once it holds conversations it keeps the current-key binding for good.
- Web's live re-mint starts a fresh identity object: a Welcome built on the stale KeyPackage by a
  sender without the guard cannot be opened, and pending state held only in the old provider (a
  `/g/` join request) is lost.
- iOS accepts an integer time written in exponent form (`1e3`); web and Android refuse it. No
  client writes that form.
- Web's sent-request recipient list stays in plaintext `localStorage` (only the intro is sealed); a
  request sent by accepting an `/i/` link was never in the sent list and is not re-sent. No client
  has a withdraw action for a sent request.
- Web fixed on the way: the restore prompt without a device key dropped the state bundle and the
  joined DeviceGroup leaf.

Consequences of the rule as first built (1 decided as rule 5, 2 and 3 closed by the second pass):

1. **Everything granted before a rotation stops opening the mailbox once the sender has observed
   the change**: the rotating account's old `/i/` and `/c/` links and the `reply` grant of its
   unanswered contact requests name the old mailbox. An accept is addressed to the new mailbox with
   the old grant and is refused; no shared conversation exists through which a new grant could
   arrive.
2. **A peer that receives the re-sent profile before it observes the proven key** holds the new
   grant and the old address; its single-shot writes fail until its next identity fetch (queued
   sweep Welcomes retry by themselves).
3. **People search refuses a hit whose key is a proven predecessor** of the fetched bundle's key
   (iOS; not checked on the others).

**Alignment pass 2026-10-12 (see ADR-0073 R.13b).** Every add path on the three clients now filters
another person's devices by KeyPackage binding (the expected key or its pinned proven chain). One
consequence is open: **a person who rotated before this device ever saw them cannot be added or
written to until their devices are re-bound to the new key**, since nothing this device holds links
the served key to the key the devices are bound to, and a supersession proof alone is not accepted
at first sight. Closing it needs the predecessor anchored in the KT log at first sight, or the new
key's signature over the old binding. Decided 2026-10-12: the refusal stays for now and is
revisited after it has been tried in use.

## Not verified

- Outside sprint T1 nothing here was built or run; every line number is
  from a read on 2026-10-07.
- Whether each client creates its device identity before its first sign-in
  (0.4's `device_id_b64`), or needs `bind-device`.
- Whether a removed web client's reload actually re-creates its directory
  row end to end, and the same on the natives (fact 9).
- Whether chat polling stops while the removed screen is up.
- Whether any client auto-approves a join request from an account already
  in the group.
- Whether any client compares a fetched KeyPackage's binding with the pin,
  and how each client keys its stored reach grants (3.5).
- The Android re-pin line for a proven change.
- Whether the reset's client order mints the genesis attestation after
  `reset/finish` on all three (0.1 requires it).
- The backup's behaviour after a reset, beyond ADR-0073's own section.
- That OpenMLS accepts the Stage B owner sequence as described; it uses
  existing calls but was not run.

## References

- [ADR-0014](0014-webauthn-action-binding.md),
  [ADR-0015](0015-device-removal-authorization.md),
  [ADR-0016](0016-identity-supersession-transport.md),
  [ADR-0017](0017-phrase-seeded-identity.md),
  [ADR-0036](0036-auth-attested-genesis.md),
  [ADR-0050](0050-per-device-conversation-leaves.md),
  [ADR-0053](0053-ds-journal-authorisation.md),
  [ADR-0057](0057-journal-ownership.md),
  [ADR-0059](0059-channel-discover.md),
  [ADR-0060](0060-scoped-discovery.md),
  [ADR-0072](0072-recovery-model-simplification.md),
  [ADR-0073](0073-account-key-reset.md)
- [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md) sections 15
  to 18; [`docs/ux/second-device.md`](../ux/second-device.md) section 3
- [`docs/PRODUCT-READINESS.md`](../PRODUCT-READINESS.md), the row "Log out
  / remove device revokes the server session"
